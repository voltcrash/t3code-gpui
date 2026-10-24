//! A new thread before its first message: pick the project, see what it
//! will run with, and send to launch it.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{Value, json};
use t3_client::providers::{self, NewThreadDefaults, ProjectDefaults, Settings};

use crate::composer::{Composer, ComposerEvent, NewThread, Target};
use crate::environment::LocalEnvironment;
use crate::tokio_bridge;

pub enum DraftEvent {
    Launched(SharedString),
}

impl EventEmitter<DraftEvent> for DraftView {}

pub struct DraftView {
    env: Entity<LocalEnvironment>,
    project_id: String,
    defaults: Option<NewThreadDefaults>,
    loaded: bool,
    composer: Entity<Composer>,
    _load: Option<Task<()>>,
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
            // A reconnect may follow a settings or provider change.
            cx.observe_in(&env, window, |this, env, window, cx| {
                if env.read(cx).rpc.is_some() && !this.loaded && this._load.is_none() {
                    this.load(window, cx);
                }
            }),
        ];
        let mut view = Self {
            env,
            project_id,
            defaults: None,
            loaded: false,
            composer,
            _load: None,
            _subscriptions: subscriptions,
        };
        view.load(window, cx);
        view
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    fn set_project(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if project_id != self.project_id {
            self.project_id = project_id;
            self.load(window, cx);
        }
    }

    /// Reads providers and settings now rather than at connect time, so the
    /// draft shows what a send would really use.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            return;
        };
        self.loaded = false;
        self.defaults = None;
        self.sync_composer(window, cx);
        let project = {
            let env = self.env.read(cx);
            ProjectDefaults {
                id: self.project_id.clone(),
                default_model_selection: env
                    .shell
                    .project(&self.project_id)
                    .and_then(|project| project.default_model_selection.clone()),
            }
        };
        let fetched = tokio_bridge::spawn(cx, async move {
            let config = rpc.call::<_, Value>("server.getConfig", &json!({})).await?;
            let settings = rpc
                .call::<_, Value>("server.getSettings", &json!({}))
                .await?;
            Ok::<_, t3_client::RpcError>((config, settings))
        });
        self._load = Some(cx.spawn_in(window, async move |this, cx| {
            let defaults = match fetched.await {
                Ok(Ok((config, settings))) => {
                    let settings: Settings = serde_json::from_value(settings).unwrap_or_default();
                    providers::new_thread_defaults(
                        &providers::from_config(&config),
                        &settings,
                        &project,
                    )
                }
                Ok(Err(error)) => {
                    tracing::warn!("loading new-thread defaults failed: {error}");
                    None
                }
                Err(error) => {
                    tracing::warn!("loading new-thread defaults failed: {error}");
                    None
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.defaults = defaults;
                this.loaded = true;
                this._load = None;
                this.sync_composer(window, cx);
                cx.notify();
            });
        }));
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
                menu
            });

        let summary: SharedString = match (&self.defaults, self.loaded) {
            (Some(defaults), _) => format!(
                "{} · {} · Local",
                defaults.provider_label, defaults.model_label
            )
            .into(),
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
