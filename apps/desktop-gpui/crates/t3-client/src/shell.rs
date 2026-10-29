//! The environment's shell: projects and thread summaries streamed by
//! `orchestration.subscribeShell`, plus the rules that place and order them
//! in the sidebar.
//!
//! The rules are ports of `packages/client-runtime/src/state` (`models.ts`,
//! `threadSort.ts`, `threadSettled.ts`) and `apps/web/src/components/Sidebar.logic.ts`.
//! Every client must agree on them, so keep them in step with those files.

use std::cmp::Ordering;
use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

pub const SUBSCRIBE: &str = "orchestration.subscribeShell";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    /// Legacy field; the server folds it into the project's settings override.
    #[serde(default)]
    pub default_model_selection: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lineage {
    pub relationship_to_parent: Option<String>,
    pub parent_thread_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRuntimeRequest {
    pub kind: String,
    /// False for async questions the run keeps working past.
    pub blocking: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackgroundTask {
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestSnapshot {
    pub title: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub repository: String,
    pub url: String,
    pub source: Option<String>,
    pub snapshot: Option<PullRequestSnapshot>,
}

/// A subset of `OrchestrationV2ThreadShell`; unknown fields are ignored so
/// newer servers stay readable.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadShell {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub creation_source: Option<String>,
    #[serde(default)]
    pub lineage: Lineage,
    #[serde(default)]
    pub pull_requests: Vec<PullRequest>,
    pub linked_pull_request: Option<PullRequest>,
    pub branch_pull_request: Option<PullRequest>,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub status: String,
    #[serde(default, deserialize_with = "present")]
    pub activity_run_status: Option<Option<String>>,
    pub latest_run_id: Option<String>,
    #[serde(default)]
    pub latest_run_stop_requested: bool,
    pub active_run_id: Option<String>,
    pub latest_run_requested_at: Option<String>,
    pub latest_run_started_at: Option<String>,
    /// Outer `None` means an older server omitted the field.
    #[serde(default, deserialize_with = "present")]
    pub latest_run_completed_at: Option<Option<String>>,
    pub active_provider_thread_id: Option<String>,
    #[serde(default)]
    pub blocked_on_thread_wait: bool,
    pub last_error: Option<String>,
    pub last_error_class: Option<String>,
    pub pending_runtime_request: Option<PendingRuntimeRequest>,
    #[serde(default)]
    pub pending_background_tasks: Vec<BackgroundTask>,
    pub latest_user_message_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub settled_override: Option<String>,
    pub settled_at: Option<String>,
    pub unsettled_at: Option<String>,
    pub snoozed_until: Option<String>,
    pub snoozed_at: Option<String>,
    pub pinned_at: Option<String>,
    pub pin_order_key: Option<String>,
    pub active_order_key: Option<String>,
    pub last_visited_at: Option<String>,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<String>>, D::Error> {
    Option::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    pub snapshot_sequence: u64,
    pub projects: Vec<Project>,
    pub threads: Vec<ThreadShell>,
    pub archived_threads: Vec<ThreadShell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadLocation {
    Active,
    Archive,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum ShellStreamItem {
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot", rename_all = "camelCase")]
    Snapshot {
        snapshot: ShellSnapshot,
        /// Present only on project-metadata refreshes, which carry no threads
        /// and must not replace the ones already loaded.
        #[serde(default)]
        resolved_repository_identity_roots: Option<Vec<String>>,
    },
    #[serde(rename = "project.updated")]
    ProjectUpdated { sequence: u64, project: Project },
    #[serde(rename = "project.removed", rename_all = "camelCase")]
    ProjectRemoved { sequence: u64, project_id: String },
    #[serde(rename = "thread.updated")]
    ThreadUpdated {
        sequence: u64,
        location: ThreadLocation,
        thread: Box<ThreadShell>,
    },
    #[serde(rename = "thread.removed", rename_all = "camelCase")]
    ThreadRemoved {
        sequence: u64,
        location: ThreadLocation,
        thread_id: String,
    },
    /// A kind this client predates. Skipping it keeps the stream alive.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Default)]
pub struct ShellState {
    /// Resume point for `afterSequence` after a reconnect.
    pub sequence: Option<u64>,
    pub projects: Vec<Project>,
    pub threads: HashMap<String, ThreadShell>,
    pub archived: HashMap<String, ThreadShell>,
}

impl ShellState {
    pub fn is_loaded(&self) -> bool {
        self.sequence.is_some()
    }

    /// Applies one stream item. Returns whether anything visible changed.
    pub fn apply(&mut self, item: ShellStreamItem) -> bool {
        let sequence = match &item {
            ShellStreamItem::Synchronized | ShellStreamItem::Unknown => return false,
            ShellStreamItem::Snapshot {
                snapshot,
                resolved_repository_identity_roots,
            } => {
                // `mergeShellSnapshotProjects` only takes repository identity
                // from a refresh, which this client does not show yet.
                if resolved_repository_identity_roots.is_some() && self.is_loaded() {
                    return false;
                }
                let snapshot = snapshot.clone();
                self.sequence = Some(snapshot.snapshot_sequence);
                self.projects = snapshot.projects;
                self.threads = by_id(snapshot.threads);
                self.archived = by_id(snapshot.archived_threads);
                return true;
            }
            ShellStreamItem::ProjectUpdated { sequence, .. }
            | ShellStreamItem::ProjectRemoved { sequence, .. }
            | ShellStreamItem::ThreadUpdated { sequence, .. }
            | ShellStreamItem::ThreadRemoved { sequence, .. } => *sequence,
        };
        // Replays after a resume can repeat events the snapshot already holds.
        if self.sequence.is_some_and(|current| sequence <= current) {
            return false;
        }
        self.sequence = Some(sequence);
        match item {
            ShellStreamItem::ProjectUpdated { project, .. } => {
                match self
                    .projects
                    .iter_mut()
                    .find(|existing| existing.id == project.id)
                {
                    Some(existing) => *existing = project,
                    None => self.projects.push(project),
                }
            }
            ShellStreamItem::ProjectRemoved { project_id, .. } => {
                self.projects.retain(|project| project.id != project_id);
            }
            ShellStreamItem::ThreadUpdated {
                location, thread, ..
            } => {
                let (target, other) = self.locations(location);
                other.remove(&thread.id);
                target.insert(thread.id.clone(), *thread);
            }
            ShellStreamItem::ThreadRemoved {
                location,
                thread_id,
                ..
            } => {
                self.locations(location).0.remove(&thread_id);
            }
            ShellStreamItem::Snapshot { .. }
            | ShellStreamItem::Synchronized
            | ShellStreamItem::Unknown => unreachable!("handled above"),
        }
        true
    }

    fn locations(
        &mut self,
        location: ThreadLocation,
    ) -> (
        &mut HashMap<String, ThreadShell>,
        &mut HashMap<String, ThreadShell>,
    ) {
        match location {
            ThreadLocation::Active => (&mut self.threads, &mut self.archived),
            ThreadLocation::Archive => (&mut self.archived, &mut self.threads),
        }
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|project| project.id == id)
    }
}

fn by_id(threads: Vec<ThreadShell>) -> HashMap<String, ThreadShell> {
    threads
        .into_iter()
        .map(|thread| (thread.id.clone(), thread))
        .collect()
}

/// Milliseconds since the epoch, or `None` for a missing or unparseable time.
pub fn timestamp_ms(value: Option<&str>) -> Option<i64> {
    value?
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|timestamp| timestamp.as_millisecond())
}

const TERMINAL_RUN_STATUSES: &[&str] = &[
    "cancelled",
    "completed",
    "failed",
    "interrupted",
    "rolled_back",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    Approval,
    Input,
    Working,
    Waiting,
    Failed,
    Limited,
    Ready,
}

impl ThreadShell {
    pub fn is_subagent(&self) -> bool {
        self.lineage.relationship_to_parent.as_deref() == Some("subagent")
    }

    /// The live run status, or `None` for a thread that never ran (`shellRuntime`).
    pub fn runtime_status(&self) -> Option<&str> {
        // Subagents and monitors keep a finished turn open; commands do not.
        let parked = self.status != "failed"
            && self.pending_background_tasks.iter().any(|task| {
                matches!(
                    task.kind.as_str(),
                    "subagent" | "monitor" | "background_task"
                )
            });
        if self.latest_run_id.is_none() && self.active_provider_thread_id.is_none() && !parked {
            return None;
        }
        Some(if parked {
            "idle"
        } else {
            self.activity_run_status
                .as_ref()
                .and_then(|status| status.as_deref())
                .unwrap_or(&self.status)
        })
    }

    pub fn latest_run_status(&self) -> Option<&str> {
        self.latest_run_id.as_ref()?;
        Some(if self.status == "idle" {
            "completed"
        } else {
            &self.status
        })
    }

    pub fn latest_run_completed_at(&self) -> Option<&str> {
        self.latest_run_id.as_ref()?;
        match &self.latest_run_completed_at {
            Some(completed_at) => completed_at.as_deref(),
            None if self.status == "idle"
                || TERMINAL_RUN_STATUSES.contains(&self.status.as_str()) =>
            {
                Some(&self.updated_at)
            }
            None => None,
        }
    }

    pub fn has_pending_approval(&self) -> bool {
        self.pending_runtime_request
            .as_ref()
            .is_some_and(|request| request.kind != "user_input" && request.kind != "auth_refresh")
    }

    pub fn has_pending_user_input(&self) -> bool {
        let working = matches!(
            self.runtime_status(),
            Some("preparing" | "starting" | "running")
        );
        self.pending_runtime_request
            .as_ref()
            .is_some_and(|request| {
                request.kind == "user_input" && (request.blocking != Some(false) || !working)
            })
    }

    /// `resolveSidebarThreadStatus`.
    pub fn status(&self) -> ThreadStatus {
        if self.has_pending_approval() {
            return ThreadStatus::Approval;
        }
        if self.has_pending_user_input() {
            return ThreadStatus::Input;
        }
        match self.runtime_status() {
            Some("running") if self.blocked_on_thread_wait => ThreadStatus::Waiting,
            Some("preparing" | "queued" | "starting" | "running" | "waiting") => {
                ThreadStatus::Working
            }
            Some("idle") => ThreadStatus::Waiting,
            Some("failed") if self.last_error_class.as_deref() == Some("usage_limit") => {
                ThreadStatus::Limited
            }
            Some("failed") => ThreadStatus::Failed,
            _ => ThreadStatus::Ready,
        }
    }

    /// A finished run the person has not opened since (`hasUnseenCompletion`).
    pub fn has_unseen_completion(&self) -> bool {
        let Some(completed_at) = timestamp_ms(self.latest_run_completed_at()) else {
            return false;
        };
        let Some(last_visited_at) = self.last_visited_at.as_deref() else {
            return false;
        };
        timestamp_ms(Some(last_visited_at)).is_none_or(|visited| completed_at > visited)
    }

    /// Only a request for the person or a fresh failure ends a snooze early.
    fn raised_hand_while_snoozed(&self) -> bool {
        if self.pending_runtime_request.is_some() {
            return true;
        }
        if self.runtime_status() != Some("failed") {
            return false;
        }
        let failed_at = if self.latest_run_status() == Some("failed")
            && self.latest_run_completed_at().is_some()
        {
            self.latest_run_completed_at()
        } else {
            Some(self.updated_at.as_str())
        };
        match self.snoozed_at.as_deref() {
            None => true,
            Some(snoozed_at) => match (timestamp_ms(failed_at), timestamp_ms(Some(snoozed_at))) {
                (Some(failed), Some(snoozed)) => failed > snoozed,
                _ => false,
            },
        }
    }

    pub fn is_snoozed(&self, now_ms: i64) -> bool {
        timestamp_ms(self.snoozed_until.as_deref()).is_some_and(|wake| wake > now_ms)
            && !self.raised_hand_while_snoozed()
    }

    /// `resolveSettledThreadTimestamp`: when the thread settled, else its last activity.
    pub fn settled_at_ms(&self) -> i64 {
        if let Some(settled) = timestamp_ms(self.settled_at.as_deref()) {
            return settled;
        }
        [
            self.latest_user_message_at.as_deref(),
            self.latest_run_requested_at.as_deref(),
            self.latest_run_started_at.as_deref(),
            self.latest_run_completed_at(),
        ]
        .into_iter()
        .filter_map(timestamp_ms)
        .max()
        .or_else(|| timestamp_ms(Some(&self.updated_at)))
        .unwrap_or(0)
    }

    fn active_anchor_ms(&self) -> i64 {
        timestamp_ms(Some(&self.created_at))
            .unwrap_or(0)
            .max(timestamp_ms(self.unsettled_at.as_deref()).unwrap_or(0))
    }
}

/// Which optional thread features the environment supports, from
/// `environment.capabilities` in the server config.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShellCapabilities {
    pub settlement: bool,
    pub snooze: bool,
}

#[derive(Debug, Default)]
pub struct SidebarSections<'a> {
    pub pinned: Vec<&'a ThreadShell>,
    pub active: Vec<&'a ThreadShell>,
    pub snoozed: Vec<&'a ThreadShell>,
    pub settled: Vec<&'a ThreadShell>,
}

/// `partitionSidebarThreads` without the beta Working shelf.
pub fn sidebar_sections(
    state: &ShellState,
    capabilities: ShellCapabilities,
    now_ms: i64,
) -> SidebarSections<'_> {
    let mut sections = SidebarSections::default();
    for thread in state.threads.values() {
        if thread.archived_at.is_some() || thread.is_subagent() {
            continue;
        }
        let section = if capabilities.snooze && thread.is_snoozed(now_ms) {
            &mut sections.snoozed
        } else if capabilities.settlement && thread.settled_override.as_deref() == Some("settled") {
            &mut sections.settled
        } else if thread.pinned_at.is_some() {
            &mut sections.pinned
        } else {
            &mut sections.active
        };
        section.push(thread);
    }
    // Ids break ties so every client renders equal-key threads identically.
    sections.pinned.sort_by(|left, right| {
        match (&left.pin_order_key, &right.pin_order_key) {
            (Some(left_key), Some(right_key)) => left_key.cmp(right_key),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => timestamp_ms(Some(&right.created_at))
                .unwrap_or(0)
                .cmp(&timestamp_ms(Some(&left.created_at)).unwrap_or(0)),
        }
        .then_with(|| left.id.cmp(&right.id))
    });
    sections.active.sort_by(|left, right| {
        match (&left.active_order_key, &right.active_order_key) {
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(left_key), Some(right_key)) => left_key.cmp(right_key),
            (None, None) => right.active_anchor_ms().cmp(&left.active_anchor_ms()),
        }
        .then_with(|| left.id.cmp(&right.id))
    });
    sections
        .snoozed
        .sort_by_key(|thread| timestamp_ms(thread.snoozed_until.as_deref()).unwrap_or(0));
    sections.settled.sort_by(|left, right| {
        right
            .settled_at_ms()
            .cmp(&left.settled_at_ms())
            .then_with(|| left.id.cmp(&right.id))
    });
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn thread(id: &str, extra: serde_json::Value) -> ThreadShell {
        let mut value = json!({
            "id": id,
            "projectId": "p",
            "title": id,
            "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": id },
            "status": "idle",
            "latestRunId": null,
            "activeProviderThreadId": null,
            "pendingRuntimeRequest": null,
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z",
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn state(threads: Vec<ThreadShell>) -> ShellState {
        ShellState {
            sequence: Some(0),
            projects: vec![],
            threads: by_id(threads),
            archived: HashMap::new(),
        }
    }

    fn ids(threads: &[&ThreadShell]) -> Vec<String> {
        threads.iter().map(|thread| thread.id.clone()).collect()
    }

    const ALL: ShellCapabilities = ShellCapabilities {
        settlement: true,
        snooze: true,
    };
    const NOW: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z

    #[test]
    fn stale_events_after_a_snapshot_are_ignored() {
        let mut shell = ShellState::default();
        shell.apply(ShellStreamItem::Snapshot {
            resolved_repository_identity_roots: None,
            snapshot: ShellSnapshot {
                snapshot_sequence: 5,
                projects: vec![],
                threads: vec![thread("a", json!({}))],
                archived_threads: vec![],
            },
        });
        let stale = ShellStreamItem::ThreadRemoved {
            sequence: 5,
            location: ThreadLocation::Active,
            thread_id: "a".into(),
        };
        assert!(!shell.apply(stale));
        assert!(shell.threads.contains_key("a"));

        let archived = ShellStreamItem::ThreadUpdated {
            sequence: 6,
            location: ThreadLocation::Archive,
            thread: Box::new(thread("a", json!({}))),
        };
        assert!(shell.apply(archived));
        assert!(!shell.threads.contains_key("a"));
        assert!(shell.archived.contains_key("a"));
    }

    #[test]
    fn an_enrichment_snapshot_keeps_threads_and_the_cursor() {
        let mut shell = ShellState::default();
        let full: ShellStreamItem = serde_json::from_value(json!({
            "kind": "snapshot",
            "snapshot": { "snapshotSequence": 7, "projects": [], "threads": [], "archivedThreads": [] },
        }))
        .unwrap();
        shell.apply(full);
        shell.threads = by_id(vec![thread("a", json!({}))]);
        // The server's project-metadata refresh carries no threads.
        let enrichment: ShellStreamItem = serde_json::from_value(json!({
            "kind": "snapshot",
            "snapshot": { "snapshotSequence": 9, "projects": [], "threads": [], "archivedThreads": [] },
            "resolvedRepositoryIdentityRoots": [],
        }))
        .unwrap();
        shell.apply(enrichment);
        assert!(shell.threads.contains_key("a"));
        assert_eq!(shell.sequence, Some(7));
    }

    #[test]
    fn unknown_stream_kinds_decode_and_are_skipped() {
        let item: ShellStreamItem =
            serde_json::from_value(json!({ "kind": "future.thing", "sequence": 9 })).unwrap();
        assert!(matches!(item, ShellStreamItem::Unknown));
    }

    #[test]
    fn status_prefers_requests_for_the_person_over_run_state() {
        let approval = thread(
            "a",
            json!({ "latestRunId": "r", "status": "running", "pendingRuntimeRequest": { "kind": "command", "createdAt": "x" } }),
        );
        assert_eq!(approval.status(), ThreadStatus::Approval);
        // A non-blocking question while the run keeps working is not yet the person's turn.
        let async_question = thread(
            "b",
            json!({ "latestRunId": "r", "status": "running", "pendingRuntimeRequest": { "kind": "user_input", "blocking": false } }),
        );
        assert_eq!(async_question.status(), ThreadStatus::Working);
        let limited = thread(
            "c",
            json!({ "latestRunId": "r", "status": "failed", "lastErrorClass": "usage_limit" }),
        );
        assert_eq!(limited.status(), ThreadStatus::Limited);
        assert_eq!(thread("d", json!({})).status(), ThreadStatus::Ready);
    }

    #[test]
    fn background_subagents_park_a_finished_thread() {
        let parked = thread(
            "a",
            json!({ "latestRunId": "r", "status": "completed", "pendingBackgroundTasks": [{ "kind": "subagent" }] }),
        );
        assert_eq!(parked.status(), ThreadStatus::Waiting);
        let command = thread(
            "b",
            json!({ "latestRunId": "r", "status": "completed", "pendingBackgroundTasks": [{ "kind": "command" }] }),
        );
        assert_eq!(command.status(), ThreadStatus::Ready);
    }

    #[test]
    fn sections_follow_snooze_then_settle_then_pin() {
        let shell = state(vec![
            thread(
                "snoozed",
                json!({ "snoozedUntil": "2026-01-02T00:00:00Z", "settledOverride": "settled" }),
            ),
            thread("woken", json!({ "snoozedUntil": "2025-12-31T00:00:00Z" })),
            thread(
                "settled",
                json!({ "settledOverride": "settled", "pinnedAt": "2026-01-01T00:00:00Z" }),
            ),
            thread("pinned", json!({ "pinnedAt": "2026-01-01T00:00:00Z" })),
            thread(
                "subagent",
                json!({ "lineage": { "relationshipToParent": "subagent" } }),
            ),
        ]);
        let sections = sidebar_sections(&shell, ALL, NOW);
        assert_eq!(ids(&sections.snoozed), ["snoozed"]);
        assert_eq!(ids(&sections.settled), ["settled"]);
        assert_eq!(ids(&sections.pinned), ["pinned"]);
        assert_eq!(ids(&sections.active), ["woken"]);

        // Without server support the same threads stay in the inbox.
        let none = sidebar_sections(&shell, ShellCapabilities::default(), NOW);
        assert_eq!(none.snoozed.len() + none.settled.len(), 0);
    }

    #[test]
    fn a_pending_request_wakes_a_snoozed_thread() {
        let raised = thread(
            "a",
            json!({ "snoozedUntil": "2026-01-02T00:00:00Z", "pendingRuntimeRequest": { "kind": "user_input" } }),
        );
        assert!(!raised.is_snoozed(NOW));
    }

    #[test]
    fn active_threads_put_unkeyed_newest_first_then_keys_in_order() {
        let shell = state(vec![
            thread("keyed-b", json!({ "activeOrderKey": "n" })),
            thread("keyed-a", json!({ "activeOrderKey": "c" })),
            thread("old", json!({ "createdAt": "2025-01-01T00:00:00Z" })),
            thread(
                "unsettled",
                json!({ "createdAt": "2025-01-01T00:00:00Z", "unsettledAt": "2026-01-01T00:00:00Z" }),
            ),
        ]);
        let sections = sidebar_sections(&shell, ALL, NOW);
        assert_eq!(
            ids(&sections.active),
            ["unsettled", "old", "keyed-a", "keyed-b"]
        );
    }

    #[test]
    fn pinned_threads_put_keys_first_then_newest() {
        let pinned = json!({ "pinnedAt": "2026-01-01T00:00:00Z" });
        let with = |extra: serde_json::Value| {
            let mut value = pinned.clone();
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            value
        };
        let shell = state(vec![
            thread("new", with(json!({ "createdAt": "2026-01-01T00:00:00Z" }))),
            thread("keyed", with(json!({ "pinOrderKey": "m" }))),
            thread("old", with(json!({ "createdAt": "2025-01-01T00:00:00Z" }))),
        ]);
        assert_eq!(
            ids(&sidebar_sections(&shell, ALL, NOW).pinned),
            ["keyed", "new", "old"]
        );
    }

    #[test]
    fn settled_threads_sort_by_settle_time_then_last_activity() {
        let shell = state(vec![
            thread(
                "activity",
                json!({ "settledOverride": "settled", "latestUserMessageAt": "2025-06-01T00:00:00Z" }),
            ),
            thread(
                "settled",
                json!({ "settledOverride": "settled", "settledAt": "2025-09-01T00:00:00Z" }),
            ),
        ]);
        assert_eq!(
            ids(&sidebar_sections(&shell, ALL, NOW).settled),
            ["settled", "activity"]
        );
    }

    #[test]
    fn a_completion_is_unseen_only_after_the_last_visit() {
        let completed = json!({ "latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-01-01T12:00:00Z" });
        let mut seen = completed.clone();
        seen["lastVisitedAt"] = json!("2026-01-01T13:00:00Z");
        let mut unseen = completed.clone();
        unseen["lastVisitedAt"] = json!("2026-01-01T11:00:00Z");
        assert!(!thread("a", seen).has_unseen_completion());
        assert!(thread("b", unseen).has_unseen_completion());
        // Never visited means a thread the person did not start, e.g. an automation's.
        assert!(!thread("c", completed).has_unseen_completion());
    }
}
