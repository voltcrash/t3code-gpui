//! Draft sessions have client identities before the server creates a thread.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::message_drafts;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDraft {
    pub project_id: String,
    pub text: String,
    #[serde(default)]
    pub picks: message_drafts::Picks,
    pub order: u64,
    #[serde(default)]
    pub workspace: Option<crate::workspaces::Strategy>,
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
    workspaces: BTreeMap<String, crate::workspaces::Strategy>,
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
            self.workspaces.remove(id);
            self.touched.insert(id.into());
            self.revision += 1;
        }
    }

    pub fn workspace(&self, id: &str) -> Option<&crate::workspaces::Strategy> {
        self.workspaces.get(id)
    }

    pub fn set_workspace(&mut self, id: &str, workspace: crate::workspaces::Strategy) {
        if !self.sessions.contains_key(id)
            || self.messages.busy(id)
            || self.workspaces.get(id) == Some(&workspace)
        {
            return;
        }
        self.workspaces.insert(id.into(), workspace);
        self.touched.insert(id.into());
        self.revision += 1;
    }

    pub fn remove(&mut self, id: &str) -> bool {
        if self.messages.busy(id) || self.sessions.remove(id).is_none() {
            return false;
        }
        self.messages.edit(id, "");
        self.messages.set_picks(id, Default::default());
        self.attachments.remove(id);
        self.workspaces.remove(id);
        self.touched.insert(id.into());
        self.revision += 1;
        true
    }

    pub fn discard(&mut self, id: &str) -> Option<DiscardedDraft> {
        let (project_id, order) = self.sessions.get(id)?.clone();
        let saved = SavedDraft {
            project_id,
            order,
            workspace: self.workspace(id).cloned(),
            text: self.messages.text(id).into(),
            picks: self.messages.picks(id).cloned().unwrap_or_default(),
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
        if let Some(workspace) = &saved.workspace {
            self.workspaces.insert(id.into(), workspace.clone());
        }
        self.messages.edit(id, &saved.text);
        self.messages.set_picks(id, saved.picks.clone());
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
        let unchanged_picks = self.messages.picks_unchanged(&submission);
        self.messages.finish(submission, accepted);
        if accepted && unchanged_picks && self.messages.text(id).is_empty() {
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

    pub fn has_saved_content(&self, id: &str) -> bool {
        !self.messages.text(id).is_empty()
            || self.messages.picks(id).is_some()
            || self.workspace(id).is_some()
    }

    pub fn snapshot(&self) -> BTreeMap<String, SavedDraft> {
        self.sessions
            .iter()
            .filter_map(|(id, (project_id, order))| {
                let text = self.messages.text(id);
                let picks = self.messages.picks(id).cloned().unwrap_or_default();
                self.has_saved_content(id).then(|| {
                    (
                        id.clone(),
                        SavedDraft {
                            project_id: project_id.clone(),
                            text: text.into(),
                            picks,
                            order: *order,
                            workspace: self.workspace(id).cloned(),
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
            if let Some(workspace) = &draft.workspace {
                self.workspaces.insert(id.clone(), workspace.clone());
            }
            self.messages.edit(id, &draft.text);
            self.messages.set_picks(id, draft.picks.clone());
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
    fn choices_without_text_survive_restart_and_discard_undo() {
        let mut drafts = Drafts::default();
        let id = drafts.create("project");
        let picks = message_drafts::Picks {
            runtime_mode: Some("approval-required".into()),
            ..Default::default()
        };
        drafts.messages.set_picks(&id, picks.clone());
        let workspace = crate::workspaces::Strategy::Worktree {
            base_ref: "release".into(),
            start_from_origin: true,
        };
        drafts.set_workspace(&id, workspace.clone());
        drafts.messages.set_picks(&id, Default::default());
        assert!(
            drafts.has_saved_content(&id),
            "workspace-only drafts stay visible"
        );
        drafts.messages.set_picks(&id, picks.clone());
        let mut restored = Drafts::default();
        restored.restore_missing(&drafts.snapshot());
        assert_eq!(restored.messages.picks(&id), Some(&picks));
        assert_eq!(restored.workspace(&id), Some(&workspace));
        let discarded = drafts.discard(&id).unwrap();
        assert!(drafts.messages.picks(&id).is_none());
        drafts.restore_discarded(&id, &discarded);
        assert_eq!(drafts.messages.picks(&id), Some(&picks));
        assert_eq!(drafts.workspace(&id), Some(&workspace));
        let submission = drafts.messages.begin(&id, true).unwrap();
        drafts.set_workspace(&id, Default::default());
        assert_eq!(drafts.workspace(&id), Some(&workspace));
        drafts.messages.set_picks(
            &id,
            message_drafts::Picks {
                runtime_mode: Some("auto".into()),
                ..Default::default()
            },
        );
        drafts.finish(&id, submission, true);
        assert_eq!(drafts.project(&id), Some("project"));
        drafts.set_project(&id, "another-project");
        assert_eq!(drafts.workspace(&id), None);
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
