//! Approvals and questions an agent is blocked on, and the answers to them.
//!
//! Ports `derivePendingThreadRequests` from
//! `packages/client-runtime/src/state/threadRequests.ts` and the answer rules
//! in `apps/web/src/pendingUserInput.ts`. Keep them in step.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::thread::{ThreadProjection, TurnItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Live,
    /// Answered by a message rather than by the provider process.
    Message,
    /// The provider process is gone.
    NotResumable,
}

impl Capability {
    fn parse(kind: &str) -> Self {
        match kind {
            "live" => Self::Live,
            "message" => Self::Message,
            _ => Self::NotResumable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ApprovalOption {
    pub decision: String,
    pub label: String,
    pub warning: Option<String>,
}

/// `DEFAULT_APPROVAL_OPTIONS` in `ComposerPendingApprovalActions.tsx`.
pub fn default_approval_options() -> Vec<ApprovalOption> {
    [
        ("cancel", "Cancel"),
        ("decline", "Decline"),
        ("acceptForSession", "Always allow this session"),
        ("accept", "Approve"),
    ]
    .into_iter()
    .map(|(decision, label)| ApprovalOption {
        decision: decision.into(),
        label: label.into(),
        warning: None,
    })
    .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingApproval {
    pub request_id: String,
    pub request_kind: String,
    pub detail: Option<String>,
    pub reason: Option<String>,
    pub full_access_override: Option<String>,
    pub app_name: Option<String>,
    pub options: Vec<ApprovalOption>,
    /// Live or not; message-mode approvals do not exist.
    pub live: bool,
    pub supports_feedback: bool,
}

impl PendingApproval {
    /// The panel heading in `ComposerPendingApprovalPanel.tsx`.
    pub fn label(&self) -> &'static str {
        match self.request_kind.as_str() {
            "mcp-elicitation" => "App access approval",
            "command" => "Command approval",
            "file-read" => "File read approval",
            "permission" => "App permission approval",
            _ => "File change approval",
        }
    }

    /// Code-like requests show their detail in monospace.
    pub fn detail_is_code(&self) -> bool {
        self.request_kind != "mcp-elicitation"
    }
}

/// `fullAccessOverrideExplanation`.
pub fn full_access_override_explanation(override_kind: &str) -> &'static str {
    if override_kind == "ask_rule" {
        "Full access skips approvals, but an ask rule in the agent's own settings matches this action, so it still asks. Remove or narrow that rule to stop these prompts."
    } else {
        "Full access skips approvals, but the agent still stops for a few safety checks of its own that no setting can skip, such as a delete whose path it can't verify. T3 Code can't turn these off, so check the request and approve it if it's what you expect."
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
    pub value: Option<String>,
}

impl QuestionOption {
    /// What an answer carries for this option.
    pub fn answer_value(&self) -> &str {
        self.value.as_deref().unwrap_or(&self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    pub allow_custom_answer: Option<bool>,
    pub initial_answer: Option<String>,
}

impl Question {
    pub fn allows_custom_answer(&self) -> bool {
        self.allow_custom_answer != Some(false)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingUserInput {
    pub request_id: String,
    pub questions: Vec<Question>,
    pub capability: Capability,
    pub dismissible: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PendingRequests {
    pub approvals: Vec<PendingApproval>,
    pub user_inputs: Vec<PendingUserInput>,
}

impl PendingRequests {
    pub fn is_empty(&self) -> bool {
        self.approvals.is_empty() && self.user_inputs.is_empty()
    }
}

fn request_item<'a>(
    projection: &'a ThreadProjection,
    kind: &str,
    request_id: &str,
) -> Option<&'a TurnItem> {
    projection
        .turn_items
        .iter()
        .rev()
        .find(|item| item.kind == kind && item.str("requestId") == Some(request_id))
}

fn nonempty(item: Option<&TurnItem>, field: &str) -> Option<String> {
    item.and_then(|item| item.str(field))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// `derivePendingThreadRequests`.
pub fn pending_requests(projection: &ThreadProjection) -> PendingRequests {
    let mut pending = PendingRequests::default();
    for request in &projection.runtime_requests {
        if request.status != "pending" {
            continue;
        }
        let capability = Capability::parse(&request.response_capability.kind);
        if request.kind == "user_input" {
            let Some(item) = request_item(projection, "user_input_request", &request.id) else {
                continue;
            };
            let questions = item
                .fields
                .get("questions")
                .cloned()
                .and_then(|questions| serde_json::from_value(questions).ok())
                .unwrap_or_default();
            pending.user_inputs.push(PendingUserInput {
                request_id: request.id.clone(),
                questions,
                capability,
                dismissible: item.str("responseMode") == Some("message")
                    || capability == Capability::Message,
            });
            continue;
        }
        if request.kind == "auth_refresh" || request.kind == "dynamic_tool_call" {
            continue;
        }
        let item = request_item(projection, "approval_request", &request.id);
        pending.approvals.push(PendingApproval {
            request_id: request.id.clone(),
            request_kind: request.kind.clone(),
            detail: nonempty(item, "prompt"),
            reason: nonempty(item, "reason"),
            full_access_override: nonempty(item, "fullAccessOverride"),
            app_name: nonempty(item, "appName"),
            options: item
                .and_then(|item| item.fields.get("options").cloned())
                .and_then(|options| serde_json::from_value(options).ok())
                .unwrap_or_else(default_approval_options),
            live: capability == Capability::Live,
            supports_feedback: capability == Capability::Live
                && request.response_capability.supports_feedback,
        });
    }
    pending
}

/// What the person has picked or typed for one question so far.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    pub selected: Vec<String>,
    pub custom: Option<String>,
}

impl Draft {
    /// Seeds a custom answer from `initialAnswer`, like `seedUserInputDraftAnswers`.
    pub fn seeded(question: &Question) -> Self {
        Self {
            selected: Vec::new(),
            custom: question
                .initial_answer
                .clone()
                .filter(|_| question.allows_custom_answer()),
        }
    }

    /// Picks an option. Choosing one clears a typed answer, which would
    /// otherwise win.
    pub fn toggle(&mut self, question: &Question, value: &str) {
        if question.multi_select {
            match self.selected.iter().position(|selected| selected == value) {
                Some(index) => {
                    self.selected.remove(index);
                }
                None => self.selected.push(value.to_owned()),
            }
        } else {
            self.selected = vec![value.to_owned()];
        }
        self.custom = None;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    One(String),
    Many(Vec<String>),
}

impl Answer {
    fn to_value(&self) -> Value {
        match self {
            Answer::One(answer) => json!(answer),
            Answer::Many(answers) => json!(answers),
        }
    }
}

/// `resolvePendingUserInputAnswer`, without attachments.
pub fn resolve_answer(question: &Question, draft: Option<&Draft>) -> Option<Answer> {
    let custom = if !question.allows_custom_answer() {
        None
    } else if question.initial_answer.is_some() {
        draft.and_then(|draft| draft.custom.clone())
    } else {
        draft
            .and_then(|draft| draft.custom.as_deref())
            .map(str::trim)
            .filter(|custom| !custom.is_empty())
            .map(str::to_owned)
    };
    if let Some(custom) = custom.clone().filter(|custom| !custom.is_empty()) {
        return Some(Answer::One(custom));
    }
    let mut selected: Vec<String> = Vec::new();
    for value in draft.map(|draft| draft.selected.as_slice()).unwrap_or(&[]) {
        if !selected.contains(value)
            && question
                .options
                .iter()
                .any(|option| option.answer_value() == value)
        {
            selected.push(value.clone());
        }
    }
    if question.multi_select {
        return if selected.is_empty() {
            custom.map(Answer::One)
        } else {
            Some(Answer::Many(selected))
        };
    }
    selected.into_iter().next().or(custom).map(Answer::One)
}

/// `buildPendingUserInputAnswers`: every question answered, or nothing.
pub fn build_answers(
    questions: &[Question],
    drafts: &HashMap<String, Draft>,
) -> Option<Map<String, Value>> {
    questions
        .iter()
        .map(|question| {
            resolve_answer(question, drafts.get(&question.id))
                .map(|answer| (question.id.clone(), answer.to_value()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::ThreadStreamItem;

    fn projection(requests: Value, items: Value) -> ThreadProjection {
        let item = serde_json::from_value::<ThreadStreamItem>(json!({
            "kind": "snapshot",
            "snapshotSequence": 1,
            "projection": {
                "thread": { "id": "t" },
                "runs": [],
                "attempts": [],
                "plans": [],
                "runtimeRequests": requests,
                "turnItems": items,
                "visibleTurnItems": [],
            },
        }))
        .unwrap();
        match item {
            ThreadStreamItem::Snapshot { projection, .. } => *projection,
            _ => unreachable!(),
        }
    }

    fn request(id: &str, kind: &str, status: &str, capability: Value) -> Value {
        json!({
            "id": id,
            "nodeId": "node",
            "kind": kind,
            "status": status,
            "responseCapability": capability,
            "createdAt": "2026-10-24T10:00:00.000Z",
        })
    }

    fn item(kind: &str, request_id: &str, extra: Value) -> Value {
        let mut item = json!({
            "id": format!("item-{request_id}"),
            "threadId": "t",
            "runId": "run",
            "nodeId": "node",
            "ordinal": 1,
            "status": "waiting",
            "type": kind,
            "updatedAt": "2026-10-24T10:00:00.000Z",
            "requestId": request_id,
        });
        item.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        item
    }

    fn question(multi_select: bool) -> Question {
        serde_json::from_value(json!({
            "id": "q",
            "header": "Pick",
            "question": "Which?",
            "multiSelect": multi_select,
            "options": [
                { "label": "A", "description": "first" },
                { "label": "B", "description": "second", "value": "b-id" },
            ],
        }))
        .unwrap()
    }

    #[test]
    fn approvals_carry_their_prompt_and_default_options() {
        let live = json!({ "type": "live", "providerSessionId": "s", "supportsFeedback": true });
        let projection = projection(
            json!([
                request("r1", "command", "pending", live.clone()),
                request("r2", "command", "resolved", live.clone()),
                request("r3", "auth_refresh", "pending", live),
            ]),
            json!([item(
                "approval_request",
                "r1",
                json!({ "prompt": "rm -rf build" })
            )]),
        );
        let pending = pending_requests(&projection);
        assert_eq!(pending.approvals.len(), 1);
        let approval = &pending.approvals[0];
        assert_eq!(approval.detail.as_deref(), Some("rm -rf build"));
        assert_eq!(approval.label(), "Command approval");
        assert!(approval.live && approval.supports_feedback);
        assert_eq!(approval.options, default_approval_options());
    }

    #[test]
    fn questions_need_their_turn_item_and_message_mode_is_dismissible() {
        let projection = projection(
            json!([
                request("q1", "user_input", "pending", json!({ "type": "message" })),
                request("q2", "user_input", "pending", json!({ "type": "message" })),
            ]),
            json!([item(
                "user_input_request",
                "q1",
                json!({ "questions": [{
                    "id": "q", "header": "H", "question": "Q?", "options": [],
                }] }),
            )]),
        );
        let pending = pending_requests(&projection);
        assert_eq!(pending.user_inputs.len(), 1);
        assert!(pending.user_inputs[0].dismissible);
        assert!(!pending.user_inputs[0].questions[0].multi_select);
    }

    #[test]
    fn a_typed_answer_wins_over_a_selection() {
        let question = question(false);
        let draft = Draft {
            selected: vec!["A".into()],
            custom: Some("  my own  ".into()),
        };
        assert_eq!(
            resolve_answer(&question, Some(&draft)),
            Some(Answer::One("my own".into()))
        );
    }

    #[test]
    fn selections_use_option_values_and_ignore_stale_ones() {
        let question = question(true);
        let mut draft = Draft::default();
        draft.toggle(&question, "b-id");
        draft.toggle(&question, "A");
        draft.selected.push("gone".into());
        assert_eq!(
            resolve_answer(&question, Some(&draft)),
            Some(Answer::Many(vec!["b-id".into(), "A".into()]))
        );
        draft.toggle(&question, "A");
        assert_eq!(
            resolve_answer(&question, Some(&draft)),
            Some(Answer::Many(vec!["b-id".into()]))
        );
    }

    #[test]
    fn answers_need_every_question() {
        let first = question(false);
        let mut second = question(false);
        second.id = "q2".into();
        let mut drafts = HashMap::new();
        drafts.insert(
            "q".to_owned(),
            Draft {
                selected: vec!["A".into()],
                custom: None,
            },
        );
        assert!(build_answers(&[first.clone(), second.clone()], &drafts).is_none());
        drafts.insert(
            "q2".to_owned(),
            Draft {
                selected: vec![],
                custom: Some("typed".into()),
            },
        );
        let answers = build_answers(&[first, second], &drafts).unwrap();
        assert_eq!(answers["q"], "A");
        assert_eq!(answers["q2"], "typed");
    }

    #[test]
    fn a_choice_only_question_ignores_typed_text() {
        let mut question = question(false);
        question.allow_custom_answer = Some(false);
        let draft = Draft {
            selected: vec![],
            custom: Some("typed".into()),
        };
        assert_eq!(resolve_answer(&question, Some(&draft)), None);
    }
}
