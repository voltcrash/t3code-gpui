//! Transition policy for thread notifications. Snapshots and replay establish a baseline.
use std::collections::{HashMap, HashSet};

use crate::shell::{ShellState, ThreadShell, ThreadStatus, timestamp_ms};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub thread_id: String,
    pub thread_title: String,
    pub title: &'static str,
}

#[derive(Default)]
pub struct Tracker {
    previous: HashMap<String, State>,
}

struct State {
    attention: Option<(Option<String>, ThreadStatus)>,
    completion: Option<i64>,
}

impl Tracker {
    pub fn update(&mut self, shell: &ShellState, live: bool) -> Vec<Notice> {
        let requests = child_requests(shell);
        let mut next = HashMap::new();
        let mut notices = Vec::new();
        for thread in shell.threads.values().filter(|t| !t.is_subagent()) {
            let prior = self.previous.get(&thread.id);
            let mut status = thread.status();
            if let Some(child) = requests.get(&thread.id)
                && status != ThreadStatus::Approval
            {
                status = *child;
            }
            if status == ThreadStatus::Ready && thread.latest_run_status() == Some("failed") {
                status = ThreadStatus::Failed;
            }
            let attention = matches!(
                status,
                ThreadStatus::Input
                    | ThreadStatus::Approval
                    | ThreadStatus::Failed
                    | ThreadStatus::Limited
            )
            .then(|| (thread.latest_run_id.clone(), status));
            let completion = if status == ThreadStatus::Ready
                && thread.latest_run_status() == Some("completed")
            {
                timestamp_ms(thread.latest_run_completed_at())
                    .or_else(|| prior.and_then(|p| p.completion))
            } else {
                prior.and_then(|p| p.completion)
            };
            if live
                && thread.archived_at.is_none()
                && let Some(prior) = prior
            {
                let title = if attention.is_some() && attention != prior.attention {
                    Some(match status {
                        ThreadStatus::Approval => "Approval needed",
                        ThreadStatus::Limited => "Usage limit reached",
                        ThreadStatus::Failed => "Thread failed",
                        _ => "Input needed",
                    })
                } else if !thread.latest_run_stop_requested
                    && completion.is_some_and(|c| prior.completion.is_none_or(|p| c > p))
                {
                    Some("Thread completed")
                } else {
                    None
                };
                if let Some(title) = title {
                    notices.push(Notice {
                        thread_id: thread.id.clone(),
                        thread_title: thread.title.clone(),
                        title,
                    });
                }
            }
            next.insert(
                thread.id.clone(),
                State {
                    attention,
                    completion,
                },
            );
        }
        self.previous = next;
        notices
    }
}

