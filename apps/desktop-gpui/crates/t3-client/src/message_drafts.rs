//! Message text outlives its view, but neither transcripts nor sockets are retained.

use std::collections::HashMap;

#[derive(Default)]
pub struct Drafts {
    revision: u64,
    text: HashMap<String, (u64, String)>,
    pending: HashMap<String, u64>,
}

pub struct Submission {
    thread_id: String,
    operation: u64,
    clear_revision: Option<u64>,
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
        if text.is_empty() {
            self.text.remove(thread_id);
        } else {
            self.text
                .insert(thread_id.into(), (self.revision, text.into()));
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
}
