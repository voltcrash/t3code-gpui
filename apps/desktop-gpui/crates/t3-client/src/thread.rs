//! One thread's detail: the projection streamed by `orchestration.subscribeThread`
//! and the timeline the chat view draws from it.
//!
//! The reducer ports `packages/client-runtime/src/state/orchestrationV2Projection.ts`
//! and the visibility rules in `packages/shared/src/orchestrationV2Timeline.ts`.
//! Timeline entries follow `deriveTimelineEntriesFromVisibleTurnItems` and
//! `projectedWorkEntry` in `apps/web/src/session-logic.ts`. Keep them in step.
//!
//! Only the collections conversation and turn review read are kept. Events for the others
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
    pub ordinal: Option<u64>,
    pub requested_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
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
pub struct ResponseCapability {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub supports_feedback: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequest {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub response_capability: ResponseCapability,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub id: String,
    pub kind: String,
    pub status: Option<String>,
    pub run_id: Option<String>,
    pub markdown: Option<String>,
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
    pub runtime_requests: Vec<RuntimeRequest>,
    pub turn_items: Vec<TurnItem>,
    pub visible_turn_items: Vec<ProjectedTurnItem>,
    #[serde(default)]
    pub provider_sessions: Vec<ProviderSession>,
    #[serde(default)]
    pub checkpoints: Vec<crate::checkpoint::Checkpoint>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSession {
    pub id: String,
    pub provider_instance_id: String,
    pub driver: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSettings {
    pub model_selection: Value,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub started: bool,
    pub has_runtime: bool,
    pub provider_driver: Option<String>,
}

impl ThreadProjection {
    fn thread_id(&self) -> Option<&str> {
        self.thread.get("id").and_then(Value::as_str)
    }
    pub fn settings(&self) -> Option<ThreadSettings> {
        let has_runtime =
            !self.runs.is_empty() || self.thread["activeProviderThreadId"].is_string();
        let mut model_selection = self.thread.get("modelSelection")?.clone();
        let runtime_instance = self.thread["providerInstanceId"]
            .as_str()
            .filter(|_| has_runtime);
        if let Some(instance) = runtime_instance {
            model_selection["instanceId"] = instance.into();
        }
        Some(ThreadSettings {
            model_selection,
            runtime_mode: self.thread["runtimeMode"].as_str()?.to_owned(),
            interaction_mode: self.thread["interactionMode"]
                .as_str()
                .unwrap_or("default")
                .to_owned(),
            started: has_runtime || !self.visible_turn_items.is_empty(),
            has_runtime,
            // A removed instance still has a session; keep its driver lock.
            provider_driver: self
                .provider_sessions
                .iter()
                .rev()
                .find(|session| Some(session.provider_instance_id.as_str()) == runtime_instance)
                .map(|session| session.driver.clone()),
        })
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
        history_cursor: Option<String>,
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
    pub history_cursor: Option<String>,
    // Even an identical cursor must not let an in-flight page overwrite a new snapshot.
    snapshot_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRequest {
    pub cursor: String,
    generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub snapshot_sequence: u64,
    pub items: Vec<ProjectedTurnItem>,
    pub next_cursor: Option<String>,
    pub has_more_history: bool,
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
                history_cursor,
                has_more_history,
                latest_local_turn_ordinal,
            } => {
                self.sequence = Some(snapshot_sequence);
                self.projection = Some(*projection);
                self.partial = has_more_history;
                self.latest_local_turn_ordinal = latest_local_turn_ordinal;
                self.history_cursor = history_cursor;
                self.snapshot_generation += 1;
                self.synchronized = false;
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

    pub fn history_request(&self) -> Option<HistoryRequest> {
        if !self.synchronized || !self.partial || self.projection.is_none() {
            return None;
        }
        Some(HistoryRequest {
            cursor: self.history_cursor.clone()?,
            generation: self.snapshot_generation,
        })
    }

    pub fn is_current_history_request(&self, request: &HistoryRequest) -> bool {
        request.generation == self.snapshot_generation
            && self.history_cursor.as_deref() == Some(request.cursor.as_str())
    }

    /// Pages extend the loaded window. They never advance the live stream's resume sequence.
    pub fn apply_history(&mut self, request: &HistoryRequest, page: HistoryPage) -> bool {
        if !self.is_current_history_request(request) {
            return false;
        }
        let Some(projection) = &mut self.projection else {
            return false;
        };
        merge_history(projection, page.items);
        self.history_cursor = page.next_cursor;
        self.partial = page.has_more_history;
        true
    }
}

/// Matches `mergeOlderHistoryIntoProjection`: current rows win over overlapping pages,
/// and local items removed by live events must not be resurrected by an older response.
fn merge_history(projection: &mut ThreadProjection, older: Vec<ProjectedTurnItem>) {
    let current: HashMap<_, _> = projection
        .visible_turn_items
        .iter()
        .map(|row| {
            (
                (row.source_thread_id.clone(), row.source_item_id.clone()),
                row,
            )
        })
        .collect();
    let items: HashMap<_, _> = projection
        .turn_items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let visibility = VisibilityRules::new(
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    );
    let mut added = Vec::new();
    let mut added_ids = HashSet::new();
    let mut keys = HashSet::new();
    let mut rows = Vec::with_capacity(older.len() + current.len());
    for mut row in older {
        let key = (row.source_thread_id.clone(), row.source_item_id.clone());
        if keys.contains(&key) {
            continue;
        }
        let local = row.visibility == Visibility::Local
            || Some(row.source_thread_id.as_str()) == projection.thread_id();
        if let Some(current) = current.get(&key) {
            row = (*current).clone();
        } else if local {
            if let Some(item) = items.get(row.source_item_id.as_str()) {
                if item.kind != "run_interrupt_request" || !visibility.is_visible(item) {
                    continue;
                }
                row.item = (*item).clone();
            }
            if !visibility.is_visible(&row.item) {
                continue;
            }
        }
        if local
            && !items.contains_key(row.source_item_id.as_str())
            && added_ids.insert(row.source_item_id.clone())
        {
            added.push(row.item.clone());
        }
        keys.insert(key);
        rows.push(row);
    }
    if rows.is_empty() {
        return;
    }
    rows.extend(
        projection
            .visible_turn_items
            .iter()
            .filter(|row| {
                !keys.contains(&(row.source_thread_id.clone(), row.source_item_id.clone()))
            })
            .cloned(),
    );
    renumber(&mut rows);
    projection.turn_items.extend(added);
    projection.visible_turn_items = rows;
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
        "provider-session.attached" | "provider-session.updated" => {
            let Ok(session) = serde_json::from_value::<ProviderSession>(event.payload) else {
                return false;
            };
            upsert(&mut projection.provider_sessions, session, |session| {
                &session.id
            });
            true
        }
        "provider-session.detached" => {
            projection.provider_sessions.retain(|session| {
                Some(session.id.as_str()) != event.payload["providerSessionId"].as_str()
            });
            true
        }
        "runtime-request.updated" => {
            let Ok(request) = serde_json::from_value::<RuntimeRequest>(event.payload) else {
                return false;
            };
            upsert(&mut projection.runtime_requests, request, |request| {
                &request.id
            });
            true
        }
        "checkpoint.captured" => {
            let Ok(checkpoint) =
                serde_json::from_value::<crate::checkpoint::Checkpoint>(event.payload)
            else {
                return false;
            };
            upsert(&mut projection.checkpoints, checkpoint, |checkpoint| {
                &checkpoint.id
            });
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
        attachments: Vec<crate::attachments::Attachment>,
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
            attachments: item
                .fields
                .get("attachments")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default(),
        },
        "assistant_message" => TimelineContent::Message {
            role: Role::Assistant,
            text: text(),
            streaming: item.fields.get("streaming") == Some(&Value::Bool(true)),
            attachments: Vec::new(),
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
                "runtimeRequests": [],
                "messages": [],
                "turnItems": items,
                "visibleTurnItems": rows,
                "updatedAt": "2026-10-24T10:00:00.000Z",
            },
        }))
        .unwrap()
    }

