//! A new thread before its first message: pick the project, see what it
//! will run with, and send to launch it.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::providers::{self, NewThreadDefaults, ProjectDefaults};

use crate::composer::{Composer, ComposerEvent, NewThread, Target};
use crate::environment::LocalEnvironment;

pub enum DraftEvent {
    Launched(SharedString),
    AddProject,
}

impl EventEmitter<DraftEvent> for DraftView {}

pub struct DraftView {
    env: Entity<LocalEnvironment>,
    project_id: String,
    defaults: Option<NewThreadDefaults>,
    loaded: bool,
    composer: Entity<Composer>,
    _subscriptions: Vec<Subscription>,
}

impl DraftView {
    pub fn new(
        env: Entity<LocalEnvironment>,
        project_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let target = Target::Draft(NewThread {
            project_id: project_id.clone(),
            defaults: None,
            loaded: false,
        });
        let composer = cx.new(|cx| Composer::new(env.clone(), target, None, window, cx));
        composer.update(cx, |composer, cx| composer.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe(&composer, |_, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::Launched(id) => cx.emit(DraftEvent::Launched(id.clone())),
            }),
            cx.observe_in(&env, window, |this, _, window, cx| {
                this.load(window, cx);
            }),
        ];
        let mut view = Self {
            env,
            project_id,
            defaults: None,
            loaded: false,
            composer,
            _subscriptions: subscriptions,
        };
        view.load(window, cx);
        view
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn toggle_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.composer
            .update(cx, |composer, cx| composer.toggle_plan_mode(window, cx))
    }

    fn set_project(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if project_id != self.project_id {
            self.project_id = project_id;
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
            project_id: self.project_id.clone(),
            defaults: self.defaults.clone(),
            loaded: self.loaded,
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
            (Some(_), _) => "Local".into(),
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
            .child(self.composer.clone())
    }
}
