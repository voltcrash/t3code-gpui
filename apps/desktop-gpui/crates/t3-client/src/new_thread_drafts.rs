//! Draft sessions have client identities before the server creates a thread.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::message_drafts;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDraft {
    pub project_id: String,
    pub text: String,
    pub order: u64,
}

#[derive(Clone)]
pub struct DiscardedDraft {
    saved: SavedDraft,
    has_attachments: bool,
}

#[derive(Default)]
pub struct Drafts {
    pub messages: message_drafts::Drafts,
    sessions: BTreeMap<String, (String, u64)>,
    touched: HashSet<String>,
    attachments: HashSet<String>,
    revision: u64,
    order: u64,
}

impl Drafts {
    pub fn create(&mut self, project_id: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.order = self.order.saturating_add(1);
        self.sessions
            .insert(id.clone(), (project_id.into(), self.order));
        self.touched.insert(id.clone());
        self.revision += 1;
        id
    }

    pub fn project(&self, id: &str) -> Option<&str> {
        self.sessions.get(id).map(|(project, _)| project.as_str())
    }

    pub fn set_project(&mut self, id: &str, project_id: &str) {
        if self.messages.busy(id) {
            return;
        }
        if let Some((project, _)) = self.sessions.get_mut(id)
            && project != project_id
        {
            *project = project_id.into();
            self.touched.insert(id.into());
            self.revision += 1;
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        if self.messages.busy(id) || self.sessions.remove(id).is_none() {
            return false;
        }
        self.messages.edit(id, "");
        self.attachments.remove(id);
        self.touched.insert(id.into());
        self.revision += 1;
        true
    }

    pub fn discard(&mut self, id: &str) -> Option<DiscardedDraft> {
        let (project_id, order) = self.sessions.get(id)?.clone();
        let saved = SavedDraft {
            project_id,
            order,
            text: self.messages.text(id).into(),
        };
        let has_attachments = self.has_attachments(id);
        self.remove(id).then_some(DiscardedDraft {
            saved,
            has_attachments,
        })
    }

    pub fn restore_discarded(&mut self, id: &str, discarded: &DiscardedDraft) -> bool {
        if self.sessions.contains_key(id) {
            return false;
        }
        let saved = &discarded.saved;
        self.sessions
            .insert(id.into(), (saved.project_id.clone(), saved.order));
        self.messages.edit(id, &saved.text);
        if discarded.has_attachments {
            self.attachments.insert(id.into());
        }
        self.revision += 1;
        true
    }

    pub fn has_attachments(&self, id: &str) -> bool {
        self.attachments.contains(id)
    }

    pub fn mark_attachments(&mut self, id: &str, present: bool) -> bool {
        if !self.sessions.contains_key(id) {
            return false;
        }
        if present {
            self.attachments.insert(id.into())
        } else {
            self.attachments.remove(id)
        }
    }

    pub fn finish(&mut self, id: &str, submission: message_drafts::Submission, accepted: bool) {
        self.messages.finish(submission, accepted);
        if accepted && self.messages.text(id).is_empty() {
            self.remove(id);
        }
    }

    pub fn revision(&self) -> (u64, u64) {
        (self.revision, self.messages.revision())
    }

    pub fn rows(&self) -> Vec<&str> {
        let mut sessions: Vec<_> = self.sessions.iter().collect();
        sessions.sort_by_key(|(_, (_, order))| std::cmp::Reverse(*order));
        sessions.into_iter().map(|(id, _)| id.as_str()).collect()
    }

    pub fn snapshot(&self) -> BTreeMap<String, SavedDraft> {
        self.sessions
            .iter()
            .filter_map(|(id, (project_id, order))| {
                let text = self.messages.text(id);
                (!text.is_empty()).then(|| {
                    (
                        id.clone(),
                        SavedDraft {
                            project_id: project_id.clone(),
                            text: text.into(),
                            order: *order,
                        },
                    )
                })
            })
            .collect()
    }

    pub fn restore_missing(&mut self, saved: &BTreeMap<String, SavedDraft>) {
        for (id, draft) in saved {
            self.order = self.order.max(draft.order);
            if self.touched.contains(id) || self.sessions.contains_key(id) {
                continue;
            }
            self.sessions
                .insert(id.clone(), (draft.project_id.clone(), draft.order));
            self.messages.edit(id, &draft.text);
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_sessions_keep_independent_text_in_the_same_project() {
        let mut drafts = Drafts::default();
        let first = drafts.create("project");
        drafts.messages.edit(&first, "First draft");
        let second = drafts.create("project");
        drafts.messages.edit(&second, "Second draft");
        let empty = drafts.create("project");
        assert_ne!(first, second);
        assert_eq!(
            drafts.rows(),
            [empty.as_str(), second.as_str(), first.as_str()]
        );
        let saved = drafts.snapshot();
        assert_eq!(saved.len(), 2);
        let mut restored = Drafts::default();
        restored.restore_missing(&saved);
        assert_eq!(restored.messages.text(&first), "First draft");
        assert_eq!(restored.messages.text(&second), "Second draft");
        assert_eq!(restored.project(&second), Some("project"));
        assert_eq!(restored.rows(), [second.as_str(), first.as_str()]);
    }

    #[test]
    fn project_changes_keep_text_and_busy_sessions_cannot_move_or_be_discarded() {
        let mut drafts = Drafts::default();
        let id = drafts.create("first");
        drafts
            .messages
            .edit(&id, "Keep the draft when choosing a different project");
        drafts.set_project(&id, "second");
        assert_eq!(drafts.snapshot()[&id].project_id, "second");
        let submission = drafts.messages.begin(&id, true).unwrap();
        drafts.set_project(&id, "third");
        assert_eq!(drafts.project(&id), Some("second"));
        assert!(!drafts.remove(&id));
        drafts.messages.finish(submission, false);
        assert!(!drafts.messages.text(&id).is_empty());
        assert!(drafts.remove(&id));
        assert!(drafts.snapshot().is_empty());
    }

    #[test]
    fn attachment_only_drafts_stay_visible_and_undo_keeps_the_tray_claim() {
        let mut drafts = Drafts::default();
        let id = drafts.create("project");
        assert!(drafts.mark_attachments(&id, true));
        assert!(!drafts.mark_attachments(&id, true));
        assert!(drafts.has_attachments(&id));
        assert!(drafts.snapshot().is_empty());
        let discarded = drafts.discard(&id).unwrap();
        assert!(!drafts.has_attachments(&id));
        assert!(!drafts.mark_attachments(&id, true));
        assert!(drafts.restore_discarded(&id, &discarded));
        assert!(drafts.has_attachments(&id));
        assert!(drafts.mark_attachments(&id, false));
        assert!(!drafts.has_attachments(&id));
    }

    #[test]
    fn accepted_launch_removes_only_its_submitted_session() {
        let mut drafts = Drafts::default();
        let first = drafts.create("project");
        let second = drafts.create("project");
        drafts.messages.edit(&first, "Launch this");
        drafts.messages.edit(&second, "Keep this");
        let submission = drafts.messages.begin(&first, true).unwrap();
        drafts.finish(&first, submission, true);
        assert_eq!(drafts.project(&first), None);
        assert_eq!(drafts.messages.text(&second), "Keep this");
        let submission = drafts.messages.begin(&second, true).unwrap();
        drafts.messages.edit(&second, "Newer edit");
        drafts.finish(&second, submission, true);
        assert_eq!(drafts.messages.text(&second), "Newer edit");
        assert_eq!(drafts.project(&second), Some("project"));
    }

    #[test]
    fn undo_restores_the_exact_session_and_never_overwrites_new_content() {
        let mut drafts = Drafts::default();
        let id = drafts.create("project");
        drafts.messages.edit(&id, "Unsent text 🌱");
        let discarded = drafts.discard(&id).unwrap();
        assert_eq!(drafts.project(&id), None);
        assert!(drafts.restore_discarded(&id, &discarded));
        assert_eq!(drafts.messages.text(&id), "Unsent text 🌱");
        drafts.messages.edit(&id, "New text");
        assert!(!drafts.restore_discarded(&id, &discarded));
        assert_eq!(drafts.messages.text(&id), "New text");
    }

    #[test]
    fn retrying_a_load_does_not_resurrect_a_discarded_session_or_replace_edits() {
        let mut drafts = Drafts::default();
        let edited = drafts.create("project");
        let discarded = drafts.create("project");
        drafts.messages.edit(&edited, "Old text");
        drafts.messages.edit(&discarded, "Discard me");
        let saved = drafts.snapshot();
        drafts.messages.edit(&edited, "New edit");
        drafts.remove(&discarded);
        drafts.restore_missing(&saved);
        assert_eq!(drafts.messages.text(&edited), "New edit");
        assert_eq!(drafts.project(&discarded), None);
        let mut restored = Drafts::default();
        restored.restore_missing(&saved);
        restored.messages.edit(&edited, "");
        restored.restore_missing(&saved);
        assert_eq!(restored.messages.text(&edited), "");
    }
}