    #[test]
    fn composer_settings_follow_the_runtime_instance_even_when_the_catalog_loses_it() {
        let mut state = ThreadState::default();
        state.apply(snapshot(vec![], json!([]), false));
        let projection = state.projection.as_mut().unwrap();
        projection.thread = json!({
            "id": THREAD,
            "modelSelection": { "instanceId": "codex", "model": "gpt" },
            "runtimeMode": "auto",
            "providerInstanceId": "work",
            "activeProviderThreadId": null,
        });
        let fresh = projection.settings().unwrap();
        assert!(!fresh.started);
        assert!(!fresh.has_runtime);
        assert_eq!(fresh.model_selection["instanceId"], "codex");
        projection.thread["activeProviderThreadId"] = "native-thread".into();
        state.apply(event(
            11,
            "provider-session.attached",
            json!({
                "id": "session", "providerInstanceId": "work", "driver": "codex",
            }),
        ));
        let started = state.projection.as_ref().unwrap().settings().unwrap();
        assert!(started.started);
        assert!(started.has_runtime);
        assert_eq!(started.model_selection["instanceId"], "work");
        assert_eq!(started.provider_driver.as_deref(), Some("codex"));
        state.apply(event(
            12,
            "provider-session.detached",
            json!({ "providerSessionId": "session" }),
        ));
        assert!(
            state
                .projection
                .as_ref()
                .unwrap()
                .settings()
                .unwrap()
                .provider_driver
                .is_none()
        );
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
                attachments: Vec::new(),
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
    fn checkpoint_capture_updates_replace_by_id_and_snapshot_replaces_removed_turns() {
        let mut initial = snapshot(vec![], json!([]), false);
        let checkpoint = json!({"id":"checkpoint", "runId":"run", "appRunOrdinal":2,
            "status":"ready", "capturedAt":"2026-10-26"});
        if let ThreadStreamItem::Snapshot { projection, .. } = &mut initial {
            projection.checkpoints = serde_json::from_value(json!([checkpoint])).unwrap();
        }
        let mut state = ThreadState::default();
        state.apply(initial);
        assert_eq!(
            crate::checkpoint::turns(&state.projection.as_ref().unwrap().checkpoints)[0].count,
            2
        );
        let mut stale = checkpoint;
        stale["status"] = "stale".into();
        assert!(state.apply(event(11, "checkpoint.captured", stale.clone())));
        let checkpoints = &state.projection.as_ref().unwrap().checkpoints;
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].status, "stale");
        stale["status"] = "ready".into();
        assert!(!state.apply(event(10, "checkpoint.captured", stale)));
        assert_eq!(
            state.projection.as_ref().unwrap().checkpoints[0].status,
            "stale"
        );
        state.apply(snapshot(vec![], json!([]), false));
        assert!(state.projection.as_ref().unwrap().checkpoints.is_empty());
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

