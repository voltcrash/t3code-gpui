//! Composer task presentation follows deriveActivePlanState/selectComposerTasksPlan.
use crate::thread::ThreadProjection;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: String,
    pub text: String,
    pub status: String,
    pub duration_ms: Option<u64>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Tasks {
    pub id: String,
    pub steps: Vec<Step>,
    pub live: bool,
}
impl Tasks {
    pub fn completed(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.status == "completed")
            .count()
    }
    pub fn current(&self) -> &str {
        self.steps
            .iter()
            .find(|step| step.status == "running")
            .or_else(|| self.steps.iter().find(|step| step.status == "pending"))
            .or_else(|| self.steps.last())
            .map_or("", |step| &step.text)
    }
}

pub fn select(
    projection: &ThreadProjection,
    active_run_id: Option<&str>,
    latest_run_id: Option<&str>,
) -> Option<Tasks> {
    // Waiting runs still own activity when a newer turn is queued. The shell's
    // presented latest ID accounts for held queues and usage limits otherwise.
    let activity = projection
        .runs
        .iter()
        .filter(|run| {
            matches!(
                run.status.as_str(),
                "preparing" | "starting" | "running" | "waiting"
            )
        })
        .max_by_key(|run| run.ordinal.unwrap_or(0))
        .map(|run| run.id.as_str())
        .or(active_run_id)
        .or(latest_run_id);
    let plans: Vec<_> = projection
        .plans
        .iter()
        .filter(|plan| plan.kind == "todo_list" && plan.status.as_deref() != Some("superseded"))
        .collect();
    let ordinals: HashMap<_, _> = projection
        .runs
        .iter()
        .map(|run| (run.id.as_str(), run.ordinal.unwrap_or(0)))
        .collect();
    let plan = activity
        .and_then(|id| {
            plans
                .iter()
                .rev()
                .find(|plan| plan.run_id.as_deref() == Some(id))
                .copied()
        })
        .or_else(|| {
            plans.into_iter().max_by_key(|plan| {
                plan.run_id
                    .as_deref()
                    .and_then(|id| ordinals.get(id).copied())
                    .unwrap_or(0)
            })
        })?;
    if plan.steps.is_empty() {
        return None;
    }
    let run = projection
        .runs
        .iter()
        .find(|run| Some(&run.id) == plan.run_id.as_ref());
    let live = plan.run_id.is_some()
        && plan.run_id.as_deref() == activity
        && (active_run_id == activity
            || run.is_some_and(|run| {
                matches!(
                    run.status.as_str(),
                    "preparing" | "queued" | "starting" | "running" | "waiting"
                )
            }));
    if !live {
        if plan.steps.iter().all(|step| step.status == "completed") {
            return None;
        }
        // Claude's unfinished tasks belong to the provider session, not just a turn.
        let provider_thread = run?.provider_thread_id.as_deref()?;
        if !projection
            .provider_threads
            .iter()
            .any(|thread| thread.id == provider_thread && thread.driver == "claudeAgent")
        {
            return None;
        }
        let latest_started = projection
            .runs
            .iter()
            .filter(|run| run.started_at.is_some() && run.provider_thread_id.is_some())
            .max_by_key(|run| run.ordinal.unwrap_or(0))?;
        if latest_started.provider_thread_id.as_deref() != Some(provider_thread) {
            return None;
        }
    }
    Some(Tasks {
        id: plan.id.clone(),
        steps: plan.steps.clone(),
        live,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn projection(driver: &str) -> ThreadProjection {
        serde_json::from_value(json!({"thread":{},"runs":[{"id":"r","status":"completed","ordinal":1,"providerThreadId":"session","startedAt":"2026-01-01"}],"plans":[{"id":"tasks","kind":"todo_list","status":"active","runId":"r","steps":[{"id":"one","text":"Inspect","status":"completed","durationMs":1234},{"id":"two","text":"Implement","status":"running"},{"id":"three","text":"Verify","status":"pending"}]}],"providerThreads":[{"id":"session","driver":driver}],"attempts":[],"runtimeRequests":[],"turnItems":[],"visibleTurnItems":[]})).unwrap()
    }
    #[test]
    fn active_tasks_show_progress_and_completed_lists_until_the_run_settles() {
        let mut p = projection("codex");
        let tasks = select(&p, Some("r"), Some("r")).unwrap();
        assert!(tasks.live);
        assert_eq!(tasks.completed(), 1);
        assert_eq!(tasks.current(), "Implement");
        assert_eq!(tasks.steps[0].duration_ms, Some(1234));
        for step in &mut p.plans[0].steps {
            step.status = "completed".into();
        }
        assert_eq!(select(&p, Some("r"), Some("r")).unwrap().completed(), 3);
        assert!(select(&p, None, Some("r")).is_none());
    }
    #[test]
    fn unfinished_claude_tasks_follow_the_same_started_provider_session() {
        let mut p = projection("claudeAgent");
        assert!(!select(&p, None, Some("r")).unwrap().live);
        p.runs.push(serde_json::from_value(json!({"id":"next","status":"running","ordinal":2,"providerThreadId":"session","startedAt":"later"})).unwrap());
        assert!(select(&p, Some("next"), Some("next")).is_some());
        p.runs[1].provider_thread_id = Some("other".into());
        assert!(select(&p, Some("next"), Some("next")).is_none());
        p.runs[1].started_at = None;
        assert!(select(&p, Some("next"), Some("next")).is_some());
        assert!(select(&projection("codex"), None, Some("r")).is_none());
    }
    #[test]
    fn plan_selection_prefers_the_activity_run_then_run_ordinals_and_skips_superseded() {
        let mut p = projection("claudeAgent");
        let mut newer = p.plans[0].clone();
        newer.id = "newer".into();
        newer.run_id = Some("next".into());
        p.plans.insert(0, newer);
        p.runs.push(
            serde_json::from_value(json!({"id":"next","status":"queued","ordinal":2})).unwrap(),
        );
        assert_eq!(select(&p, Some("r"), Some("next")).unwrap().id, "tasks");
        assert_eq!(select(&p, None, Some("next")).unwrap().id, "newer");
        p.runs[0].status = "waiting".into();
        assert_eq!(select(&p, None, Some("next")).unwrap().id, "tasks");
        p.runs[0].status = "completed".into();
        p.plans[0].status = Some("superseded".into());
        assert_eq!(select(&p, None, Some("next")).unwrap().id, "tasks");
        p.plans[1].steps.clear();
        assert!(select(&p, Some("r"), Some("r")).is_none());
    }
}
