//! The window's root: sidebar beside the open thread or a new-thread draft.

use std::collections::HashMap;

use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::StyledExt;
use gpui_kit::component::command::CommandState;
use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::component::{ActiveTheme, Disableable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use t3_client::shell::timestamp_ms;

use crate::appearance::{SIDEBAR_WIDTH, TOPBAR_HEIGHT};
use crate::attachment_draft::AttachmentDraft;
use crate::changes::{self, Changes};
use crate::desktop::{
    self, About, CloseWindow, Maximize, Minimize, OpenSettings, PasteAsText, ResetZoom,
    ToggleFullscreen, ZoomIn, ZoomOut,
};
use crate::draft_view::{DraftEvent, DraftView};
use crate::environment::{ConnectionState, LocalEnvironment};
use crate::project_picker::{ProjectEvent, ProjectPicker};
use crate::settings::{self, Settings};
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::thread_actions::{self, ThreadActions};
use crate::thread_view::ThreadView;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};

const CONTEXT: &str = "Workspace";

actions!(
    workspace,
    [
        NewThread,
        AddProject,
        ToggleCommandPalette,
        CloseCommandPalette,
        FindInThread,
        ToggleChanges,
        TogglePlanMode,
        ToggleSidebar,
        ToggleThreadPin,
        ToggleThreadSettlement,
        PreviousThread,
        NextThread
    ]
);

#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
pub struct JumpToThread {
    index: usize,
}

fn palette_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("secondary-k", ToggleCommandPalette, Some(CONTEXT)),
        // Command's default Escape only clears the query; the app dismisses it.
        KeyBinding::new("escape", CloseCommandPalette, Some("Workspace > Command")),
        KeyBinding::new(
            "escape",
            CloseCommandPalette,
            Some("Workspace > Command > Input"),
        ),
    ]
}

fn traversal_bindings() -> Vec<KeyBinding> {
    vec![
        // Linux reports shifted punctuation as the symbol with Shift removed.
        KeyBinding::new("secondary-{", PreviousThread, Some(CONTEXT)),
        KeyBinding::new("secondary-}", NextThread, Some(CONTEXT)),
        KeyBinding::new("secondary-shift-[", PreviousThread, Some(CONTEXT)),
        KeyBinding::new("secondary-shift-]", NextThread, Some(CONTEXT)),
    ]
}

/// `chat.new` in `packages/shared/src/keybindings.ts`.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(traversal_bindings());
    cx.bind_keys(palette_bindings());
    cx.bind_keys([
        KeyBinding::new("secondary-shift-o", NewThread, Some(CONTEXT)),
        KeyBinding::new("secondary-n", NewThread, Some(CONTEXT)),
        KeyBinding::new("secondary-f", FindInThread, Some(CONTEXT)),
        KeyBinding::new("secondary-shift-p", ToggleThreadPin, Some(CONTEXT)),
        KeyBinding::new("secondary-shift-s", ToggleThreadSettlement, Some(CONTEXT)),
        KeyBinding::new("secondary-b", ToggleSidebar, Some(CONTEXT)),
        KeyBinding::new("secondary-d", ToggleChanges, Some(CONTEXT)),
        KeyBinding::new("secondary-shift-a", TogglePlanMode, Some(CONTEXT)),
        KeyBinding::new(
            "shift-tab",
            TogglePlanMode,
            Some("NativePlanComposer > Input"),
        ),
    ]);
    for index in 0..9 {
        cx.bind_keys([KeyBinding::new(
            &format!("secondary-{}", index + 1),
            JumpToThread { index },
            Some(CONTEXT),
        )]);
    }
}