    fn history_snapshot(items: Vec<Value>, cursor: &str) -> ThreadStreamItem {
        let mut snapshot = snapshot(items, json!([]), true);
        if let ThreadStreamItem::Snapshot { history_cursor, .. } = &mut snapshot {
            *history_cursor = Some(cursor.into());
        }
        snapshot
    }

    fn page(items: Vec<Value>, next: Option<&str>) -> HistoryPage {
        serde_json::from_value(json!({
            "snapshotSequence": 99,
            "items": items,
            "nextCursor": next,
            "hasMoreHistory": next.is_some(),
        }))
        .unwrap()
    }

    #[test]
    fn older_pages_preserve_live_text_and_resume_sequence() {
        let mut state = ThreadState::default();
        state.apply(history_snapshot(
            vec![item("c", "assistant_message", 3, None)],
            "opaque+cursor",
        ));
        assert!(
            state.history_request().is_none(),
            "wait for replay completion"
        );
        state.apply(ThreadStreamItem::Synchronized);
        let request = state.history_request().unwrap();
        let mut live = item("c", "assistant_message", 3, None);
        live["text"] = "live text".into();
        state.apply(event(11, "turn-item.updated", live));
        assert!(state.apply_history(
            &request,
            page(
                vec![
                    row(0, item("a", "user_message", 1, None)),
                    row(1, item("a", "user_message", 1, None)),
                    row(2, item("b", "assistant_message", 2, None)),
                    row(3, item("c", "assistant_message", 3, None)),
                ],
                Some("next")
            )
        ));
        assert_eq!(ids(&state), ["a", "b", "c"]);
        assert_eq!(state.sequence, Some(11));
        assert_eq!(state.history_cursor.as_deref(), Some("next"));
        assert!(state.partial);
        assert!(
            matches!(&state.timeline()[2].content, TimelineContent::Message { text, .. } if text == "live text")
        );
        let mut updated = item("a", "user_message", 1, None);
        updated["text"] = "updated older message".into();
        state.apply(event(12, "turn-item.updated", updated));
        assert_eq!(ids(&state), ["a", "b", "c"]);
        assert!(
            matches!(&state.timeline()[0].content, TimelineContent::Message { text, .. } if text == "updated older message")
        );
        let last = state.history_request().unwrap();
        assert!(state.apply_history(&last, page(vec![], None)));
        assert!(!state.partial);
        assert!(state.history_request().is_none());
    }

