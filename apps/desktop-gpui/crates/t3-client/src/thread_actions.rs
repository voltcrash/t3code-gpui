//! Thread organization commands from the existing client runtime. Mutations
//! become visible only through the server's shell projection.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::shell::ThreadShell;

#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    pub pinning: bool,
    pub settlement: bool,
}

impl Capabilities {
    pub fn from_config(config: &Value) -> Self {
        let capabilities = &config["environment"]["capabilities"];
        Self {
            pinning: capabilities["threadPinning"] == true,
            settlement: capabilities["threadSettlement"] == true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Rename(String),
    Pin,
    Unpin,
    Settle,
    Unsettle,
    Archive,
    Restore,
    Delete,
}

impl Action {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Rename(_) => "Rename thread",
            Self::Pin => "Pin thread",
            Self::Unpin => "Unpin thread",
            Self::Settle => "Settle thread",
            Self::Unsettle => "Un-settle thread",
            Self::Archive => "Archive thread",
            Self::Restore => "Restore thread",
            Self::Delete => "Delete thread",
        }
    }

    pub fn command(&self, thread_id: &str) -> Value {
        let kind = match self {
            Self::Rename(_) => "thread.metadata.update",
            Self::Pin => "thread.pin",
            Self::Unpin => "thread.unpin",
            Self::Settle => "thread.settle",
            Self::Unsettle => "thread.unsettle",
            Self::Archive => "thread.archive",
            Self::Restore => "thread.unarchive",
            Self::Delete => "thread.delete",
        };
        let mut command = json!({
            "type": kind, "commandId": Uuid::new_v4().to_string(), "threadId": thread_id,
        });
        match self {
            Self::Rename(title) => command["title"] = title.trim().into(),
            Self::Unsettle => command["reason"] = "user".into(),
            _ => {}
        }
        command
    }
}

pub fn menu_actions(thread: &ThreadShell, capabilities: Capabilities) -> Vec<(Action, bool)> {
    let mut actions = vec![(Action::Rename(thread.title.clone()), true)];
    if thread.archived_at.is_some() {
        actions.push((Action::Restore, true));
    } else {
        if capabilities.pinning {
            actions.push((
                if thread.pinned_at.is_some() {
                    Action::Unpin
                } else {
                    Action::Pin
                },
                true,
            ));
        }
        if capabilities.settlement {
            actions.push((
                if thread.settled_override.as_deref() == Some("settled") {
                    Action::Unsettle
                } else {
                    Action::Settle
                },
                true,
            ));
        }
        // `threadRuntimeCanArchive`: queued work without an attached run can
        // be archived; preparing, starting and running work cannot.
        let can_archive = match thread.runtime_status() {
            Some("preparing" | "starting" | "running") => false,
            Some("queued") => thread.active_run_id.is_none(),
            _ => true,
        };
        actions.push((Action::Archive, can_archive));
    }
    actions.push((Action::Delete, true));
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(patch: Value) -> ThreadShell {
        let mut value = json!({"id":"thread-1", "projectId":"project-1", "title":"Before", "status":"idle", "createdAt":"2026-01-01T00:00:00Z", "updatedAt":"2026-01-01T00:00:00Z"});
        value
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn organization_actions_follow_capabilities_and_have_reverse_paths() {
        let caps = Capabilities {
            pinning: true,
            settlement: true,
        };
        let active = menu_actions(&thread(json!({})), caps);
        assert!(active.contains(&(Action::Pin, true)));
        assert!(active.contains(&(Action::Settle, true)));
        assert!(active.contains(&(Action::Archive, true)));
        let parked = menu_actions(
            &thread(json!({"pinnedAt":"2026-01-01T00:00:00Z","settledOverride":"settled"})),
            caps,
        );
        assert!(parked.contains(&(Action::Unpin, true)));
        assert!(parked.contains(&(Action::Unsettle, true)));
        let archived = menu_actions(&thread(json!({"archivedAt":"2026-01-01T00:00:00Z"})), caps);
        assert!(archived.contains(&(Action::Restore, true)));
        assert!(
            !archived.iter().any(|(action, _)| matches!(
                action,
                Action::Pin | Action::Settle | Action::Archive
            ))
        );
        let unsupported = menu_actions(&thread(json!({})), Capabilities::default());
        assert!(
            !unsupported
                .iter()
                .any(|(action, _)| matches!(action, Action::Pin | Action::Settle))
        );
    }

    #[test]
    fn archiving_respects_runtime_attachment_including_queued_runs() {
        for (status, attached, enabled) in [
            ("running", false, false),
            ("preparing", true, false),
            ("starting", true, false),
            ("queued", true, false),
            ("queued", false, true),
            ("completed", true, true),
        ] {
            let thread = thread(
                json!({"status":status,"latestRunId":"run-1","activeRunId":if attached {Some("run-1")}else{None}}),
            );
            assert!(
                menu_actions(&thread, Capabilities::default())
                    .contains(&(Action::Archive, enabled)),
                "{status}, attached={attached}"
            );
        }
    }
}
