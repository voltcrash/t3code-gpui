//! A new thread before its first message: pick the project, see what it
//! will run with, and send to launch it.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::providers::{self, NewThreadDefaults, ProjectDefaults};

use crate::attachment_draft::AttachmentDraft;
use crate::composer::{Composer, ComposerEvent, NewThread, Target};
use crate::environment::LocalEnvironment;

pub enum DraftEvent {
    Launched(SharedString),
    AddProject,
}

impl EventEmitter<DraftEvent> for DraftView {}

pub struct DraftView {
    env: Entity<LocalEnvironment>,
    id: String,
    project_id: String,
    defaults: Option<NewThreadDefaults>,
    loaded: bool,
    composer: Entity<Composer>,
    workspace: Entity<crate::draft_workspace::DraftWorkspace>,
    _subscriptions: Vec<Subscription>,
}

impl DraftView {
    pub fn new(
        env: Entity<LocalEnvironment>,
        id: String,
        project_id: String,
        attachments: Entity<AttachmentDraft>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace = cx.new(|cx| {
            crate::draft_workspace::DraftWorkspace::new(
                env.clone(),
                id.clone(),
                project_id.clone(),
                window,
                cx,
            )
        });
        let target = Target::Draft(NewThread {
            id: id.clone(),
            project_id: project_id.clone(),
            defaults: None,
            workspace: Default::default(),
            loaded: false,
        });
        let composer =
            cx.new(|cx| Composer::new(env.clone(), target, None, attachments, window, cx));
        composer.update(cx, |composer, cx| composer.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &workspace,
                window,
                |this, _, _: &crate::draft_workspace::Changed, window, cx| {
                    this.sync_composer(window, cx);
                    cx.notify();
                },
            ),
            cx.subscribe(&composer, |_, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::Launched(id) => cx.emit(DraftEvent::Launched(id.clone())),
            }),
            cx.observe_in(&env, window, |this, _, window, cx| {
                this.load(window, cx);
            }),
        ];
        let mut view = Self {
            env,
            id,
            project_id,
            defaults: None,
            loaded: false,
            composer,
            workspace,
            _subscriptions: subscriptions,
        };
        view.load(window, cx);
        view
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |composer, cx| composer.focus(window, cx));
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn can_choose_workspace(&self, cx: &App) -> bool {
        self.workspace.read(cx).can_choose(cx)
    }

    pub fn toggle_workspace(&mut self, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.toggle(cx));
    }

    pub fn open_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.open_branches(window, cx));
    }

    pub fn editor_cwd(&self, cx: &App) -> Option<String> {
        match self.workspace.read(cx).strategy(cx) {
            t3_client::workspaces::Strategy::ExistingWorktree { worktree_path, .. } => {
                Some(worktree_path)
            }
            _ => self
                .env
                .read(cx)
                .shell
                .project(&self.project_id)
                .map(|project| project.workspace_root.clone()),
        }
    }

    pub fn can_toggle_plan_mode(&self, cx: &App) -> bool {
        self.composer.read(cx).can_toggle_plan_mode(cx)
    }

    pub fn toggle_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.composer
            .update(cx, |composer, cx| composer.toggle_plan_mode(window, cx))
    }

    fn set_project(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if project_id != self.project_id {
            if self.env.read(cx).new_thread_drafts.messages.busy(&self.id) {
                return;
            }
            self.env.update(cx, |env, cx| {
                env.new_thread_drafts.set_project(&self.id, &project_id);
                cx.notify();
            });
            self.project_id = project_id;
            self.workspace.update(cx, |workspace, cx| {
                workspace.set_project(self.project_id.clone(), window, cx)
            });
            self.defaults = None;
            self.loaded = false;
            self.sync_composer(window, cx);
            self.load(window, cx);
            cx.notify();
        }
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env.read(cx);
        let loaded = env.server.settings.is_some() && env.server.providers_loaded;
        let defaults = env
            .server
            .settings
            .as_ref()
            .filter(|_| loaded)
            .and_then(|settings| {
                providers::new_thread_defaults(
                    &env.server.providers,
                    settings,
                    &ProjectDefaults {
                        id: self.project_id.clone(),
                        default_model_selection: env
                            .shell
                            .project(&self.project_id)
                            .and_then(|project| project.default_model_selection.clone()),
                    },
                )
            });
        if self.loaded != loaded || self.defaults != defaults {
            self.loaded = loaded;
            self.defaults = defaults;
            self.sync_composer(window, cx);
            cx.notify();
        }
    }

    fn sync_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        let new_thread = NewThread {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            defaults: self.defaults.clone(),
            loaded: self.loaded && self.workspace.read(cx).ready(),
            workspace: Box::new(self.workspace.read(cx).strategy(cx)),
        };
        self.composer.update(cx, |composer, cx| {
            composer.set_new_thread(new_thread, window, cx)
        });
    }
}

impl Render for DraftView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let env = self.env.read(cx);
        let project_name: SharedString = env.shell.project(&self.project_id).map_or_else(
            || "Choose a project".into(),
            |project| project.title.clone().into(),
        );
        let projects: Vec<(String, String)> = env
            .shell
            .projects
            .iter()
            .map(|project| (project.id.clone(), project.title.clone()))
            .collect();
        let current = self.project_id.clone();
        let this = cx.entity().downgrade();
        let picker = Button::new("draft-project")
            .disabled(env.new_thread_drafts.messages.busy(&self.id))
            .label(project_name)
            .ghost()
            .large()
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for (id, title) in &projects {
                    let this = this.clone();
                    let id = id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(title.clone())
                            .checked(id == current)
                            .on_click(move |_, window, cx| {
                                let id = id.clone();
                                let _ =
                                    this.update(cx, |view, cx| view.set_project(id, window, cx));
                            }),
                    );
                }
                let this = this.clone();
                menu.separator()
                    .item(PopupMenuItem::new("Add project").on_click(move |_, _, cx| {
                        let _ = this.update(cx, |_, cx| cx.emit(DraftEvent::AddProject));
                    }))
            });

        let summary: SharedString = match (&self.defaults, self.loaded) {
            (Some(_), _) => self.workspace.read(cx).strategy(cx).label().into(),
            (None, false) => "Loading providers…".into(),
            (None, true) => "No provider is ready to run this thread".into(),
        };

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_8()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .text_2xl()
                            .child("What should we build in")
                            .child(picker)
                            .child("?"),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .when(self.loaded && self.defaults.is_none(), |row| {
                                row.child(
                                    gpui_kit::component::Icon::new(IconName::TriangleAlert)
                                        .xsmall(),
                                )
                            })
                            .child(summary),
                    ),
            )
            .child(
                h_flex().w_full().justify_center().px_6().child(
                    div()
                        .w_full()
                        .max_w(crate::appearance::CONTENT_WIDTH)
                        .child(self.workspace.clone()),
                ),
            )
            .child(self.composer.clone())
    }
}