    #[test]
    fn replacement_snapshots_reject_old_pages_even_with_the_same_cursor() {
        let mut state = ThreadState::default();
        state.apply(history_snapshot(
            vec![item("c", "user_message", 3, None)],
            "same",
        ));
        state.apply(ThreadStreamItem::Synchronized);
        let request = state.history_request().unwrap();
        state.apply(history_snapshot(
            vec![item("d", "user_message", 4, None)],
            "same",
        ));
        state.apply(ThreadStreamItem::Synchronized);
        assert!(!state.is_current_history_request(&request));
        assert!(!state.apply_history(
            &request,
            page(vec![row(0, item("a", "user_message", 1, None))], None)
        ));
        assert_eq!(ids(&state), ["d"]);
        assert_eq!(state.history_cursor.as_deref(), Some("same"));
    }

    #[test]
    fn history_identity_includes_the_source_thread_and_visibility() {
        let mut state = ThreadState::default();
        state.apply(history_snapshot(
            vec![item("same", "user_message", 2, None)],
            "first",
        ));
        state.apply(ThreadStreamItem::Synchronized);
        let request = state.history_request().unwrap();
        let mut inherited = row(0, item("same", "user_message", 1, None));
        inherited["visibility"] = "inherited".into();
        inherited["sourceThreadId"] = "parent".into();
        inherited["item"]["threadId"] = "parent".into();
        assert!(state.apply_history(&request, page(vec![inherited], None)));
        let projection = state.projection.unwrap();
        assert_eq!(projection.visible_turn_items.len(), 2);
        assert_eq!(projection.visible_turn_items[0].source_thread_id, "parent");
        assert_eq!(projection.visible_turn_items[0].position, 0);
        assert_eq!(projection.visible_turn_items[1].position, 1);
        assert_eq!(
            projection.turn_items.len(),
            1,
            "inherited items stay out of local state"
        );
    }

    #[test]
    fn history_does_not_resurrect_items_hidden_by_live_run_changes() {
        let mut state = ThreadState::default();
        state.apply(history_snapshot(
            vec![item("c", "user_message", 3, None)],
            "first",
        ));
        state.apply(ThreadStreamItem::Synchronized);
        let request = state.history_request().unwrap();
        state.apply(event(
            11,
            "run.updated",
            json!({ "id": "old", "status": "rolled_back" }),
        ));
        assert!(state.apply_history(
            &request,
            page(
                vec![row(0, item("a", "user_message", 1, Some("old")))],
                None
            )
        ));
        assert_eq!(ids(&state), ["c"]);
        assert!(!state.partial);
    }

    #[test]
    fn overlapping_pages_keep_hidden_items_hidden_and_restore_retained_interrupt_requests() {
        let mut state = ThreadState::default();
        state.apply(history_snapshot(
            vec![item("c", "user_message", 3, None)],
            "first",
        ));
        state.apply(ThreadStreamItem::Synchronized);
        let request = state.history_request().unwrap();
        let hidden = item("hidden", "user_message", 1, None);
        let interrupt = item("interrupt", "run_interrupt_request", 2, None);
        // Bounded snapshots retain some control items without a visible row.
        state.projection.as_mut().unwrap().turn_items.extend([
            serde_json::from_value(hidden.clone()).unwrap(),
            serde_json::from_value(interrupt.clone()).unwrap(),
        ]);
        assert!(state.apply_history(
            &request,
            page(vec![row(0, hidden), row(1, interrupt)], None)
        ));
        assert_eq!(ids(&state), ["interrupt", "c"]);
        assert_eq!(state.projection.as_ref().unwrap().turn_items.len(), 3);
    }
}
