//! Message text outlives its view, but neither transcripts nor sockets are retained.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Picks {
    pub model: Option<Value>,
    pub runtime_mode: Option<String>,
    pub interaction_mode: Option<String>,
}

#[derive(Default)]
pub struct Drafts {
    revision: u64,
    text: HashMap<String, (u64, String)>,
    pending: HashMap<String, u64>,
    touched: HashSet<String>,
    picks: HashMap<String, (u64, Picks)>,
    picks_touched: HashSet<String>,
}

#[derive(Clone)]
pub struct Submission {
    thread_id: String,
    operation: u64,
    clear_revision: Option<u64>,
    picks_revision: Option<u64>,
}

impl Drafts {
    pub fn text(&self, thread_id: &str) -> &str {
        self.text
            .get(thread_id)
            .map(|(_, text)| text.as_str())
            .unwrap_or_default()
    }

    pub fn edit(&mut self, thread_id: &str, text: &str) {
        if self.text(thread_id) == text {
            return;
        }
        self.revision += 1;
        self.touched.insert(thread_id.to_owned());
        if text.is_empty() {
            self.text.remove(thread_id);
        } else {
            self.text
                .insert(thread_id.into(), (self.revision, text.into()));
        }
    }

    pub fn picks(&self, id: &str) -> Option<&Picks> {
        self.picks.get(id).map(|(_, picks)| picks)
    }

    pub fn set_picks(&mut self, id: &str, picks: Picks) -> bool {
        if self.picks(id).cloned().unwrap_or_default() == picks {
            return false;
        }
        self.revision += 1;
        self.picks_touched.insert(id.into());
        if picks == Picks::default() {
            self.picks.remove(id);
        } else {
            self.picks.insert(id.into(), (self.revision, picks));
        }
        true
    }

    pub fn picks_snapshot(&self) -> BTreeMap<String, Picks> {
        self.picks
            .iter()
            .map(|(id, (_, picks))| (id.clone(), picks.clone()))
            .collect()
    }

    pub fn restore_picks_missing(&mut self, saved: &BTreeMap<String, Picks>) {
        for (id, picks) in saved {
            if !self.picks_touched.contains(id) {
                self.set_picks(id, picks.clone());
            }
        }
    }

    pub fn picks_unchanged(&self, submission: &Submission) -> bool {
        self.picks
            .get(&submission.thread_id)
            .map(|(revision, _)| *revision)
            == submission.picks_revision
    }

    pub fn clear_submitted_interaction(&mut self, submission: &Submission) {
        if self.picks_unchanged(submission)
            && let Some(mut picks) = self.picks(&submission.thread_id).cloned()
            && picks.interaction_mode.take().is_some()
        {
            self.set_picks(&submission.thread_id, picks);
        }
    }

