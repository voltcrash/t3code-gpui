//! Unsubmitted answers belong to a thread and request, independently of its view.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::requests::{Draft, PendingRequests, Question};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub answers: HashMap<String, HashMap<String, Draft>>,
    pub indices: HashMap<String, usize>,
}

impl State {
    pub fn draft(&self, request: &str, question: &Question) -> Draft {
        self.answers
            .get(request)
            .and_then(|answers| answers.get(&question.id))
            .cloned()
            .unwrap_or_else(|| Draft::seeded(question))
    }

    pub fn draft_mut(&mut self, request: &str, question: &Question) -> &mut Draft {
        self.answers
            .entry(request.into())
            .or_default()
            .entry(question.id.clone())
            .or_insert_with(|| Draft::seeded(question))
    }

    pub fn prune(&mut self, pending: &PendingRequests) {
        self.answers.retain(|id, answers| {
            let Some(request) = pending
                .user_inputs
                .iter()
                .find(|request| &request.request_id == id)
            else {
                return false;
            };
            answers.retain(|id, _| request.questions.iter().any(|question| &question.id == id));
            !answers.is_empty()
        });
        self.indices.retain(|id, _| {
            pending
                .user_inputs
                .iter()
                .any(|request| &request.request_id == id)
        });
    }
}

#[derive(Default)]
pub struct Drafts {
    threads: BTreeMap<String, State>,
    touched: HashSet<String>,
    revision: u64,
}

impl Drafts {
    pub fn get(&self, thread: &str) -> State {
        self.threads.get(thread).cloned().unwrap_or_default()
    }

    pub fn edit(&mut self, thread: &str, state: State) {
        if self
            .threads
            .get(thread)
            .is_some_and(|stored| stored == &state)
            || (!self.threads.contains_key(thread) && state == State::default())
        {
            return;
        }
        self.revision += 1;
        self.touched.insert(thread.into());
        if state == State::default() {
            self.threads.remove(thread);
        } else {
            self.threads.insert(thread.into(), state);
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn snapshot(&self) -> BTreeMap<String, State> {
        self.threads.clone()
    }

    pub fn restore_missing(&mut self, saved: &BTreeMap<String, State>) {
        for (thread, state) in saved {
            if !self.touched.contains(thread) {
                self.edit(thread, state.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requests::{Capability, PendingUserInput};
    use serde_json::json;

    fn question(id: &str) -> Question {
        serde_json::from_value(json!({ "id":id,"header":"Pick","question":"Which?","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}],"multiSelect":true })).unwrap()
    }

    #[test]
    fn answers_selections_and_question_position_survive_recreation_and_restart() {
        let first = question("first");
        let second = question("second");
        let mut state = State::default();
        state.draft_mut("request", &first).toggle(&first, "A");
        state.draft_mut("request", &first).toggle(&first, "B");
        state.draft_mut("request", &second).custom = Some("Unsent answer 🌱\nSecond line".into());
        state.indices.insert("request".into(), 1);
        let mut drafts = Drafts::default();
        drafts.edit("thread", state.clone());
        assert_eq!(drafts.get("thread"), state);
        let bytes = serde_json::to_vec(&drafts.snapshot()).unwrap();
        let mut restored = Drafts::default();
        restored.restore_missing(&serde_json::from_slice(&bytes).unwrap());
        assert_eq!(restored.get("thread"), state);
        assert_eq!(restored.get("other"), State::default());
    }

    #[test]
    fn authoritative_requests_prune_resolved_requests_and_removed_questions() {
        let first = question("first");
        let gone = question("gone");
        let mut state = State::default();
        state.draft_mut("live", &first).custom = Some("Keep".into());
        state.draft_mut("live", &gone).custom = Some("Drop".into());
        state.draft_mut("resolved", &first).custom = Some("Drop".into());
        state.indices.insert("live".into(), 1);
        state.indices.insert("resolved".into(), 0);
        state.prune(&PendingRequests {
            approvals: Vec::new(),
            user_inputs: vec![PendingUserInput {
                request_id: "live".into(),
                questions: vec![first.clone()],
                capability: Capability::Message,
                dismissible: true,
            }],
        });
        assert_eq!(state.answers.len(), 1);
        assert_eq!(state.answers["live"].len(), 1);
        assert_eq!(state.draft("live", &first).custom.as_deref(), Some("Keep"));
        assert_eq!(state.indices.len(), 1);
        state.prune(&PendingRequests::default());
        assert_eq!(state, State::default());
    }

    #[test]
    fn retried_load_preserves_local_changes_and_does_not_resurrect_resolved_answers() {
        let question = question("question");
        let mut state = State::default();
        state.draft_mut("request", &question).custom = Some("Old answer".into());
        let mut drafts = Drafts::default();
        drafts.edit("edited", state.clone());
        drafts.edit("resolved", state.clone());
        drafts.edit("untouched", state.clone());
        let saved = drafts.snapshot();
        let mut recovered = Drafts::default();
        state.draft_mut("request", &question).custom = Some("Local edit".into());
        recovered.edit("edited", state.clone());
        recovered.edit("resolved", state);
        recovered.edit("resolved", State::default());
        recovered.restore_missing(&saved);
        assert_eq!(
            recovered
                .get("edited")
                .draft("request", &question)
                .custom
                .as_deref(),
            Some("Local edit")
        );
        assert_eq!(recovered.get("resolved"), State::default());
        assert_eq!(recovered.get("untouched"), saved["untouched"]);
    }
}
