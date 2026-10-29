//! Read watermarks use server activity timestamps, never the desktop's clock.

use std::time::Duration;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::shell::{ThreadShell, timestamp_ms};

const THROTTLE: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct State {
    dispatched: Option<String>,
    last_dispatch: Option<Duration>,
}

impl State {
    pub fn delay(&self, thread: &ThreadShell, now: Duration) -> Option<Duration> {
        let updated = timestamp_ms(Some(&thread.updated_at))?;
        let visited = timestamp_ms(thread.last_visited_at.as_deref());
        if visited.is_some_and(|visited| visited >= updated)
            || self.dispatched.as_deref() == Some(&thread.updated_at)
        {
            return None;
        }
        let unseen_completion = timestamp_ms(thread.latest_run_completed_at())
            .is_some_and(|completed| visited.is_none_or(|visited| completed > visited));
        Some(if unseen_completion {
            Duration::ZERO
        } else {
            self.last_dispatch.map_or(Duration::ZERO, |last| {
                THROTTLE.saturating_sub(now.saturating_sub(last))
            })
        })
    }

    pub fn dispatched(&mut self, updated_at: String, now: Duration) {
        self.dispatched = Some(updated_at);
        self.last_dispatch = Some(now);
    }

    pub fn failed(&mut self) {
        self.dispatched = None;
    }
}

pub fn command(thread_id: &str, visited_at: &str) -> Value {
    json!({
        "type": "thread.visit",
        "commandId": Uuid::new_v4().to_string(),
        "threadId": thread_id,
        "visitedAt": visited_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(updated: &str, visited: Option<&str>, completed: Option<&str>) -> ThreadShell {
        serde_json::from_value(json!({
            "id":"t", "projectId":"p", "title":"Test", "status":"idle",
            "createdAt":"2026-01-01T00:00:00Z", "updatedAt":updated,
            "lastVisitedAt":visited, "latestRunId":"run", "latestRunCompletedAt":completed,
        }))
        .unwrap()
    }

    #[test]
    fn opening_a_thread_visits_its_activity_without_using_the_local_wall_clock() {
        let updated = "2026-01-01T12:00:00Z";
        let thread = thread(updated, None, None);
        let mut state = State::default();
        assert_eq!(state.delay(&thread, Duration::ZERO), Some(Duration::ZERO));
        let command = command(&thread.id, &thread.updated_at);
        assert_eq!(command["type"], "thread.visit");
        assert_eq!(command["visitedAt"], updated);
        state.dispatched(updated.into(), Duration::ZERO);
        assert_eq!(state.delay(&thread, Duration::from_secs(20)), None);
        // A server echo or marking the open thread unread must not repeat a visit.
        assert_eq!(state.delay(&thread, Duration::from_secs(30)), None);
    }

    #[test]
    fn streaming_is_throttled_but_a_new_completion_is_visited_immediately() {
        let mut state = State::default();
        state.dispatched("2026-01-01T12:00:00Z".into(), Duration::ZERO);
        let streaming = thread("2026-01-01T12:00:01Z", Some("2026-01-01T12:00:00Z"), None);
        assert_eq!(
            state.delay(&streaming, Duration::from_secs(2)),
            Some(Duration::from_secs(8))
        );
        assert_eq!(
            state.delay(&streaming, Duration::from_secs(10)),
            Some(Duration::ZERO)
        );
        let completed = thread(
            "2026-01-01T12:00:02Z",
            Some("2026-01-01T12:00:00Z"),
            Some("2026-01-01T12:00:02Z"),
        );
        assert_eq!(
            state.delay(&completed, Duration::from_secs(2)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn server_watermarks_invalid_timestamps_and_failed_requests_are_safe() {
        let mut state = State::default();
        assert_eq!(
            state.delay(&thread("bad", None, None), Duration::ZERO),
            None
        );
        let seen = thread("2026-01-01T12:00:00Z", Some("2026-01-01T13:00:00Z"), None);
        assert_eq!(state.delay(&seen, Duration::ZERO), None);
        let unseen = thread("2026-01-01T12:00:00Z", None, None);
        state.dispatched(unseen.updated_at.clone(), Duration::ZERO);
        state.failed();
        assert_eq!(
            state.delay(&unseen, Duration::from_secs(10)),
            Some(Duration::ZERO)
        );
        // Reopening creates a fresh state, so a manual mark-unread is consumed then.
        assert_eq!(
            State::default().delay(&unseen, Duration::ZERO),
            Some(Duration::ZERO)
        );
    }
}