    pub fn snapshot(&self) -> BTreeMap<String, String> {
        self.text
            .iter()
            .map(|(id, (_, text))| (id.clone(), text.clone()))
            .collect()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn restore_missing(&mut self, saved: &BTreeMap<String, String>) {
        for (id, text) in saved {
            // A failed load can be retried after typing. Explicit clears must
            // remain empty instead of resurrecting an older saved draft.
            if !self.touched.contains(id) {
                self.edit(id, text);
            }
        }
    }

    pub fn busy(&self, thread_id: &str) -> bool {
        self.pending.contains_key(thread_id)
    }

    pub fn begin(&mut self, thread_id: &str, clears_text: bool) -> Option<Submission> {
        if self.busy(thread_id) {
            return None;
        }
        self.revision += 1;
        let operation = self.revision;
        self.pending.insert(thread_id.into(), operation);
        Some(Submission {
            thread_id: thread_id.into(),
            operation,
            picks_revision: self.picks.get(thread_id).map(|(revision, _)| *revision),
            clear_revision: clears_text
                .then(|| self.text.get(thread_id).map(|(revision, _)| *revision))
                .flatten(),
        })
    }

    pub fn finish(&mut self, submission: Submission, accepted: bool) {
        if self.pending.get(&submission.thread_id) != Some(&submission.operation) {
            return;
        }
        self.pending.remove(&submission.thread_id);
        if accepted
            && submission.clear_revision.is_some()
            && self
                .text
                .get(&submission.thread_id)
                .map(|(revision, _)| *revision)
                == submission.clear_revision
        {
            self.text.remove(&submission.thread_id);
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_threads_restores_each_unsent_message_without_transcripts() {
        let mut drafts = Drafts::default();
        drafts.edit("first", "Unsent first message");
        drafts.edit("second", "A different draft\nwith another line");
        assert_eq!(drafts.text("first"), "Unsent first message");
        assert_eq!(
            drafts.text("second"),
            "A different draft\nwith another line"
        );
        drafts.edit("second", "");
        assert_eq!(drafts.text("second"), "");
        assert_eq!(drafts.text("first"), "Unsent first message");
    }

    #[test]
    fn acceptance_after_navigation_clears_only_the_submitted_draft() {
        let mut drafts = Drafts::default();
        drafts.edit("first", "Send this");
        let submitted = drafts.begin("first", true).unwrap();
        assert!(drafts.busy("first"));
        assert!(drafts.begin("first", true).is_none());
        drafts.edit("second", "Keep this");
        drafts.finish(submitted, true);
        assert!(!drafts.busy("first"));
        assert_eq!(drafts.text("first"), "");
        assert_eq!(drafts.text("second"), "Keep this");
    }

    #[test]
    fn failed_sends_keep_the_text_and_release_the_thread_for_retry() {
        let mut drafts = Drafts::default();
        drafts.edit("thread", "Try again");
        let submission = drafts.begin("thread", true).unwrap();
        drafts.finish(submission, false);
        assert_eq!(drafts.text("thread"), "Try again");
        assert!(drafts.begin("thread", true).is_some());
    }

    #[test]
    fn a_completion_never_erases_a_newer_edit_or_a_draft_while_answering_a_question() {
        let mut drafts = Drafts::default();
        drafts.edit("thread", "Old message");
        let submission = drafts.begin("thread", true).unwrap();
        drafts.edit("thread", "");
        drafts.edit("thread", "Old message");
        drafts.finish(submission, true);
        assert_eq!(drafts.text("thread"), "Old message");
        let answer = drafts.begin("thread", false).unwrap();
        drafts.finish(answer, true);
        assert_eq!(drafts.text("thread"), "Old message");
    }

    #[test]
    fn pending_choices_restore_without_text_and_retry_keeps_new_picks_and_resets() {
        let picks = Picks {
            model: Some(
                serde_json::json!({"instanceId":"codex", "model":"chosen", "options":{"effort":"high"}}),
            ),
            runtime_mode: Some("approval-required".into()),
            interaction_mode: Some("plan".into()),
        };
        let mut drafts = Drafts::default();
        drafts.set_picks("thread", picks.clone());
        assert!(drafts.snapshot().is_empty());
        let saved = drafts.picks_snapshot();
        drafts.set_picks("thread", Picks::default());
        drafts.restore_picks_missing(&saved);
        assert!(drafts.picks("thread").is_none());
        let mut restored = Drafts::default();
        restored.restore_picks_missing(&saved);
        assert_eq!(restored.picks("thread"), Some(&picks));
        restored.set_picks(
            "thread",
            Picks {
                runtime_mode: Some("auto".into()),
                ..Default::default()
            },
        );
        restored.restore_picks_missing(&saved);
        assert_eq!(
            restored.picks("thread").unwrap().runtime_mode.as_deref(),
            Some("auto")
        );
    }

    #[test]
    fn acceptance_clears_only_the_submitted_plan_choice_and_preserves_other_options() {
        let mut drafts = Drafts::default();
        let picks = Picks {
            runtime_mode: Some("approval-required".into()),
            interaction_mode: Some("plan".into()),
            ..Default::default()
        };
        drafts.set_picks("thread", picks.clone());
        let submitted = drafts.begin("thread", true).unwrap();
        drafts.finish(submitted.clone(), true);
        drafts.clear_submitted_interaction(&submitted);
        let restored = drafts.picks("thread").unwrap();
        assert_eq!(restored.interaction_mode, None);
        assert_eq!(restored.runtime_mode, picks.runtime_mode);
        drafts.set_picks("thread", picks.clone());
        let submitted = drafts.begin("thread", true).unwrap();
        drafts.set_picks(
            "thread",
            Picks {
                runtime_mode: Some("auto".into()),
                ..picks.clone()
            },
        );
        drafts.set_picks("thread", picks.clone());
        drafts.finish(submitted.clone(), true);
        drafts.clear_submitted_interaction(&submitted);
        assert_eq!(drafts.picks("thread"), Some(&picks));
    }

    #[test]
    fn retrying_a_load_keeps_new_text_and_explicit_clears() {
        let mut drafts = Drafts::default();
        drafts.edit("edited", "New local text");
        drafts.edit("cleared", "Delete this");
        drafts.edit("cleared", "");
        drafts.restore_missing(&BTreeMap::from([
            ("edited".into(), "Old text".into()),
            ("cleared".into(), "Old text".into()),
            ("untouched".into(), "Recovered text 🌱\nSecond line".into()),
        ]));
        assert_eq!(drafts.text("edited"), "New local text");
        assert_eq!(drafts.text("cleared"), "");
        assert_eq!(drafts.text("untouched"), "Recovered text 🌱\nSecond line");
        assert!(!drafts.snapshot().contains_key("cleared"));
    }

    #[test]
    fn snapshots_restore_text_without_restoring_inflight_commands() {
        let mut drafts = Drafts::default();
        drafts.edit("thread", "Unsent text");
        let _submission = drafts.begin("thread", true).unwrap();
        let mut restored = Drafts::default();
        restored.restore_missing(&drafts.snapshot());
        assert_eq!(restored.text("thread"), "Unsent text");
        assert!(!restored.busy("thread"));
    }

    #[test]
    fn acceptance_after_saving_an_inflight_draft_marks_the_cleared_text_for_saving() {
        let mut drafts = Drafts::default();
        drafts.edit("thread", "Submitted text");
        let submission = drafts.begin("thread", true).unwrap();
        let saved_revision = drafts.revision();
        drafts.finish(submission, true);
        assert!(drafts.snapshot().is_empty());
        assert!(drafts.revision() > saved_revision);
    }
}