fn child_requests(shell: &ShellState) -> HashMap<String, ThreadStatus> {
    let mut requests = HashMap::new();
    for child in shell
        .threads
        .values()
        .filter(|t| t.is_subagent() && t.creation_source.as_deref() != Some("provider"))
    {
        let Some(request) = &child.pending_runtime_request else {
            continue;
        };
        let live = child.activity_run_status.as_ref().map_or_else(
            || {
                matches!(
                    child.status.as_str(),
                    "preparing" | "starting" | "running" | "waiting"
                )
            },
            |status| status.is_some(),
        );
        if !live || request.kind == "auth_refresh" {
            continue;
        }
        let status = if request.kind == "user_input" {
            ThreadStatus::Input
        } else {
            ThreadStatus::Approval
        };
        let mut current: &ThreadShell = child;
        let mut seen = HashSet::from([child.id.as_str()]);
        while current.is_subagent() {
            let Some(parent) = current.lineage.parent_thread_id.as_deref() else {
                break;
            };
            if !seen.insert(parent) {
                break;
            }
            let entry = requests.entry(parent.to_owned()).or_insert(status);
            if status == ThreadStatus::Approval {
                *entry = status;
            }
            let Some(next) = shell.threads.get(parent) else {
                break;
            };
            current = next;
        }
    }
    requests
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn thread(id: &str, extra: Value) -> ThreadShell {
        let mut value = json!({"id":id,"projectId":"p","title":id,"status":"running", "latestRunId":"r", "createdAt":"2026-01-01T00:00:00Z", "updatedAt":"2026-01-01T00:00:00Z"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }
    fn shell(threads: Vec<ThreadShell>) -> ShellState {
        ShellState {
            threads: threads.into_iter().map(|t| (t.id.clone(), t)).collect(),
            ..Default::default()
        }
    }
    #[test]
    fn baselines_and_deduplicates_attention_and_completion() {
        let mut tracker = Tracker::default();
        let running = || thread("a", json!({}));
        let approval = || thread("a", json!({"pendingRuntimeRequest":{"kind":"command"}}));
        assert!(tracker.update(&shell(vec![approval()]), false).is_empty());
        assert!(tracker.update(&shell(vec![approval()]), true).is_empty());
        tracker.update(&shell(vec![running()]), true);
        assert_eq!(
            tracker.update(&shell(vec![approval()]), true)[0].title,
            "Approval needed"
        );
        assert!(tracker.update(&shell(vec![approval()]), true).is_empty());
        let complete = || {
            thread(
                "a",
                json!({"status":"completed","latestRunCompletedAt":"2026-01-01T01:00:00Z"}),
            )
        };
        assert_eq!(
            tracker.update(&shell(vec![complete()]), true)[0].title,
            "Thread completed"
        );
        assert!(tracker.update(&shell(vec![complete()]), true).is_empty());
        tracker.update(&shell(vec![running()]), false);
        assert!(tracker.update(&shell(vec![complete()]), false).is_empty());
        assert!(tracker.update(&shell(vec![complete()]), true).is_empty());
    }
    #[test]
    fn ignores_new_archived_and_stopped_threads_and_reports_child_requests_on_parent() {
        let mut tracker = Tracker::default();
        let parent = || thread("p", json!({}));
        tracker.update(&shell(vec![parent()]), false);
        let child = thread(
            "c",
            json!({"lineage":{"relationshipToParent":"subagent","parentThreadId":"p"},"pendingRuntimeRequest":{"kind":"user_input"}}),
        );
        let notices = tracker.update(&shell(vec![parent(), child]), true);
        assert_eq!(notices.len(), 1);
        assert_eq!(
            (&*notices[0].thread_id, notices[0].title),
            ("p", "Input needed")
        );
        let stopped = thread(
            "p",
            json!({"status":"completed","latestRunStopRequested":true,"latestRunCompletedAt":"2026-01-01T01:00:00Z"}),
        );
        let new = thread("new", json!({"status":"failed"}));
        assert!(tracker.update(&shell(vec![stopped, new]), true).is_empty());
        let archived = thread(
            "p",
            json!({"status":"failed","archivedAt":"2026-01-01T00:00:00Z"}),
        );
        assert!(tracker.update(&shell(vec![archived]), true).is_empty());
    }

    #[test]
    fn attention_types_follow_runtime_requests_and_new_runs_can_notify_again() {
        for (extra, title) in [
            (
                json!({"pendingRuntimeRequest":{"kind":"user_input"}}),
                "Input needed",
            ),
            (
                json!({"pendingRuntimeRequest":{"kind":"command"}}),
                "Approval needed",
            ),
            (json!({"status":"failed"}), "Thread failed"),
            (
                json!({"status":"failed","lastErrorClass":"usage_limit"}),
                "Usage limit reached",
            ),
        ] {
            let mut tracker = Tracker::default();
            tracker.update(&shell(vec![thread("a", json!({}))]), false);
            let mut pending = thread("a", extra);
            assert_eq!(
                tracker.update(&shell(vec![pending.clone()]), true)[0].title,
                title
            );
            assert!(
                tracker
                    .update(&shell(vec![pending.clone()]), true)
                    .is_empty()
            );
            pending.latest_run_id = Some("next-run".into());
            assert_eq!(tracker.update(&shell(vec![pending]), true)[0].title, title);
        }
        for request in [
            json!({"kind":"auth_refresh"}),
            json!({"kind":"user_input","blocking":false}),
        ] {
            let mut tracker = Tracker::default();
            tracker.update(&shell(vec![thread("a", json!({}))]), false);
            assert!(
                tracker
                    .update(
                        &shell(vec![thread("a", json!({"pendingRuntimeRequest": request}))]),
                        true
                    )
                    .is_empty()
            );
        }
    }

    #[test]
    fn replay_and_removal_reset_the_baseline_without_emitting_old_alerts() {
        let mut tracker = Tracker::default();
        let running = || thread("a", json!({}));
        let completed = || {
            thread(
                "a",
                json!({"status":"completed","latestRunCompletedAt":"2026-01-01T01:00:00Z"}),
            )
        };
        tracker.update(&shell(vec![running()]), false);
        assert!(tracker.update(&shell(vec![completed()]), false).is_empty());
        assert!(tracker.update(&shell(vec![completed()]), true).is_empty());
        tracker.update(&shell(vec![]), true);
        assert!(tracker.update(&shell(vec![completed()]), true).is_empty());
        tracker.update(&shell(vec![running()]), true);
        // Replaying an already-seen completion must not turn an unrelated update into an alert.
        assert!(tracker.update(&shell(vec![completed()]), true).is_empty());
        let newer = thread(
            "a",
            json!({"status":"completed","latestRunId":"r2","latestRunCompletedAt":"2026-01-01T02:00:00Z"}),
        );
        assert_eq!(
            tracker.update(&shell(vec![newer]), true)[0].title,
            "Thread completed"
        );
    }
}
