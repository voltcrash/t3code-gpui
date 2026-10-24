//! One thread's detail: the projection streamed by `orchestration.subscribeThread`
//! and the timeline the chat view draws from it.
//!
//! The reducer ports `packages/client-runtime/src/state/orchestrationV2Projection.ts`
//! and the visibility rules in `packages/shared/src/orchestrationV2Timeline.ts`.
//! Timeline entries follow `deriveTimelineEntriesFromVisibleTurnItems` and
//! `projectedWorkEntry` in `apps/web/src/session-logic.ts`. Keep them in step.
//!
//! Only the collections the timeline reads are kept. Events for the others
//! still advance the resume sequence.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Map, Value};

pub const SUBSCRIBE: &str = "orchestration.subscribeThread";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: String,
    pub run_id: String,
    pub root_node_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub id: String,
    pub kind: String,
    pub status: Option<String>,
}

/// A turn item. The fields every type shares are typed; the rest stay as
/// JSON so item types this client predates still render a row.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnItem {
    pub id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub ordinal: u64,
    pub status: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl TurnItem {
    pub fn str(&self, field: &str) -> Option<&str> {
        self.fields.get(field).and_then(Value::as_str)
    }

    fn input_intent(&self) -> Option<&str> {
        self.str("inputIntent")
    }

    pub fn created_at(&self) -> &str {
        self.started_at.as_deref().unwrap_or(&self.updated_at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Local,
    Inherited,
    Synthetic,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTurnItem {
    pub position: usize,
    pub visibility: Visibility,
    pub source_thread_id: String,
    pub source_item_id: String,
    pub item: TurnItem,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjection {
    pub thread: Value,
    pub runs: Vec<Run>,
    pub attempts: Vec<RunAttempt>,
    pub plans: Vec<Plan>,
    pub turn_items: Vec<TurnItem>,
    pub visible_turn_items: Vec<ProjectedTurnItem>,
}

impl ThreadProjection {
    fn thread_id(&self) -> Option<&str> {
        self.thread.get("id").and_then(Value::as_str)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub thread_id: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum ThreadStreamItem {
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot", rename_all = "camelCase")]
    Snapshot {
        snapshot_sequence: u64,
        projection: Box<ThreadProjection>,
        #[serde(default)]
        has_more_history: bool,
        latest_local_turn_ordinal: Option<u64>,
    },
    #[serde(rename = "event")]
    Event { sequence: u64, event: DomainEvent },
    #[serde(other)]
    Unknown,
}

/// The input for `orchestration.subscribeThread`. A resumed subscription
/// replays only the events after `after_sequence`.
pub fn subscribe_input(thread_id: &str, after_sequence: Option<u64>) -> Value {
    let mut input = serde_json::json!({
        "threadId": thread_id,
        // A bounded window keeps opening a long thread cheap. Compact turn
        // items would need `boundedSnapshotProjection` to restore, so stay off.
        "acceptBoundedSnapshot": true,
        "requestCompletionMarker": true,
    });
    if let Some(sequence) = after_sequence {
        input["afterSequence"] = sequence.into();
    }
    input
}

#[derive(Debug, Default)]
pub struct ThreadState {
    pub sequence: Option<u64>,
    pub projection: Option<ThreadProjection>,
    /// The snapshot was a bounded window; older history is not loaded.
    pub partial: bool,
    pub latest_local_turn_ordinal: Option<u64>,
    /// Replay after a (re)subscribe is complete.
    pub synchronized: bool,
}

impl ThreadState {
    /// Applies one stream item. Returns whether the timeline may have changed.
    pub fn apply(&mut self, item: ThreadStreamItem) -> bool {
        match item {
            ThreadStreamItem::Synchronized => {
                let changed = !self.synchronized;
                self.synchronized = true;
                changed
            }
            ThreadStreamItem::Unknown => false,
            ThreadStreamItem::Snapshot {
                snapshot_sequence,
                projection,
                has_more_history,
                latest_local_turn_ordinal,
            } => {
                self.sequence = Some(snapshot_sequence);
                self.projection = Some(*projection);
                self.partial = has_more_history;
                self.latest_local_turn_ordinal = latest_local_turn_ordinal;
                true
            }
            ThreadStreamItem::Event { sequence, event } => {
                if self.sequence.is_some_and(|current| sequence <= current) {
                    return false;
                }
                self.sequence = Some(sequence);
                let Some(projection) = self.projection.as_mut() else {
                    return false;
                };
                apply_event(
                    projection,
                    event,
                    self.partial,
                    self.latest_local_turn_ordinal,
                )
            }
        }
    }

    pub fn timeline(&self) -> Vec<TimelineEntry> {
        self.projection
            .as_ref()
            .map(timeline_entries)
            .unwrap_or_default()
    }
}

/// Mirrors `applyOrchestrationV2ProjectionEvent` for the collections kept here.
fn apply_event(
    projection: &mut ThreadProjection,
    event: DomainEvent,
    partial: bool,
    latest_local_turn_ordinal: Option<u64>,
) -> bool {
    if projection.thread_id() != Some(event.thread_id.as_str()) {
        return false;
    }
    let kind = event.kind.as_str();
    match kind {
        "thread.pull-request-link-updated" => false,
        _ if kind.starts_with("thread.") => {
            projection.thread = event.payload;
            true
        }
        "run.created" | "run.updated" => {
            let Ok(run) = serde_json::from_value::<Run>(event.payload) else {
                return false;
            };
            upsert(&mut projection.runs, run, |run| &run.id);
            refresh_visibility(projection);
            true
        }
        "run-attempt.created" | "run-attempt.updated" => {
            let Ok(attempt) = serde_json::from_value::<RunAttempt>(event.payload) else {
                return false;
            };
            upsert(&mut projection.attempts, attempt, |attempt| &attempt.id);
            refresh_visibility(projection);
            true
        }
        "plan.updated" => {
            let Ok(plan) = serde_json::from_value::<Plan>(event.payload) else {
                return false;
            };
            upsert(&mut projection.plans, plan, |plan| &plan.id);
            true
        }
        "turn-item.updated" => match serde_json::from_value::<TurnItem>(event.payload) {
            Ok(item) => apply_turn_item(projection, item, partial, latest_local_turn_ordinal),
            Err(error) => {
                tracing::debug!("skipping an unreadable turn item: {error}");
                false
            }
        },
        _ => false,
    }
}

fn upsert<T>(items: &mut Vec<T>, item: T, id: impl Fn(&T) -> &String) {
    match items.iter().position(|existing| id(existing) == id(&item)) {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

fn apply_turn_item(
    projection: &mut ThreadProjection,
    item: TurnItem,
    partial: bool,
    latest_local_turn_ordinal: Option<u64>,
) -> bool {
    // Streamed updates land on the newest items, so search from the end.
    let index = projection
        .turn_items
        .iter()
        .rposition(|existing| existing.id == item.id);
    if partial
        && index.is_none()
        && should_drop_missing_partial_item(projection, &item, latest_local_turn_ordinal)
    {
        return false;
    }
    let was_interrupt_request =
        index.is_some_and(|index| projection.turn_items[index].kind == "run_interrupt_request");
    match index {
        Some(index) => projection.turn_items[index] = item.clone(),
        None => projection.turn_items.push(item.clone()),
    }
    // Only interrupt requests change another item's visibility. Streaming
    // text must not recheck every row against every run.
    if item.kind == "run_interrupt_request" || was_interrupt_request {
        refresh_visibility(projection);
    }
    if is_visible(&item, projection) {
        upsert_visible(projection, item, partial, latest_local_turn_ordinal);
    } else if let Some(index) = projection
        .visible_turn_items
        .iter()
        .position(|row| row.source_item_id == item.id)
    {
        projection.visible_turn_items.remove(index);
        renumber(&mut projection.visible_turn_items);
    }
    true
}

const UNFINISHED_RUN_STATUSES: [&str; 5] =
    ["preparing", "queued", "starting", "running", "waiting"];

fn should_drop_missing_partial_item(
    projection: &ThreadProjection,
    item: &TurnItem,
    latest_local_turn_ordinal: Option<u64>,
) -> bool {
    if projection
        .visible_turn_items
        .iter()
        .any(|row| row.source_item_id == item.id)
    {
        return false;
    }
    if let Some(run_id) = &item.run_id {
        // Items of an unfinished run are new, not unloaded history.
        if projection
            .runs
            .iter()
            .any(|run| &run.id == run_id && UNFINISHED_RUN_STATUSES.contains(&run.status.as_str()))
        {
            return false;
        }
        if projection.visible_turn_items.iter().any(|row| {
            row.visibility == Visibility::Local
                && row.item.run_id.as_ref() == Some(run_id)
                && row.item.ordinal <= item.ordinal
        }) {
            return false;
        }
    }
    if latest_local_turn_ordinal.is_some_and(|latest| item.ordinal <= latest) {
        return true;
    }
    projection
        .visible_turn_items
        .iter()
        .filter(|row| row.visibility == Visibility::Local)
        .map(|row| row.item.ordinal)
        .min()
        .is_some_and(|oldest| item.ordinal < oldest)
}

fn upsert_visible(
    projection: &mut ThreadProjection,
    item: TurnItem,
    partial: bool,
    latest_local_turn_ordinal: Option<u64>,
) {
    let rows = &projection.visible_turn_items;
    let index = rows.iter().rposition(|row| row.source_item_id == item.id);
    if let Some(index) = index {
        let row = &rows[index];
        if row.visibility == Visibility::Local && row.item == item {
            return;
        }
    }
    if index.is_none()
        && partial
        && should_drop_missing_partial_item(projection, &item, latest_local_turn_ordinal)
    {
        return;
    }
    let rows = &mut projection.visible_turn_items;
    if let Some(index) = index {
        rows.remove(index);
    }
    // The item goes before the first later local row.
    let insertion = rows
        .iter()
        .position(|row| {
            row.visibility == Visibility::Local
                && (row.item.ordinal > item.ordinal
                    || (row.item.ordinal == item.ordinal && row.item.id > item.id))
        })
        .unwrap_or(rows.len());
    rows.insert(
        insertion,
        ProjectedTurnItem {
            position: insertion,
            visibility: Visibility::Local,
            source_thread_id: item.thread_id.clone(),
            source_item_id: item.id.clone(),
            item,
        },
    );
    renumber(rows);
}

fn renumber(rows: &mut [ProjectedTurnItem]) {
    for (position, row) in rows.iter_mut().enumerate() {
        row.position = position;
    }
}

/// Drops local rows that a run or attempt change has hidden.
fn refresh_visibility(projection: &mut ThreadProjection) {
    let visibility = VisibilityRules::new(
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    );
    let before = projection.visible_turn_items.len();
    projection
        .visible_turn_items
        .retain(|row| row.visibility != Visibility::Local || visibility.is_visible(&row.item));
    if projection.visible_turn_items.len() != before {
        renumber(&mut projection.visible_turn_items);
    }
}

fn is_visible(item: &TurnItem, projection: &ThreadProjection) -> bool {
    VisibilityRules::new(
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    )
    .is_visible(item)
}

/// `createOrchestrationV2TurnItemVisibility`.
struct VisibilityRules<'a> {
    statuses: HashMap<&'a str, &'a str>,
    superseded_roots: HashSet<(&'a str, &'a str)>,
    interrupt_runs: HashSet<&'a str>,
}

impl<'a> VisibilityRules<'a> {
    fn new(runs: &'a [Run], attempts: &'a [RunAttempt], turn_items: &'a [TurnItem]) -> Self {
        Self {
            statuses: runs
                .iter()
                .map(|run| (run.id.as_str(), run.status.as_str()))
                .collect(),
            superseded_roots: attempts
                .iter()
                .filter(|attempt| attempt.status == "superseded")
                .map(|attempt| (attempt.run_id.as_str(), attempt.root_node_id.as_str()))
                .collect(),
            interrupt_runs: turn_items
                .iter()
                .filter(|item| item.kind == "run_interrupt_request")
                .filter_map(|item| item.run_id.as_deref())
                .collect(),
        }
    }

    fn is_visible(&self, item: &TurnItem) -> bool {
        let status = item
            .run_id
            .as_deref()
            .and_then(|run| self.statuses.get(run).copied());
        if status == Some("rolled_back") {
            return false;
        }
        // A queued message whose run was cancelled never reached the provider.
        if status == Some("cancelled")
            && item.kind == "user_message"
            && item.input_intent() == Some("queued_turn")
        {
            return false;
        }
        // Paired stop-then-steer results stay; plain-steer results hide.
        !(item.kind == "run_interrupt_result"
            && match (&item.run_id, &item.node_id) {
                (Some(run), Some(node)) => {
                    self.superseded_roots
                        .contains(&(run.as_str(), node.as_str()))
                        && !self.interrupt_runs.contains(run.as_str())
                }
                _ => false,
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkStatus {
    InProgress,
    Completed,
    Idle,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimelineContent {
    Message {
        role: Role,
        text: String,
        streaming: bool,
    },
    ProposedPlan {
        markdown: String,
        status: String,
    },
    Work {
        label: String,
        detail: Option<String>,
        status: WorkStatus,
        thinking: bool,
    },
    /// Fork, handoff, interrupt, subagent and secret cards.
    Event {
        kind: String,
        label: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    pub id: String,
    pub created_at: String,
    pub content: TimelineContent,
}

const STANDALONE_TYPES: [&str; 6] = [
    "fork",
    "handoff",
    "run_interrupt_request",
    "run_interrupt_result",
    "secret_request",
    "subagent",
];
const WORKSPACE_PREPARATION_INPUT: &str = "Preparing workspace";
const WORKSPACE_PREPARATION_FAILURE_CODE: &str = "workspace_preparation_failed";

fn is_workspace_preparation(item: &TurnItem) -> bool {
    (item.kind == "command_execution" && item.str("input") == Some(WORKSPACE_PREPARATION_INPUT))
        || (item.kind == "error"
            && item.status == "cancelled"
            && item
                .fields
                .get("failure")
                .and_then(|failure| failure.get("code"))
                .and_then(Value::as_str)
                == Some(WORKSPACE_PREPARATION_FAILURE_CODE))
}

pub fn timeline_entries(projection: &ThreadProjection) -> Vec<TimelineEntry> {
    let folded_answers: HashSet<String> = projection
        .visible_turn_items
        .iter()
        .filter(|row| row.item.kind == "user_input_request")
        .filter_map(|row| {
            row.item
                .fields
                .get("questionAnswer")?
                .get("requestId")?
                .as_str()
                .map(|request| format!("async-answer:{request}"))
        })
        .collect();

    projection
        .visible_turn_items
        .iter()
        .filter_map(|row| {
            let item = &row.item;
            if is_workspace_preparation(item)
                || item.kind == "todo_list"
                || item.kind == "checkpoint"
                || (item.kind == "user_message"
                    && item
                        .str("messageId")
                        .is_some_and(|id| folded_answers.contains(id)))
            {
                return None;
            }
            Some(TimelineEntry {
                id: item.id.clone(),
                created_at: item.created_at().to_owned(),
                content: entry_content(item, projection),
            })
        })
        .collect()
}

fn entry_content(item: &TurnItem, projection: &ThreadProjection) -> TimelineContent {
    let text = || item.str("text").unwrap_or_default().to_owned();
    match item.kind.as_str() {
        "user_message" => TimelineContent::Message {
            role: Role::User,
            text: text(),
            streaming: false,
        },
        "assistant_message" => TimelineContent::Message {
            role: Role::Assistant,
            text: text(),
            streaming: item.fields.get("streaming") == Some(&Value::Bool(true)),
        },
        "proposed_plan" => {
            let status = item
                .str("planId")
                .and_then(|id| projection.plans.iter().find(|plan| plan.id == id))
                .filter(|plan| plan.kind == "proposed_plan")
                .and_then(|plan| plan.status.clone())
                .unwrap_or_else(|| "active".into());
            TimelineContent::ProposedPlan {
                markdown: item.str("markdown").unwrap_or_default().to_owned(),
                status,
            }
        }
        kind if STANDALONE_TYPES.contains(&kind) => TimelineContent::Event {
            kind: kind.to_owned(),
            label: title(item).unwrap_or_else(|| kind.replace('_', " ")),
        },
        _ => {
            let (label, detail) = work_label(item);
            TimelineContent::Work {
                label,
                detail,
                status: work_status(&item.status),
                thinking: item.kind == "reasoning",
            }
        }
    }
}

fn title(item: &TurnItem) -> Option<String> {
    item.title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
}

fn work_status(status: &str) -> WorkStatus {
    match status {
        "pending" | "running" | "waiting" => WorkStatus::InProgress,
        "completed" => WorkStatus::Completed,
        "idle" => WorkStatus::Idle,
        "failed" => WorkStatus::Failed,
        _ => WorkStatus::Stopped,
    }
}

/// `projectedWorkEntry`'s label and detail. Tool-specific labels that need
/// `toolActivity.ts` classification fall back to the item's title.
fn work_label(item: &TurnItem) -> (String, Option<String>) {
    let title = title(item);
    let field = |name: &str| {
        item.str(name)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let or = |fallback: &str| title.clone().unwrap_or_else(|| fallback.to_owned());
    match item.kind.as_str() {
        "notification" => (field("summary").unwrap_or_default(), None),
        "thread_created" => ("Created thread".into(), None),
        "compaction" => (compaction_label(&item.status).into(), field("summary")),
        "reasoning" => (or("Thinking"), field("text")),
        "command_execution" => (or("Ran command"), field("input")),
        "file_change" => {
            let changes = item
                .fields
                .get("changes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let label = if changes > 1 {
                format!("Changed {changes} files")
            } else {
                format!("Changed {}", field("fileName").unwrap_or_default())
            };
            (title.clone().unwrap_or(label), None)
        }
        "file_search" => (or("Searched files"), field("pattern")),
        "web_search" => {
            let patterns = item
                .fields
                .get("patterns")
                .and_then(Value::as_array)
                .map(|patterns| {
                    patterns
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|patterns| !patterns.is_empty());
            (or("Searched the web"), patterns)
        }
        "system_notice" => (field("message").unwrap_or_default(), None),
        "error" => error_label(item),
        "dynamic_tool" => (
            title
                .clone()
                .or_else(|| field("toolName"))
                .unwrap_or_else(|| "Tool call".into()),
            None,
        ),
        "approval_request" => (
            or("Approval requested"),
            field("prompt").or_else(|| field("requestKind")),
        ),
        "user_input_request" => (
            or(if item.fields.contains_key("questionAnswer") {
                "Answered questions"
            } else {
                "Input requested"
            }),
            None,
        ),
        kind => (or(&kind.replace('_', " ")), None),
    }
}

fn compaction_label(status: &str) -> &'static str {
    match status {
        "failed" => "Context compaction failed",
        "cancelled" | "interrupted" => "Context compaction stopped",
        "pending" | "running" | "waiting" => "Compacting context",
        _ => "Context compacted",
    }
}

/// `providerErrorPresentation`.
fn error_label(item: &TurnItem) -> (String, Option<String>) {
    let failure = item.fields.get("failure");
    let class = failure
        .and_then(|failure| failure.get("class"))
        .and_then(Value::as_str);
    let message = failure
        .and_then(|failure| failure.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let usage_limit = class == Some("usage_limit");
    let Some(retry) = item.fields.get("retry").filter(|retry| !retry.is_null()) else {
        let label = if usage_limit {
            "Usage limit reached".into()
        } else {
            title(item).unwrap_or_else(|| "Provider error".into())
        };
        return (label, Some(message));
    };
    let attempt = retry.get("attempt").and_then(Value::as_u64).unwrap_or(0);
    let progress = match retry.get("maxAttempts").and_then(Value::as_u64) {
        Some(max) => format!("{attempt}/{max}"),
        None => attempt.to_string(),
    };
    let label = match item.status.as_str() {
        "running" => format!("Retrying provider ({progress})"),
        "completed" => format!("Provider recovered ({progress} retries)"),
        "failed" if usage_limit => format!("Usage limit reached after {progress} retries"),
        "failed" => format!("Provider error after {progress} retries"),
        _ => format!("Provider retry stopped ({progress})"),
    };
    let delay = retry
        .get("retryDelayMs")
        .and_then(Value::as_u64)
        .filter(|delay| item.status == "running" && *delay > 0)
        .map(|delay| {
            if delay < 1_000 {
                format!(" Retrying in {delay}ms.")
            } else {
                let seconds = format!("{:.1}", delay as f64 / 1_000.0);
                format!(
                    " Retrying in {}s.",
                    seconds.strip_suffix(".0").unwrap_or(&seconds)
                )
            }
        })
        .unwrap_or_default();
    (label, Some(format!("{message}{delay}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const THREAD: &str = "thread-1";

    fn item(id: &str, kind: &str, ordinal: u64, run: Option<&str>) -> Value {
        json!({
            "id": id,
            "threadId": THREAD,
            "runId": run,
            "nodeId": run.map(|run| format!("{run}-root")),
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": ordinal,
            "status": "completed",
            "type": kind,
            "updatedAt": "2026-10-24T10:00:00.000Z",
            "text": format!("text of {id}"),
            "messageId": format!("message-{id}"),
            "inputIntent": "turn",
            "streaming": false,
        })
    }

    fn row(position: usize, item: Value) -> Value {
        json!({
            "position": position,
            "visibility": "local",
            "sourceThreadId": THREAD,
            "sourceItemId": item["id"],
            "item": item,
        })
    }

    fn snapshot(items: Vec<Value>, runs: Value, has_more_history: bool) -> ThreadStreamItem {
        let rows: Vec<Value> = items
            .iter()
            .cloned()
            .enumerate()
            .map(|(position, item)| row(position, item))
            .collect();
        serde_json::from_value(json!({
            "kind": "snapshot",
            "snapshotSequence": 10,
            "hasMoreHistory": has_more_history,
            "latestLocalTurnOrdinal": null,
            "projection": {
                "thread": { "id": THREAD, "title": "Thread" },
                "runs": runs,
                "attempts": [],
                "nodes": [],
                "plans": [],
                "messages": [],
                "turnItems": items,
                "visibleTurnItems": rows,
                "updatedAt": "2026-10-24T10:00:00.000Z",
            },
        }))
        .unwrap()
    }

    fn event(sequence: u64, kind: &str, payload: Value) -> ThreadStreamItem {
        serde_json::from_value(json!({
            "kind": "event",
            "sequence": sequence,
            "event": {
                "id": format!("event-{sequence}"),
                "type": kind,
                "threadId": THREAD,
                "occurredAt": "2026-10-24T10:00:00.000Z",
                "payload": payload,
            },
        }))
        .unwrap()
    }

    fn ids(state: &ThreadState) -> Vec<String> {
        state
            .projection
            .as_ref()
            .unwrap()
            .visible_turn_items
            .iter()
            .map(|row| row.source_item_id.clone())
            .collect()
    }

    #[test]
    fn streamed_text_replaces_the_row_in_place() {
        let mut state = ThreadState::default();
        state.apply(snapshot(
            vec![
                item("a", "user_message", 1, Some("run-1")),
                item("b", "assistant_message", 2, Some("run-1")),
            ],
            json!([{ "id": "run-1", "status": "running" }]),
            false,
        ));
        let mut update = item("b", "assistant_message", 2, Some("run-1"));
        update["text"] = "Hello".into();
        update["streaming"] = true.into();
        assert!(state.apply(event(11, "turn-item.updated", update)));

        assert_eq!(ids(&state), ["a", "b"]);
        assert_eq!(
            state.timeline()[1].content,
            TimelineContent::Message {
                role: Role::Assistant,
                text: "Hello".into(),
                streaming: true,
            }
        );
    }

    #[test]
    fn new_items_are_placed_by_ordinal() {
        let mut state = ThreadState::default();
        state.apply(snapshot(
            vec![
                item("a", "user_message", 1, Some("run-1")),
                item("c", "assistant_message", 3, Some("run-1")),
            ],
            json!([{ "id": "run-1", "status": "running" }]),
            false,
        ));
        state.apply(event(
            11,
            "turn-item.updated",
            item("b", "reasoning", 2, Some("run-1")),
        ));
        assert_eq!(ids(&state), ["a", "b", "c"]);
        let positions: Vec<usize> = state
            .projection
            .as_ref()
            .unwrap()
            .visible_turn_items
            .iter()
            .map(|row| row.position)
            .collect();
        assert_eq!(positions, [0, 1, 2]);
    }

    #[test]
    fn replayed_events_are_ignored() {
        let mut state = ThreadState::default();
        state.apply(snapshot(vec![], json!([]), false));
        assert!(!state.apply(event(
            10,
            "turn-item.updated",
            item("a", "user_message", 1, None),
        )));
        assert!(ids(&state).is_empty());
    }

    #[test]
    fn rolling_back_a_run_hides_its_items() {
        let mut state = ThreadState::default();
        state.apply(snapshot(
            vec![
                item("a", "user_message", 1, Some("run-1")),
                item("b", "user_message", 2, Some("run-2")),
            ],
            json!([
                { "id": "run-1", "status": "completed" },
                { "id": "run-2", "status": "completed" },
            ]),
            false,
        ));
        state.apply(event(
            11,
            "run.updated",
            json!({ "id": "run-2", "status": "rolled_back" }),
        ));
        assert_eq!(ids(&state), ["a"]);
    }

    #[test]
    fn a_bounded_window_drops_updates_to_unloaded_history() {
        let mut state = ThreadState::default();
        state.apply(snapshot(
            vec![item("c", "user_message", 5, Some("run-3"))],
            json!([
                { "id": "run-1", "status": "completed" },
                { "id": "run-3", "status": "completed" },
            ]),
            true,
        ));
        state.apply(event(
            11,
            "turn-item.updated",
            item("old", "assistant_message", 2, Some("run-1")),
        ));
        assert_eq!(ids(&state), ["c"]);

        // A new run's items are not history, even with a lower ordinal.
        state.apply(event(
            12,
            "run.created",
            json!({ "id": "run-4", "status": "queued" }),
        ));
        state.apply(event(
            13,
            "turn-item.updated",
            item("new", "user_message", 4, Some("run-4")),
        ));
        assert_eq!(ids(&state), ["new", "c"]);
    }

    #[test]
    fn timeline_skips_checkpoints_and_workspace_preparation() {
        let mut preparing = item("prep", "command_execution", 2, Some("run-1"));
        preparing["input"] = WORKSPACE_PREPARATION_INPUT.into();
        let mut command = item("cmd", "command_execution", 3, Some("run-1"));
        command["input"] = "cargo test".into();
        let mut state = ThreadState::default();
        state.apply(snapshot(
            vec![
                item("a", "user_message", 1, Some("run-1")),
                preparing,
                command,
                item("cp", "checkpoint", 4, Some("run-1")),
            ],
            json!([{ "id": "run-1", "status": "completed" }]),
            false,
        ));
        let timeline = state.timeline();
        assert_eq!(timeline.len(), 2);
        assert_eq!(
            timeline[1].content,
            TimelineContent::Work {
                label: "Ran command".into(),
                detail: Some("cargo test".into()),
                status: WorkStatus::Completed,
                thinking: false,
            }
        );
    }

    #[test]
    fn provider_retries_describe_progress_and_delay() {
        let mut error = item("e", "error", 1, Some("run-1"));
        error["status"] = "running".into();
        error["failure"] = json!({ "class": "overloaded", "message": "Overloaded." });
        error["retry"] = json!({ "attempt": 2, "maxAttempts": 5, "retryDelayMs": 1500 });
        let item: TurnItem = serde_json::from_value(error).unwrap();
        assert_eq!(
            error_label(&item),
            (
                "Retrying provider (2/5)".into(),
                Some("Overloaded. Retrying in 1.5s.".into())
            )
        );
    }

    #[test]
    fn subscribe_input_resumes_after_the_last_sequence() {
        assert_eq!(subscribe_input("t", Some(7))["afterSequence"], 7);
        assert!(subscribe_input("t", None).get("afterSequence").is_none());
    }
}