pub struct Workspace {
    focus: FocusHandle,
    palette: Option<Entity<CommandState>>,
    palette_focus: FocusHandle,
    palette_return_focus: Option<FocusHandle>,
    zoom: f32,
    menu: Entity<AppMenuBar>,
    env: Entity<LocalEnvironment>,
    sidebar: Entity<Sidebar>,
    visit: Entity<crate::thread_visit::ThreadVisit>,
    sidebar_visible: bool,
    sidebar_focus: FocusHandle,
    actions: Entity<ThreadActions>,
    thread: Option<Entity<ThreadView>>,
    attachment_drafts: HashMap<SharedString, Entity<AttachmentDraft>>,
    draft: Option<(Entity<DraftView>, Subscription)>,
    project_picker: Option<(Entity<ProjectPicker>, Subscription)>,
    pending_project: Option<String>,
    project_notice: Option<SharedString>,
    settings: Option<(Entity<Settings>, Subscription)>,
    settings_return_focus: Option<FocusHandle>,
    restore_settings_focus: bool,
    changes: Option<(Entity<Changes>, Subscription)>,
    changes_return_focus: Option<FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_none() {
            return;
        }
        self.palette_return_focus
            .take()
            .unwrap_or_else(|| self.focus.clone())
            .focus(window, cx);
        cx.notify();
    }

    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        if self.project_picker.is_some() || self.actions.read(cx).has_dialog() {
            return;
        }
        self.palette_return_focus = window.focused(cx);
        let palette = cx.new(|cx| CommandState::new(window, cx));
        palette.read(cx).focus_handle(cx).focus(window, cx);
        self.palette = Some(palette);
        let this = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |this, cx| {
                if let Some(palette) = &this.palette {
                    palette.read(cx).focus_handle(cx).focus(window, cx);
                }
            });
        });
        cx.notify();
    }

    fn navigate_thread(
        &mut self,
        destination: t3_client::navigation::Destination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_palette(window, cx);
        if self.settings.is_some()
            || self.project_picker.is_some()
            || self.actions.read(cx).has_dialog()
        {
            return;
        }
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.navigate(destination, window, cx));
    }

    fn thread_shortcut(
        &mut self,
        shortcut: t3_client::thread_actions::Shortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_palette(window, cx);
        if self.settings.is_some()
            || self.project_picker.is_some()
            || self.actions.read(cx).has_dialog()
        {
            return;
        }
        let Some(thread) = &self.thread else {
            return;
        };
        let id = thread.read(cx).thread_id().to_string();
        self.actions
            .update(cx, |actions, cx| actions.shortcut(id, shortcut, window, cx));
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        self.sidebar_visible = !self.sidebar_visible;
        if !self.sidebar_visible && self.sidebar_focus.contains_focused(window, cx) {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    fn toggle_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        if self.settings.is_some()
            || self.project_picker.is_some()
            || self.actions.read(cx).has_dialog()
        {
            return;
        }
        let toggled = if let Some((draft, _)) = &self.draft {
            draft.update(cx, |draft, cx| draft.toggle_plan_mode(window, cx))
        } else if let Some(thread) = &self.thread {
            thread.update(cx, |thread, cx| thread.toggle_plan_mode(window, cx))
        } else {
            false
        };
        if toggled && window.viewport_size().width < px(1150.) {
            self.changes = None;
            self.changes_return_focus = None;
            cx.notify();
        }
    }

    fn close_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.changes = None;
        if let Some(focus) = self.changes_return_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn toggle_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        if self.settings.is_some()
            || self.project_picker.is_some()
            || self.actions.read(cx).has_dialog()
        {
            return;
        }
        if self.changes.is_some() {
            self.close_changes(window, cx);
            return;
        }
        let Some(thread) = &self.thread else {
            return;
        };
        self.changes_return_focus = window.focused(cx);
        let panel = cx.new(|cx| Changes::new(self.env.clone(), thread.clone(), window, cx));
        let subscription = cx.subscribe_in(
            &panel,
            window,
            |this, _, _: &changes::Closed, window, cx| this.close_changes(window, cx),
        );
        self.changes = Some((panel, subscription));
        cx.notify();
    }
    fn find_in_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        if self.settings.is_some()
            || self.project_picker.is_some()
            || self.actions.read(cx).has_dialog()
        {
            return;
        }
        if let Some(thread) = &self.thread {
            // Find in Thread reveals the conversation when a narrow window shows only Changes.
            if window.viewport_size().width < px(1150.) {
                self.changes = None;
                self.changes_return_focus = None;
            }
            thread.update(cx, |thread, cx| thread.open_find(window, cx));
        }
    }
    pub fn new(env: Entity<LocalEnvironment>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_window_bounds(window, |_, window, cx| {
            crate::window_state::remember(window, cx);
        })
        .detach();

        let focus = cx.focus_handle();
        let actions = cx.new(|cx| ThreadActions::new(env.clone(), focus.clone(), window, cx));
        let sidebar = cx.new(|cx| Sidebar::new(env.clone(), actions.clone(), window, cx));
        let visit = cx.new(|cx| crate::thread_visit::ThreadVisit::new(env.clone(), window, cx));
        let subscriptions = vec![
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    crate::notifications::clear(cx);
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                let appearance = crate::preferences::global(cx).read(cx).value.appearance;
                if appearance == t3_host::preferences::Appearance::System {
                    crate::preferences::apply_appearance(appearance, Some(window), cx);
                }
            }),
            cx.observe(&actions, |_, _, cx| cx.notify()),
            cx.observe_in(&env, window, |this, env, window, cx| {
                let env = env.read(cx);
                let missing_draft = this.draft.as_ref().is_some_and(|(draft, _)| {
                    env.shell.project(draft.read(cx).project_id()).is_none()
                });
                let missing_thread = this
                    .sidebar
                    .read(cx)
                    .selected()
                    .is_some_and(|id| env.thread(id.as_ref()).is_none());
                let ready = this
                    .pending_project
                    .clone()
                    .filter(|id| env.shell.project(id).is_some());
                if missing_draft {
                    this.draft = None;
                }
                if missing_thread {
                    this.thread = None;
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.clear_selection(cx));
                }
                if let Some(id) = ready {
                    this.open_project(id, window, cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &sidebar,
                window,
                |this, _, event: &SidebarEvent, window, cx| match event {
                    SidebarEvent::SelectionChanged => this.open_selected(window, cx),
                    SidebarEvent::NewThread => this.new_thread(window, cx),
                    SidebarEvent::AddProject => this.add_project(window, cx),
                    SidebarEvent::Settings => this.open_settings(window, cx),
                },
            ),
        ];
        Self {
            focus,
            palette: None,
            palette_focus: cx.focus_handle(),
            palette_return_focus: None,
            zoom: 1.,
            menu: AppMenuBar::new(cx),
            env,
            sidebar,
            visit,
            sidebar_visible: true,
            sidebar_focus: cx.focus_handle(),
            actions,
            thread: None,
            attachment_drafts: HashMap::new(),
            draft: None,
            project_picker: None,
            pending_project: None,
            project_notice: None,
            settings: None,
            settings_return_focus: None,
            restore_settings_focus: false,
            changes: None,
            changes_return_focus: None,
            _subscriptions: subscriptions,
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        if self.settings.is_some()
            || self.project_picker.is_some()
            || self.actions.read(cx).has_dialog()
        {
            return;
        }
        self.settings_return_focus = window.focused(cx);
        let settings = cx.new(|cx| Settings::new(self.env.clone(), window, cx));
        let subscription =
            cx.subscribe_in(&settings, window, |this, _, _: &settings::Closed, _, cx| {
                this.settings = None;
                this.restore_settings_focus = true;
                cx.notify();
            });
        self.settings = Some((settings, subscription));
        cx.notify();
    }

    /// Opens a draft in the open thread's project, else the draft's own,
    /// else the most recently active one.
    fn new_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        if self.actions.read(cx).has_dialog() {
            return;
        }
        self.settings = None;
        self.project_picker = None;
        self.pending_project = None;
        self.project_notice = None;
        let env = self.env.read(cx);
        let project_id = self
            .thread
            .as_ref()
            .and_then(|view| env.shell.threads.get(view.read(cx).thread_id().as_ref()))
            .map(|thread| thread.project_id.clone())
            .or_else(|| {
                self.draft
                    .as_ref()
                    .map(|(draft, _)| draft.read(cx).project_id().to_owned())
            })
            .or_else(|| {
                // The web orders by the sidebar's saved project order, which
                // this app does not have yet; the latest activity stands in.
                env.shell
                    .threads
                    .values()
                    .filter(|thread| env.shell.project(&thread.project_id).is_some())
                    .max_by_key(|thread| timestamp_ms(Some(&thread.updated_at)))
                    .map(|thread| thread.project_id.clone())
            })
            .or_else(|| env.shell.projects.first().map(|project| project.id.clone()));
        let Some(project_id) = project_id else {
            self.add_project(window, cx);
            return;
        };
        self.open_draft(project_id, window, cx);
    }

    fn open_draft(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.changes = None;
        self.changes_return_focus = None;
        let draft = cx.new(|cx| DraftView::new(self.env.clone(), project_id, window, cx));
        let subscription = cx.subscribe_in(
            &draft,
            window,
            |this, _, event: &DraftEvent, window, cx| match event {
                DraftEvent::Launched(thread_id) => {
                    let thread_id = thread_id.clone();
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.select(thread_id, cx));
                }
                DraftEvent::AddProject => this.add_project(window, cx),
            },
        );
        self.thread = None;
        self.draft = Some((draft, subscription));
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.clear_selection(cx));
        cx.notify();
    }

    fn add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        if self.actions.read(cx).has_dialog() {
            return;
        }
        if self.project_picker.is_some() {
            return;
        }
        let env = self.env.read(cx);
        let cwd = self
            .draft
            .as_ref()
            .and_then(|(draft, _)| env.shell.project(draft.read(cx).project_id()))
            .or_else(|| {
                self.sidebar
                    .read(cx)
                    .selected()
                    .and_then(|id| env.shell.threads.get(id.as_ref()))
                    .and_then(|thread| env.shell.project(&thread.project_id))
            })
            .map(|project| project.workspace_root.clone());
        let picker = cx.new(|cx| ProjectPicker::new(self.env.clone(), cwd, window, cx));
        let subscription = cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &ProjectEvent, window, cx| {
                match event {
                    ProjectEvent::Closed => {
                        this.project_picker = None;
                    }
                    ProjectEvent::Opened(id, warning) => {
                        this.project_notice = warning.as_ref().map(|warning| {
                            format!("Project created without a first commit: {warning}").into()
                        });
                        this.open_project(id.clone(), window, cx);
                    }
                }
                cx.notify();
            },
        );
        self.pending_project = None;
        self.project_picker = Some((picker, subscription));
        cx.notify();
    }

    fn open_project(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        self.project_picker = None;
        let env = self.env.read(cx);
        if env.shell.project(&project_id).is_none() {
            // The reply can beat the committed project event. Open the draft
            // only after its project reaches the shared shell projection.
            self.pending_project = Some(project_id);
            cx.notify();
            return;
        }
        let latest = env
            .shell
            .threads
            .values()
            .filter(|thread| thread.project_id == project_id)
            .max_by_key(|thread| timestamp_ms(Some(&thread.updated_at)))
            .filter(|thread| thread.settled_override.as_deref() != Some("settled"))
            .map(|thread| SharedString::from(thread.id.clone()));
        self.pending_project = None;
        if let Some(id) = latest {
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.select(id, cx));
        } else {
            self.open_draft(project_id, window, cx);
        }
    }

    pub fn open_notification_thread(
        &mut self,
        id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.env.read(cx).shell.threads.contains_key(&id) {
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.select(id.into(), cx));
        } else {
            tracing::warn!(thread_id = %id, "notification thread is no longer available");
        }
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        self.project_picker = None;
        self.pending_project = None;
        let selected = self.sidebar.read(cx).selected().cloned();
        if selected.is_some() {
            self.draft = None;
        }
        let current = self.thread.as_ref().map(|view| view.read(cx).thread_id());
        if selected.as_ref() != current {
            self.attachment_drafts
                .retain(|_, draft| draft.read(cx).retain_on_navigation());
            self.thread = selected.clone().map(|id| {
                let attachments = self
                    .attachment_drafts
                    .entry(id.clone())
                    .or_insert_with(|| cx.new(|_| AttachmentDraft::new(self.env.clone())))
                    .clone();
                cx.new(|cx| ThreadView::new(self.env.clone(), id, attachments, window, cx))
            });
            if let (Some(thread), Some((panel, _))) = (&self.thread, &self.changes) {
                self.changes_return_focus = window.focused(cx);
                panel.update(cx, |panel, cx| panel.set_thread(thread.clone(), window, cx));
            } else {
                self.changes = None;
                self.changes_return_focus = None;
            }
        }
        cx.notify();
    }

    fn render_main(&self, width: Pixels, cx: &mut Context<Self>) -> AnyElement {
        if let Some((settings, _)) = &self.settings {
            return settings.clone().into_any_element();
        }
        let theme = cx.theme();
        let env = self.env.read(cx);
        let thread = self
            .sidebar
            .read(cx)
            .selected()
            .and_then(|id| env.thread(id.as_ref()));

        let header = h_flex()
            .h(TOPBAR_HEIGHT)
            .flex_none()
            .px_4()
            .gap_2()
            .border_b_1()
            .bg(crate::appearance::palette(cx).color("toolbar"))
            .border_color(crate::appearance::palette(cx).color("toolbarBorder"))
            .child(
                Button::new("toggle-sidebar")
                    .icon(gpui_kit::component::Icon::new(
                        gpui_kit::assets::IconName::PanelLeft,
                    ))
                    .ghost()
                    .small()
                    .accessibility_label(if self.sidebar_visible {
                        "Hide sidebar"
                    } else {
                        "Show sidebar"
                    })
                    .tooltip(if self.sidebar_visible {
                        "Hide sidebar (Cmd/Ctrl+B)"
                    } else {
                        "Show sidebar (Cmd/Ctrl+B)"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx))),
            );
        let header = match (thread, &self.draft) {
            (_, Some(_)) => header.child(div().text_sm().font_medium().child("New thread")),
            (Some(thread), None) => {
                let project = env
                    .shell
                    .project(&thread.project_id)
                    .map(|project| project.title.clone());
                header
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .truncate()
                            .child(thread.title.clone()),
                    )
                    .when_some(project, |header, project| {
                        header.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(project),
                        )
                    })
                    .when_some(thread.branch.clone(), |header, branch| {
                        header.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(branch),
                        )
                    })
                    .child(
                        h_flex()
                            .ml_auto()
                            .gap_1()
                            .child(
                                Button::new("toggle-changes")
                                    .label("Changes")
                                    .ghost()
                                    .small()
                                    .accessibility_label("Toggle changes")
                                    .tooltip("Toggle changes (Cmd/Ctrl+D)")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_changes(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("find-in-thread")
                                    .icon(gpui_kit::component::Icon::new(
                                        gpui_kit::assets::IconName::Search,
                                    ))
                                    .ghost()
                                    .small()
                                    .accessibility_label("Find in thread")
                                    .tooltip("Find in thread (Cmd/Ctrl+F)")
                                    .disabled(!env.server.thread_find || env.rpc.is_none())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.find_in_thread(window, cx)
                                    })),
                            )
                            .child(thread_actions::button(&self.actions, &thread.id)),
                    )
            }
            (None, None) => header.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground.opacity(0.5))
                    .child("No active thread"),
            ),
        };

        let centered = |content: Div| {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p_8()
                .child(content)
                .into_any_element()
        };
        let body = match (&env.state, thread.and(self.thread.as_ref())) {
            (ConnectionState::Failed(error), _) => centered(
                v_flex()
                    .gap_2()
                    .max_w(px(720.))
                    .child(
                        div()
                            .text_base()
                            .font_medium()
                            .child("The T3 Code server stopped"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(error.clone()),
                    ),
            ),
            (_, _) if self.pending_project.is_some() => {
                centered(div().text_sm().child("Opening project…"))
            }
            (_, _) if self.draft.is_some() => v_flex()
                .flex_1()
                .min_h_0()
                .children(self.draft.as_ref().map(|(draft, _)| draft.clone()))
                .into_any_element(),
            (_, Some(view)) => {
                if let Some((panel, _)) = &self.changes {
                    if width >= px(1150.) {
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .child(div().flex_1().min_w_0().h_full().child(view.clone()))
                            .child(
                                div()
                                    .w(relative(0.55))
                                    .h_full()
                                    .min_w_0()
                                    .border_l_1()
                                    .border_color(theme.border)
                                    .child(panel.clone()),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .child(panel.clone())
                            .into_any_element()
                    }
                } else {
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .child(view.clone())
                        .into_any_element()
                }
            }
            (_, None) if env.shell.projects.is_empty() => centered(
                v_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_base()
                            .font_medium()
                            .child("What should we work on?"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Add a project to start your first thread."),
                    )
                    .child(
                        Button::new("empty-add-project")
                            .label("Add project")
                            .primary()
                            .small()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_project(window, cx)),
                            ),
                    ),
            ),
            (_, None) => centered(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_base()
                            .font_medium()
                            .child("Pick a thread to continue"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .text_color(theme.muted_foreground)
                            .child("Select an existing thread or create a new one to get started."),
                    ),
            ),
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.background)
            .child(header)
            .when_some(self.project_notice.clone(), |column, notice| {
                column.child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .text_xs()
                        .child(div().flex_1().child(notice))
                        .child(
                            Button::new("dismiss-project-notice")
                                .label("Dismiss")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.project_notice = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(
                self.actions
                    .read(cx)
                    .error
                    .clone()
                    .filter(|_| !self.actions.read(cx).has_dialog()),
                |column, error| {
                    column.child(
                        h_flex()
                            .px_4()
                            .py_2()
                            .gap_2()
                            .text_xs()
                            .child(div().flex_1().text_color(theme.danger).child(error))
                            .child(
                                Button::new("dismiss-thread-error")
                                    .label("Dismiss")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.actions
                                            .update(cx, |actions, cx| actions.dismiss_error(cx))
                                    })),
                            ),
                    )
                },
            )
            .child(body)
            .into_any_element()
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible = self.settings.is_none()
            && self.project_picker.is_none()
            && self.pending_project.is_none()
            && (self.changes.is_none() || window.viewport_size().width >= px(1150.));
        let thread_id = self
            .thread
            .as_ref()
            .map(|thread| thread.read(cx).thread_id().clone());
        self.visit.update(cx, |visit, cx| {
            visit.set_target(thread_id, visible, window, cx)
        });
        if self.restore_settings_focus {
            self.restore_settings_focus = false;
            let focus = self
                .settings_return_focus
                .take()
                .unwrap_or_else(|| self.focus.clone());
            window.on_next_frame(move |window, cx| focus.focus(window, cx));
        }
        let theme = cx.theme();
        window.set_rem_size(theme.font_size * self.zoom);
        div()
            .track_focus(&self.focus)
            .relative()
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &CloseCommandPalette, window, cx| {
                this.close_palette(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_palette(window, cx)
            }))
            .on_action(cx.listener(|this, _: &AddProject, window, cx| this.add_project(window, cx)))
            .on_action(cx.listener(
                |this, action: &crate::command_palette::OpenThread, window, cx| {
                    this.close_palette(window, cx);
                    if this.env.read(cx).thread(&action.thread_id).is_some() {
                        this.sidebar.update(cx, |sidebar, cx| {
                            sidebar.select(action.thread_id.clone(), cx)
                        });
                    }
                },
            ))
            .on_action(cx.listener(|this, _: &PreviousThread, window, cx| {
                this.navigate_thread(t3_client::navigation::Destination::Previous, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextThread, window, cx| {
                this.navigate_thread(t3_client::navigation::Destination::Next, window, cx)
            }))
            .on_action(cx.listener(|this, action: &JumpToThread, window, cx| {
                this.navigate_thread(
                    t3_client::navigation::Destination::Index(action.index),
                    window,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &ToggleThreadPin, window, cx| {
                this.thread_shortcut(t3_client::thread_actions::Shortcut::TogglePin, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleThreadSettlement, window, cx| {
                this.thread_shortcut(
                    t3_client::thread_actions::Shortcut::ToggleSettlement,
                    window,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &NewThread, window, cx| this.new_thread(window, cx)))
            .on_action(
                cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FindInThread, window, cx| this.find_in_thread(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &TogglePlanMode, window, cx| {
                    this.toggle_plan_mode(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ToggleChanges, window, cx| this.toggle_changes(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Maximize, window, _| window.zoom_window())
            .on_action(|_: &ToggleFullscreen, window, _| window.toggle_fullscreen())
            .on_action(|_: &About, window, cx| desktop::about(window, cx))
            .on_action(|_: &PasteAsText, window, cx| {
                window.dispatch_action(Box::new(gpui_kit::component::input::Paste), cx);
            })
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                this.zoom = (this.zoom * 1.2).min(3.);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                this.zoom = (this.zoom / 1.2).max(0.5);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| {
                this.zoom = 1.;
                cx.notify();
            }))
            .flex()
            .flex_col()
            .size_full()
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .when(!cfg!(target_os = "macos"), |root| {
                root.child(div().h_8().flex_none().child(self.menu.clone()))
            })
            .child(
                h_flex()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_visible, |row| {
                        row.child(
                            div()
                                .track_focus(&self.sidebar_focus)
                                .w(SIDEBAR_WIDTH)
                                .h_full()
                                .flex_none()
                                .child(self.sidebar.clone()),
                        )
                    })
                    .child(self.render_main(window.viewport_size().width, cx)),
            )
            .children(self.palette.as_ref().map(|palette| {
                let this = cx.weak_entity();
                let cancel = this.clone();
                let thread_id = self
                    .thread
                    .as_ref()
                    .filter(|_| self.settings.is_none())
                    .map(|thread| thread.read(cx).thread_id().clone());
                let can_plan = self.settings.is_none()
                    && (self
                        .draft
                        .as_ref()
                        .is_some_and(|(draft, _)| draft.read(cx).can_toggle_plan_mode(cx))
                        || self
                            .thread
                            .as_ref()
                            .is_some_and(|thread| thread.read(cx).can_toggle_plan_mode(cx)));
                let command = crate::command_palette::content(
                    palette,
                    self.env.read(cx),
                    thread_id.as_deref(),
                    can_plan,
                    self.sidebar_visible,
                )
                .on_confirm(move |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.close_palette(window, cx));
                })
                .on_cancel(move |window, cx| {
                    let cancel = cancel.clone();
                    window.defer(cx, move |window, cx| {
                        let _ = cancel.update(cx, |this, cx| this.close_palette(window, cx));
                    });
                });
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(cx.theme().background.opacity(0.7))
                    .flex()
                    .items_start()
                    .justify_center()
                    .pt(px(100.))
                    .px_4()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                    )
                    .child(
                        div()
                            .id("command-palette")
                            .role(Role::Dialog)
                            .aria_label("Command palette")
                            .w_full()
                            .max_w(px(640.))
                            .bg(cx.theme().background)
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .shadow_lg()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(command)
                            .focus_trap("command-palette", &self.palette_focus),
                    )
            }))
            .when(self.actions.read(cx).has_dialog(), |root| {
                root.child(
                    div()
                        .absolute()
                        .inset_0()
                        .occlude()
                        .bg(cx.theme().background.opacity(0.7))
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_4()
                        .child(self.actions.clone()),
                )
            })
            .children(self.project_picker.as_ref().map(|(picker, _)| {
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            if let Some((picker, _)) = &this.project_picker {
                                picker.update(cx, |picker, cx| picker.close(window, cx));
                            }
                        }),
                    )
                    .bg(cx.theme().background.opacity(0.7))
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .child(picker.clone())
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CloseCommandPalette, NextThread, PreviousThread, palette_bindings, traversal_bindings,
    };
    use gpui_kit::{Action, KeyBinding, KeyContext, Keymap, Keystroke, Modifiers};

    #[test]
    fn escape_dismisses_the_palette_instead_of_only_clearing_its_query() {
        let mut keymap = Keymap::new(vec![
            KeyBinding::new("escape", gpui_kit::base::actions::Cancel, Some("Command")),
            KeyBinding::new("escape", gpui_kit::component::input::Escape, Some("Input")),
        ]);
        keymap.add_bindings(palette_bindings());
        let typed = Keystroke::parse("escape").unwrap();
        for scopes in [
            vec!["Workspace", "Command"],
            vec!["Workspace", "Command", "Input"],
        ] {
            let contexts: Vec<_> = scopes
                .iter()
                .map(|scope| KeyContext::parse(scope).unwrap())
                .collect();
            let (bindings, _) = keymap.bindings_for_input(std::slice::from_ref(&typed), &contexts);
            assert_eq!(
                bindings.first().map(|binding| binding.action().name()),
                Some(CloseCommandPalette.name()),
                "{scopes:?}"
            );
        }
        let contexts = [
            KeyContext::parse("Workspace").unwrap(),
            KeyContext::parse("Input").unwrap(),
        ];
        let (bindings, _) = keymap.bindings_for_input(&[typed], &contexts);
        assert_eq!(
            bindings.first().map(|binding| binding.action().name()),
            Some(gpui_kit::component::input::Escape.name())
        );
    }

    #[test]
    fn traversal_accepts_linux_shifted_symbols_and_explicit_shift_brackets() {
        let bindings = traversal_bindings();
        for (key, shift, previous) in [
            ("{", false, true),
            ("}", false, false),
            ("[", true, true),
            ("]", true, false),
        ] {
            let mut modifiers = if cfg!(target_os = "macos") {
                Modifiers::command()
            } else {
                Modifiers::control()
            };
            modifiers.shift = shift;
            let typed = Keystroke {
                modifiers,
                key: key.into(),
                key_char: None,
            };
            let matched = bindings.iter().find(|binding| {
                binding.match_keystrokes(std::slice::from_ref(&typed)) == Some(false)
            });
            let expected = if previous {
                PreviousThread.name()
            } else {
                NextThread.name()
            };
            assert_eq!(
                matched.map(|binding| binding.action().name()),
                Some(expected),
                "{typed:?}"
            );
        }
    }
}
