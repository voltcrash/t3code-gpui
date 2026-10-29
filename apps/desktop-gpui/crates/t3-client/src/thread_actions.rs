//! Thread organization commands from the existing client runtime. Mutations
//! become visible only through the server's shell projection.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::shell::ThreadShell;

#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    pub pinning: bool,
    pub settlement: bool,
    pub visited_tracking: bool,
    pub snooze: bool,
}

impl Capabilities {
    pub fn from_config(config: &Value) -> Self {
        let capabilities = &config["environment"]["capabilities"];
        Self {
            pinning: capabilities["threadPinning"] == true,
            settlement: capabilities["threadSettlement"] == true,
            visited_tracking: capabilities["threadVisitedTracking"] == true,
            snooze: capabilities["threadSnooze"] == true,
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
    MarkUnread,
    Snooze(String),
    Unsnooze,
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
            Self::MarkUnread => "Mark unread",
            Self::Snooze(_) => "Snooze",
            Self::Unsnooze => "Wake thread",
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
            Self::MarkUnread => "thread.mark-unread",
            Self::Snooze(_) => "thread.snooze",
            Self::Unsnooze => "thread.unsnooze",
            Self::Delete => "thread.delete",
        };
        let mut command = json!({
            "type": kind, "commandId": Uuid::new_v4().to_string(), "threadId": thread_id,
        });
        match self {
            Self::Rename(title) => command["title"] = title.trim().into(),
            Self::Unsettle | Self::Unsnooze => command["reason"] = "user".into(),
            Self::Snooze(until) => command["snoozedUntil"] = until.as_str().into(),
            _ => {}
        }
        command
    }
}

#[derive(Clone, Copy)]
pub enum Shortcut {
    TogglePin,
    ToggleSettlement,
}

impl Shortcut {
    pub fn action(self, thread: &ThreadShell, capabilities: Capabilities) -> Option<Action> {
        menu_actions(thread, capabilities)
            .into_iter()
            .find(|(action, enabled)| {
                *enabled
                    && matches!(
                        (self, action),
                        (Self::TogglePin, Action::Pin | Action::Unpin)
                            | (Self::ToggleSettlement, Action::Settle | Action::Unsettle)
                    )
            })
            .map(|(action, _)| action)
    }
}

pub fn menu_actions(thread: &ThreadShell, capabilities: Capabilities) -> Vec<(Action, bool)> {
    menu_actions_at(
        thread,
        capabilities,
        jiff::Timestamp::now().as_millisecond(),
    )
}

