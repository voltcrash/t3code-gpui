//! The native palette dispatches the same actions as menus and shortcuts.

use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::*;

use crate::desktop::{OpenSettings, ResetZoom, ToggleFullscreen, ZoomIn, ZoomOut};
use crate::environment::LocalEnvironment;
use crate::workspace::{
    AddProject, FindInThread, GoBack, GoForward, NewThread, NextThread, OpenInEditor,
    PreviousThread, ToggleChanges, TogglePlanMode, ToggleSidebar, ToggleThreadPin,
    ToggleThreadSettlement,
};

#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
pub struct OpenThread {
    pub thread_id: SharedString,
}

fn item(label: &str, icon: IconName, action: impl Action) -> CommandItem {
    CommandItem::new()
        .label(label)
        .icon(icon)
        .action(Box::new(action))
}

pub fn content(
    state: &Entity<CommandState>,
    env: &LocalEnvironment,
    thread_id: Option<&str>,
    can_plan: bool,
    sidebar_visible: bool,
    can_open_editor: bool,
) -> Command {
    let thread = thread_id.and_then(|id| env.thread(id));
    let can_operate = env.rpc.is_some();
    let mut actions = CommandGroup::new()
        .label("Actions")
        .item(item("New thread", IconName::SquarePen, NewThread))
        .item(item("Add project", IconName::FolderPlus, AddProject))
        .item(item("Settings", IconName::Settings, OpenSettings))
        .item(
            item(
                "Open in editor",
                IconName::SquareArrowOutUpRight,
                OpenInEditor,
            )
            .disabled(!can_open_editor),
        )
        .item(
            item("Find in thread", IconName::Search, FindInThread)
                .disabled(thread.is_none() || !can_operate || !env.server.thread_find),
        )
        .item(item("Toggle plan mode", IconName::ListTodo, TogglePlanMode).disabled(!can_plan));
    if let Some(thread) = thread {
        use t3_client::thread_actions::Shortcut;
        actions = actions
            .item(
                item(
                    if thread.pinned_at.is_some() {
                        "Unpin thread"
                    } else {
                        "Pin thread"
                    },
                    IconName::Pin,
                    ToggleThreadPin,
                )
                .disabled(
                    !can_operate
                        || Shortcut::TogglePin
                            .action(thread, env.thread_actions)
                            .is_none(),
                ),
            )
            .item(
                item(
                    if thread.settled_override.as_deref() == Some("settled") {
                        "Un-settle thread"
                    } else {
                        "Settle thread"
                    },
                    IconName::CircleCheck,
                    ToggleThreadSettlement,
                )
                .disabled(
                    !can_operate
                        || Shortcut::ToggleSettlement
                            .action(thread, env.thread_actions)
                            .is_none(),
                ),
            );
    }
    let view = CommandGroup::new()
        .label("View")
        .item(item("Back", IconName::ArrowLeft, GoBack))
        .item(item("Forward", IconName::ArrowRight, GoForward))
        .item(item(
            if sidebar_visible {
                "Hide sidebar"
            } else {
                "Show sidebar"
            },
            IconName::PanelLeft,
            ToggleSidebar,
        ))
        .item(item("Toggle changes", IconName::FileDiff, ToggleChanges).disabled(thread.is_none()))
        .item(item("Previous thread", IconName::ArrowUp, PreviousThread))
        .item(item("Next thread", IconName::ArrowDown, NextThread))
        .item(item("Zoom in", IconName::ZoomIn, ZoomIn))
        .item(item("Zoom out", IconName::ZoomOut, ZoomOut))
        .item(item("Actual size", IconName::Maximize, ResetZoom))
        .item(item(
            "Toggle full screen",
            IconName::Maximize,
            ToggleFullscreen,
        ));
    let mut threads = CommandGroup::new().label("Threads");
    // Shell summaries keep the palette independent of transcript size.
    let mut ordered: Vec<_> = env
        .shell
        .threads
        .values()
        .filter(|thread| !thread.is_subagent())
        .collect();
    ordered.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    for thread in ordered {
        let project = env
            .shell
            .project(&thread.project_id)
            .map(|project| project.title.clone())
            .unwrap_or_default();
        threads = threads.item(
            CommandItem::new()
                .label(thread.title.clone())
                .keywords([project])
                .icon(IconName::MessageSquare)
                .action(Box::new(OpenThread {
                    thread_id: thread.id.clone().into(),
                })),
        );
    }
    Command::new(state)
        .placeholder("Search commands and threads…")
        .group(actions)
        .group(view)
        .group(threads)
        .max_h(px(420.))
}