pub fn menu_actions_at(
    thread: &ThreadShell,
    capabilities: Capabilities,
    now_ms: i64,
) -> Vec<(Action, bool)> {
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
        if capabilities.snooze {
            if thread.is_snoozed(now_ms) {
                actions.push((Action::Unsnooze, true));
            } else {
                actions.push((
                    Action::Snooze(String::new()),
                    crate::snooze::can_snooze(thread, now_ms),
                ));
            }
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
    if capabilities.visited_tracking {
        actions.push((
            Action::MarkUnread,
            thread.latest_run_completed_at().is_some(),
        ));
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
            ..Default::default()
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
    fn shortcuts_toggle_the_committed_state_and_respect_available_actions() {
        let caps = Capabilities {
            pinning: true,
            settlement: true,
            ..Default::default()
        };
        let active = thread(json!({}));
        assert_eq!(Shortcut::TogglePin.action(&active, caps), Some(Action::Pin));
        assert_eq!(
            Shortcut::ToggleSettlement.action(&active, caps),
            Some(Action::Settle)
        );
        let organized =
            thread(json!({"pinnedAt":"2026-01-01T00:00:00Z", "settledOverride":"settled"}));
        assert_eq!(
            Shortcut::TogglePin.action(&organized, caps),
            Some(Action::Unpin)
        );
        assert_eq!(
            Shortcut::ToggleSettlement.action(&organized, caps),
            Some(Action::Unsettle)
        );
        for shortcut in [Shortcut::TogglePin, Shortcut::ToggleSettlement] {
            assert_eq!(shortcut.action(&active, Capabilities::default()), None);
            assert_eq!(
                shortcut.action(&thread(json!({"archivedAt":"2026-01-01T00:00:00Z"})), caps),
                None
            );
        }
        let pin = Shortcut::TogglePin
            .action(&active, caps)
            .unwrap()
            .command(&active.id);
        assert_eq!(pin["type"], "thread.pin");
        assert_eq!(pin["threadId"], active.id);
        let unsettle = Shortcut::ToggleSettlement
            .action(&organized, caps)
            .unwrap()
            .command(&organized.id);
        assert_eq!(unsettle["type"], "thread.unsettle");
        assert_eq!(unsettle["reason"], "user");
    }

    #[test]
    fn marking_unread_uses_server_read_state_even_for_archived_threads() {
        let caps = Capabilities {
            visited_tracking: true,
            ..Default::default()
        };
        assert!(menu_actions(&thread(json!({})), caps).contains(&(Action::MarkUnread, false)));
        for patch in [json!({}), json!({"archivedAt":"2026-01-01T00:00:00Z"})] {
            let mut patch = patch;
            patch["latestRunId"] = "run-1".into();
            patch["latestRunCompletedAt"] = "2026-01-01T01:00:00Z".into();
            let thread = thread(patch);
            assert!(menu_actions(&thread, caps).contains(&(Action::MarkUnread, true)));
            assert!(
                !menu_actions(&thread, Capabilities::default())
                    .iter()
                    .any(|(action, _)| *action == Action::MarkUnread)
            );
            let command = Action::MarkUnread.command(&thread.id);
            assert_eq!(command["type"], "thread.mark-unread");
            assert_eq!(command["threadId"], thread.id);
            assert!(command.get("visitedAt").is_none());
        }
    }

    #[test]
    fn snooze_actions_follow_time_capabilities_and_pending_requests() {
        let now = "2026-01-01T12:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond();
        let until = "2026-01-02T09:00:00Z";
        let caps = Capabilities::from_config(
            &json!({"environment":{"capabilities":{"threadSnooze":true}}}),
        );
        let snooze = Action::Snooze(String::new());
        assert!(menu_actions_at(&thread(json!({})), caps, now).contains(&(snooze.clone(), true)));
        assert!(
            menu_actions_at(
                &thread(json!({"pendingRuntimeRequest":{"kind":"user_input", "blocking":false}})),
                caps,
                now
            )
            .contains(&(snooze.clone(), false))
        );
        let sleeping = thread(json!({"snoozedUntil":until}));
        let actions = menu_actions_at(&sleeping, caps, now);
        assert!(actions.contains(&(Action::Unsnooze, true)));
        assert!(
            !actions
                .iter()
                .any(|(action, _)| matches!(action, Action::Snooze(_)))
        );
        assert!(menu_actions_at(&sleeping, caps, now + 2 * 86_400_000).contains(&(snooze, true)));
        for patch in [json!({}), json!({"archivedAt":"2026-01-01T00:00:00Z"})] {
            assert!(
                !menu_actions_at(&thread(patch), Capabilities::default(), now)
                    .iter()
                    .any(|(action, _)| matches!(action, Action::Snooze(_) | Action::Unsnooze))
            );
        }
        let snooze = Action::Snooze(until.into()).command("thread-1");
        assert_eq!(snooze["type"], "thread.snooze");
        assert_eq!(snooze["snoozedUntil"], until);
        let wake = Action::Unsnooze.command("thread-1");
        assert_eq!(wake["type"], "thread.unsnooze");
        assert_eq!(wake["reason"], "user");
        assert!(crate::snooze::valid_until(until, now));
        assert!(!crate::snooze::valid_until(until, now + 2 * 86_400_000));
        assert!(!crate::snooze::valid_until("invalid", now));
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
