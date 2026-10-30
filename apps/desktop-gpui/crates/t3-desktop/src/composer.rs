//! The message box under a thread: send a follow-up, stop the running turn,
//! and answer what the agent is blocked on.
//!
//! An approval or a question takes over the box the way it does on the web
//! (`ChatComposer.tsx`): text typed under an approval declines it, and text
//! typed under a question is that question's own answer.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;
use t3_client::commands;
use t3_client::planning::{self, ProposedPlan};
use t3_client::providers::{self, NewThreadDefaults};
use t3_client::requests::{
    self, Capability, Draft, PendingApproval, PendingRequests, PendingUserInput, Question,
};
use t3_client::thread::ThreadSettings;

use crate::appearance::CONTENT_WIDTH;
use crate::attachment_draft::AttachmentDraft;
use crate::environment::LocalEnvironment;
use crate::thread_view::ThreadView;
use crate::tokio_bridge;

const PLACEHOLDER: &str = "Ask anything...";
/// `DISCONNECTED_COMPOSER_PLACEHOLDER`.
const DISCONNECTED_PLACEHOLDER: &str = "Ask for changes, send follow-ups, or attach images";
const UNSUPPORTED_PLACEHOLDER: &str = "Update the T3 Code server to send messages from here";
const FEEDBACK_PLACEHOLDER: &str = "Send feedback to decline this request";
const REDIRECT_PLACEHOLDER: &str = "Send a message to decline and redirect the agent";
const CHOICE_PLACEHOLDER: &str = "Choose an option above";
const ANSWER_PLACEHOLDER: &str =
    "Type your own answer, or leave this blank to use the selected option";
const PROVIDER_GONE: &str = "Provider process is gone. Interrupt or restart the run to respond.";
const NO_PROVIDER_PLACEHOLDER: &str = "Enable a provider in Settings to send a message";

/// The question whose answer the text box currently holds.
#[derive(Clone, PartialEq)]
struct Binding {
    request_id: String,
    question_id: String,
}

/// Where a message goes: an existing thread, or a new one in a project.
#[derive(Clone)]
pub enum Target {
    Thread(SharedString),
    Draft(NewThread),
}

impl Target {
    fn draft_id(&self) -> &str {
        match self {
            Self::Thread(id) => id,
            Self::Draft(draft) => &draft.id,
        }
    }

    fn drafts<'a>(&self, env: &'a LocalEnvironment) -> &'a t3_client::message_drafts::Drafts {
        match self {
            Self::Thread(_) => &env.message_drafts,
            Self::Draft(_) => &env.new_thread_drafts.messages,
        }
    }

    fn drafts_mut<'a>(
        &self,
        env: &'a mut LocalEnvironment,
    ) -> &'a mut t3_client::message_drafts::Drafts {
        match self {
            Self::Thread(_) => &mut env.message_drafts,
            Self::Draft(_) => &mut env.new_thread_drafts.messages,
        }
    }
}

/// A draft's project and the defaults it launches with; `defaults` stays
/// `None` while they load and when no provider can run.
#[derive(Clone)]
pub struct NewThread {
    pub id: String,
    pub project_id: String,
    pub defaults: Option<NewThreadDefaults>,
    pub loaded: bool,
}

pub enum ComposerEvent {
    /// The server created the draft's thread and took its first message.
    Launched(SharedString),
}

impl EventEmitter<ComposerEvent> for Composer {}

pub struct Composer {
    env: Entity<LocalEnvironment>,
    target: Target,
    input: Entity<TextareaState>,
    attachments: Entity<AttachmentDraft>,
    requests: PendingRequests,
    /// Per request, per question.
    question_drafts: t3_client::question_drafts::State,
    requests_loaded: bool,
    binding: Option<Binding>,
    /// The message the person was writing before a question took the box.
    stashed_message: Option<SharedString>,
    /// A command is in flight; its button shows that instead of guessing.
    busy: bool,
    error: Option<SharedString>,
    placeholder: &'static str,
    /// The open thread's own model and mode; `None` for a draft.
    thread_settings: Option<ThreadSettings>,
    /// Picks that apply from the next send; `None` keeps the thread's or
    /// the draft's defaults.
    picked_model: Option<Value>,
    picked_runtime_mode: Option<String>,
    picked_interaction_mode: Option<String>,
    proposed_plan: Option<Rc<ProposedPlan>>,
    _subscriptions: Vec<Subscription>,
}

enum Mode<'a> {
    Approval(&'a PendingApproval),
    Question(&'a PendingUserInput, usize),
    Message,
}

impl Composer {
    pub fn new(
        env: Entity<LocalEnvironment>,
        target: Target,
        thread: Option<&Entity<ThreadView>>,
        attachments: Entity<AttachmentDraft>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let restored: SharedString = target
            .drafts(env.read(cx))
            .text(target.draft_id())
            .to_owned()
            .into();
        let picks = target
            .drafts(env.read(cx))
            .picks(target.draft_id())
            .cloned()
            .unwrap_or_default();
        let question_drafts = match &target {
            Target::Thread(id) => env.read(cx).question_drafts.get(id),
            Target::Draft(_) => Default::default(),
        };
        let input = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder(PLACEHOLDER);
            input.set_value(restored, window, cx);
            input
        });
        let mut subscriptions =
            vec![
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                        InputEvent::Change => this.input_changed(cx),
                        _ => {}
                    }
                }),
                cx.observe_in(&env, window, |this, _, window, cx| this.sync(window, cx)),
                cx.observe(&attachments, |_, _, cx| cx.notify()),
                cx.observe(&crate::preferences::global(cx), |_, _, cx| cx.notify()),
            ];
        if let Some(thread) = thread {
            subscriptions.push(cx.observe_in(thread, window, |this, thread, window, cx| {
                let settings = thread.read(cx).settings();
                if settings != this.thread_settings {
                    this.thread_settings = settings;
                    if let Some(settings) = &this.thread_settings {
                        if this.picked_model.as_ref() == Some(&settings.model_selection) {
                            this.picked_model = None;
                        }
                        if this.picked_runtime_mode.as_ref() == Some(&settings.runtime_mode) {
                            this.picked_runtime_mode = None;
                        }
                        if this.picked_interaction_mode.as_ref() == Some(&settings.interaction_mode)
                        {
                            this.picked_interaction_mode = None;
                        }
                    }
                    this.store_picks(cx);
                    cx.notify();
                }
                let proposed = thread.read(cx).proposed_plan();
                if proposed.as_ref().map(Rc::as_ptr) != this.proposed_plan.as_ref().map(Rc::as_ptr)
                {
                    this.proposed_plan = proposed;
                    cx.notify();
                }
                let pending = thread.read(cx).pending();
                let loaded = thread.read(cx).requests_ready();
                if *pending != this.requests || loaded != this.requests_loaded {
                    this.requests = pending.clone();
                    this.requests_loaded = loaded;
                    this.sync(window, cx);
                }
            }));
        }
        let mut composer = Self {
            env,
            target,
            input,
            attachments,
            requests: PendingRequests::default(),
            question_drafts,
            requests_loaded: false,
            binding: None,
            stashed_message: None,
            busy: false,
            error: None,
            placeholder: PLACEHOLDER,
            thread_settings: None,
            picked_model: picks.model,
            picked_runtime_mode: picks.runtime_mode,
            picked_interaction_mode: picks.interaction_mode,
            proposed_plan: None,
            _subscriptions: subscriptions,
        };
        composer.sync(window, cx);
        composer
    }

    /// Updates a draft's project and defaults; a thread composer ignores it.
    pub fn set_new_thread(
        &mut self,
        new_thread: NewThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Target::Draft(previous) = &self.target {
            let project_changed = previous.project_id != new_thread.project_id;
            if project_changed {
                self.picked_model = None;
                self.picked_runtime_mode = None;
                self.picked_interaction_mode = None;
                self.error = None;
            }
            self.target = Target::Draft(new_thread);
            if project_changed {
                self.store_picks(cx);
            }
            self.sync(window, cx);
        }
    }

    fn store_picks(&self, cx: &mut Context<Self>) {
        let picks = t3_client::message_drafts::Picks {
            model: self.picked_model.clone(),
            runtime_mode: self.picked_runtime_mode.clone(),
            interaction_mode: self.picked_interaction_mode.clone(),
        };
        self.env.update(cx, |env, cx| {
            if self
                .target
                .drafts_mut(env)
                .set_picks(self.target.draft_id(), picks)
            {
                cx.notify();
            }
        });
    }

    /// The model and mode a send would use without picks, and whether a
    /// provider session can block changing models.
    fn base_settings(&self) -> Option<(Value, String, bool)> {
        match &self.target {
            Target::Thread(_) => self.thread_settings.as_ref().map(|settings| {
                (
                    settings.model_selection.clone(),
                    settings.runtime_mode.clone(),
                    settings.has_runtime,
                )
            }),
            Target::Draft(draft) => draft.defaults.as_ref().map(|defaults| {
                (
                    defaults.model_selection.clone(),
                    defaults.runtime_mode.clone(),
                    false,
                )
            }),
        }
    }

    fn effective_model(&self) -> Option<Value> {
        self.picked_model
            .clone()
            .or_else(|| self.base_settings().map(|(model, ..)| model))
    }

    fn effective_runtime_mode(&self, cx: &App) -> Option<String> {
        let requested = self
            .picked_runtime_mode
            .clone()
            .or_else(|| self.base_settings().map(|(_, mode, _)| mode))?;
        let model = self.effective_model()?;
        let env = self.env.read(cx);
        let provider =
            env.server.providers.iter().find(|provider| {
                Some(provider.instance_id.as_str()) == model["instanceId"].as_str()
            });
        Some(providers::resolve_runtime_mode(provider, &requested))
    }

    fn plan_mode_enabled(&self, cx: &App) -> bool {
        let model = self.effective_model();
        let env = self.env.read(cx);
        let provider = env.server.providers.iter().find(|provider| {
            Some(provider.instance_id.as_str())
                == model
                    .as_ref()
                    .and_then(|model| model["instanceId"].as_str())
        });
        planning::enabled(
            crate::preferences::global(cx)
                .read(cx)
                .value
                .plan_mode_enabled,
            provider,
        )
    }

    fn effective_interaction_mode(&self, cx: &App) -> &'static str {
        let requested = self
            .picked_interaction_mode
            .as_deref()
            .or_else(|| {
                self.thread_settings
                    .as_ref()
                    .map(|settings| settings.interaction_mode.as_str())
            })
            .unwrap_or("default");
        if self.plan_mode_enabled(cx) && requested == "plan" {
            "plan"
        } else {
            "default"
        }
    }

    fn changed_interaction_mode(&self, cx: &App) -> Option<&'static str> {
        let mode = self.effective_interaction_mode(cx);
        (self
            .thread_settings
            .as_ref()
            .map(|settings| settings.interaction_mode.as_str())
            != Some(mode))
        .then_some(mode)
    }

    pub fn can_toggle_plan_mode(&self, cx: &App) -> bool {
        !self.busy && self.plan_mode_enabled(cx)
    }

    pub fn toggle_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.can_toggle_plan_mode(cx) {
            return false;
        }
        self.picked_interaction_mode = Some(
            if self.effective_interaction_mode(cx) == "plan" {
                "default"
            } else {
                "plan"
            }
            .into(),
        );
        self.store_picks(cx);
        self.focus(window, cx);
        cx.notify();
        true
    }

    fn plan_follow_up(&self, cx: &App) -> Option<&ProposedPlan> {
        (matches!(self.mode(), Mode::Message)
            && self.effective_interaction_mode(cx) == "plan"
            && !self.is_running(cx)
            && self.attachments.read(cx).is_empty())
        .then_some(self.proposed_plan.as_deref())
        .flatten()
    }

    /// The pick when it differs from what the thread already runs.
    fn changed_model(&self) -> Option<Value> {
        let (base, ..) = self.base_settings()?;
        self.picked_model.clone().filter(|picked| *picked != base)
    }

    fn changed_runtime_mode(&self, cx: &App) -> Option<String> {
        let (_, base, _) = self.base_settings()?;
        self.effective_runtime_mode(cx).filter(|mode| *mode != base)
    }

    fn pick_model(&mut self, selection: Value, cx: &mut Context<Self>) {
        let env = self.env.read(cx);
        let Some(instance) = selection["instanceId"].as_str() else {
            return;
        };
        let Some(provider) = env
            .server
            .providers
            .iter()
            .find(|provider| provider.instance_id == instance)
        else {
            return;
        };
        let enabled = env.server.settings.as_ref().is_some_and(|settings| {
            settings.instance_enabled(self.project_id(cx).as_deref(), provider)
        });
        if !enabled
            || provider.status != "ready"
            || provider.availability.as_deref() == Some("unavailable")
        {
            self.error = Some("This provider is no longer available".into());
            cx.notify();
            return;
        }
        if let Some((base, _, has_runtime)) = self.base_settings()
            && let Some(reason) =
                providers::model_change_block(&env.server.providers, has_runtime, &base, &selection)
        {
            self.error = Some(reason.into());
        } else {
            self.error = None;
            self.picked_model = Some(selection);
            self.store_picks(cx);
        }
        cx.notify();
    }

    fn selection_error(&self, cx: &App) -> Option<&'static str> {
        let picked = self.picked_model.as_ref()?;
        let env = self.env.read(cx);
        if let Some((base, _, has_runtime)) = self.base_settings()
            && let Some(reason) =
                providers::model_change_block(&env.server.providers, has_runtime, &base, picked)
        {
            return Some(reason);
        }
        let provider =
            env.server.providers.iter().find(|provider| {
                Some(provider.instance_id.as_str()) == picked["instanceId"].as_str()
            });
        let available = provider.is_some_and(|provider| {
            provider.status == "ready"
                && provider.availability.as_deref() != Some("unavailable")
                && env.server.settings.as_ref().is_some_and(|settings| {
                    settings.instance_enabled(self.project_id(cx).as_deref(), provider)
                })
        });
        (!available)
            .then_some("This provider is no longer available. Choose another model before sending.")
    }

    fn project_id(&self, cx: &App) -> Option<String> {
        match &self.target {
            Target::Draft(draft) => Some(draft.project_id.clone()),
            Target::Thread(id) => self
                .env
                .read(cx)
                .shell
                .threads
                .get(id.as_ref())
                .map(|thread| thread.project_id.clone()),
        }
    }

    /// The model and runtime-mode menus, `ProviderModelPicker` and the
    /// runtime mode control of `ComposerControlsBar`.
    fn render_pickers(&self, cx: &mut Context<Self>) -> Option<Div> {
        let model = self.effective_model()?;
        let runtime_mode = self.effective_runtime_mode(cx)?;
        let env = self.env.read(cx);
        let settings = env.server.settings.clone().unwrap_or_default();
        let instance_id = model["instanceId"].as_str().unwrap_or_default().to_owned();
        let provider = env
            .server
            .providers
            .iter()
            .find(|provider| provider.instance_id == instance_id)
            .cloned();
        let has_runtime = self
            .base_settings()
            .is_some_and(|(_, _, has_runtime)| has_runtime);
        let started = self
            .thread_settings
            .as_ref()
            .is_some_and(|settings| settings.started);
        let base = self
            .base_settings()
            .map(|settings| settings.0)
            .unwrap_or_else(|| model.clone());
        let locked_instance = base["instanceId"].as_str().unwrap_or_default();
        let locked_driver = started.then(|| {
            if let Some(driver) = self
                .thread_settings
                .as_ref()
                .and_then(|settings| settings.provider_driver.clone())
            {
                return driver;
            }
            env.server
                .providers
                .iter()
                .find(|provider| provider.instance_id == locked_instance)
                .map_or_else(
                    || locked_instance.to_owned(),
                    |provider| provider.driver.clone(),
                )
        });
        let entries = providers::picker_providers(
            &env.server.providers,
            &settings,
            self.project_id(cx).as_deref(),
            locked_driver.as_deref(),
        );
        let slug = model["model"].as_str().unwrap_or_default().to_owned();
        let model_label: SharedString = match &provider {
            Some(provider) => format!(
                "{} · {}",
                provider.display_name(),
                providers::picker_models(provider, &settings)
                    .iter()
                    .find(|model| model.slug == slug)
                    .map_or_else(|| provider.model_name(&slug), |model| model.name.clone())
            ),
            None => slug.clone(),
        }
        .into();
        let this = cx.entity().downgrade();
        let catalog = env.server.providers.clone();
        let model_menu = Button::new("model-picker")
            .label(model_label.clone())
            .tooltip(model_label)
            .max_w_full()
            .ghost()
            .xsmall()
            .dropdown_caret(true)
            .disabled(self.busy)
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.max_h(px(360.)).scrollable(true);
                for (ix, entry) in entries.iter().enumerate() {
                    if ix > 0 {
                        menu = menu.separator();
                    }
                    menu = menu.label(if entry.ready {
                        entry.label.clone()
                    } else {
                        format!("{} · Unavailable", entry.label)
                    });
                    for candidate in &entry.models {
                        let selection =
                            providers::select_model(&base, &entry.instance_id, &candidate.slug);
                        let blocked =
                            providers::model_change_block(&catalog, has_runtime, &base, &selection);
                        let this = this.clone();
                        menu = menu.item(
                            PopupMenuItem::new(candidate.name.clone())
                                .checked(entry.instance_id == instance_id && candidate.slug == slug)
                                .disabled(!entry.ready || blocked.is_some())
                                .on_click(move |_, _, cx| {
                                    let selection = selection.clone();
                                    let _ = this.update(cx, |composer, cx| {
                                        composer.pick_model(selection, cx)
                                    });
                                }),
                        );
                    }
                }
                menu
            });

        let modes = providers::runtime_modes_for(provider.as_ref());
        let (mode_label, mode_description) = providers::runtime_mode_label(&runtime_mode);
        let this = cx.entity().downgrade();
        let mode_menu = Button::new("runtime-mode-picker")
            .label(mode_label)
            .tooltip(mode_description)
            .ghost()
            .xsmall()
            .dropdown_caret(true)
            .disabled(self.busy)
            .dropdown_menu(move |mut menu, _, _| {
                for mode in &modes {
                    let (label, _) = providers::runtime_mode_label(mode);
                    let this = this.clone();
                    let mode = (*mode).to_owned();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(mode == runtime_mode)
                            .on_click(move |_, _, cx| {
                                let mode = mode.clone();
                                let _ = this.update(cx, |composer, cx| {
                                    composer.picked_runtime_mode = Some(mode);
                                    composer.store_picks(cx);
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });
        Some(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_1()
                .child(model_menu)
                .child(mode_menu)
                .when(self.plan_mode_enabled(cx), |row| {
                    row.child(
                        Button::new("interaction-mode")
                            .label(if self.effective_interaction_mode(cx) == "plan" {
                                "Plan"
                            } else {
                                "Build"
                            })
                            .ghost()
                            .xsmall()
                            .selected(self.effective_interaction_mode(cx) == "plan")
                            .accessibility_label("Toggle Plan mode")
                            .tooltip("Toggle Plan/Build (Shift+Tab)")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_plan_mode(window, cx);
                            })),
                    )
                }),
        )
    }

    fn thread_id(&self) -> Option<&str> {
        match &self.target {
            Target::Thread(id) => Some(id),
            Target::Draft(_) => None,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Approvals come first, as on the web; then the oldest question.
    fn mode(&self) -> Mode<'_> {
        if let Some(approval) = self.requests.approvals.first() {
            return Mode::Approval(approval);
        }
        if let Some(input) = self.requests.user_inputs.first() {
            let last = input.questions.len().saturating_sub(1);
            let index = self
                .question_drafts
                .indices
                .get(&input.request_id)
                .copied()
                .unwrap_or(0)
                .min(last);
            return Mode::Question(input, index);
        }
        Mode::Message
    }

    fn draft(&self, request_id: &str, question: &Question) -> Draft {
        self.question_drafts.draft(request_id, question)
    }

    fn draft_mut(&mut self, request_id: &str, question: &Question) -> &mut Draft {
        self.question_drafts.draft_mut(request_id, question)
    }

    fn store_question_drafts(&self, cx: &mut Context<Self>) {
        if let Some(id) = self.thread_id() {
            self.env.update(cx, |env, _| {
                env.question_drafts.edit(id, self.question_drafts.clone());
            });
        }
    }

    /// Points the text box and placeholder at whatever the mode needs.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        {
            let id = self.target.draft_id();
            let drafts = self.target.drafts(self.env.read(cx));
            self.busy = drafts.busy(id);
            let picks = drafts.picks(id).cloned().unwrap_or_default();
            self.picked_model = picks.model;
            self.picked_runtime_mode = picks.runtime_mode;
            self.picked_interaction_mode = picks.interaction_mode;
            let message = drafts.text(id);
            if self.binding.is_some() {
                if self.stashed_message.as_deref() != Some(message) {
                    self.stashed_message = Some(message.to_owned().into());
                }
            } else if self.input.read(cx).value().as_ref() != message {
                let message = message.to_owned();
                self.input
                    .update(cx, |input, cx| input.set_value(message, window, cx));
            }
        }
        if let Some(id) = self.thread_id() {
            self.question_drafts = self.env.read(cx).question_drafts.get(id);
        }
        // The initial empty request list precedes the thread snapshot.
        if self.requests_loaded {
            self.question_drafts.prune(&self.requests);
            self.store_question_drafts(cx);
        }

        let (binding, text) = match self.mode() {
            Mode::Question(input, index) => match input.questions.get(index) {
                Some(question) => {
                    let draft = self.draft(&input.request_id, question);
                    (
                        Some(Binding {
                            request_id: input.request_id.clone(),
                            question_id: question.id.clone(),
                        }),
                        draft.custom.unwrap_or_default(),
                    )
                }
                None => (None, String::new()),
            },
            _ => (None, String::new()),
        };
        if binding == self.binding
            && binding.is_some()
            && self.input.read(cx).value().as_ref() != text
        {
            self.input
                .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        }
        if binding != self.binding {
            let value = match (&self.binding, &binding) {
                (None, Some(_)) => {
                    self.stashed_message = Some(self.input.read(cx).value());
                    text.into()
                }
                (Some(_), None) => self.stashed_message.take().unwrap_or_default(),
                _ => text.into(),
            };
            self.binding = binding;
            self.input
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }

        let env = self.env.read(cx);
        let placeholder = match (&env.rpc, env.commands_supported, self.mode()) {
            (None, _, _) => DISCONNECTED_PLACEHOLDER,
            (Some(_), false, _) => UNSUPPORTED_PLACEHOLDER,
            (_, _, Mode::Approval(approval)) if approval.supports_feedback => FEEDBACK_PLACEHOLDER,
            (_, _, Mode::Approval(_)) => REDIRECT_PLACEHOLDER,
            (_, _, Mode::Question(input, index)) => match input.questions.get(index) {
                Some(question) if !question.allows_custom_answer() => CHOICE_PLACEHOLDER,
                _ => ANSWER_PLACEHOLDER,
            },
            (_, _, Mode::Message) => match &self.target {
                Target::Draft(draft) if draft.loaded && draft.defaults.is_none() => {
                    NO_PROVIDER_PLACEHOLDER
                }
                _ => PLACEHOLDER,
            },
        };
        if placeholder != self.placeholder {
            self.placeholder = placeholder;
            self.input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        cx.notify();
    }

    /// Typing under a question is its answer, and replaces a picked option
    /// (`setPendingUserInputCustomAnswer`).
    fn input_changed(&mut self, cx: &mut Context<Self>) {
        if let Mode::Question(input, index) = self.mode()
            && let Some(question) = input.questions.get(index).cloned()
        {
            let request_id = input.request_id.clone();
            let text = self.input.read(cx).value().to_string();
            let draft = self.draft_mut(&request_id, &question);
            if question.initial_answer.is_none() && !text.trim().is_empty() {
                draft.selected.clear();
            }
            draft.custom = Some(text);
            self.store_question_drafts(cx);
        } else {
            let text = self.input.read(cx).value();
            self.env.update(cx, |env, _| {
                self.target
                    .drafts_mut(env)
                    .edit(self.target.draft_id(), &text);
            });
        }
        cx.notify();
    }

    fn can_send(&self, cx: &App) -> bool {
        let env = self.env.read(cx);
        let ready = match &self.target {
            Target::Thread(id) => env.shell.threads.contains_key(id.as_ref()),
            Target::Draft(draft) => draft.defaults.is_some(),
        };
        env.rpc.is_some() && env.commands_supported && !self.busy && ready
    }

    /// Whether the thread has a turn to stop, as the shell reports it.
    fn is_running(&self, cx: &App) -> bool {
        let Some(thread_id) = self.thread_id() else {
            return false;
        };
        self.env
            .read(cx)
            .shell
            .threads
            .get(thread_id)
            .and_then(|thread| thread.runtime_status())
            .is_some_and(|status| {
                matches!(
                    status,
                    "preparing" | "queued" | "starting" | "running" | "waiting"
                )
            })
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let text = self.input.read(cx).value();
        let attachments = if matches!(self.mode(), Mode::Message) {
            let Some(uploads) = self.attachments.read(cx).ready() else {
                self.error = Some(
                    "Wait for attachments to upload, or retry or remove failed uploads.".into(),
                );
                cx.notify();
                return;
            };
            uploads
                .iter()
                .map(|upload| upload.attachment.clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let sends_turn = matches!(self.mode(), Mode::Message)
            || matches!(self.mode(), Mode::Approval(approval) if !approval.supports_feedback);
        if sends_turn && let Some(error) = self.selection_error(cx) {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        if let Target::Draft(draft) = &self.target {
            let (Some(_), Mode::Message) = (&draft.defaults, self.mode()) else {
                return;
            };
            let (Some(model_selection), Some(runtime_mode)) =
                (self.effective_model(), self.effective_runtime_mode(cx))
            else {
                return;
            };
            if text.trim().is_empty() && attachments.is_empty() {
                return;
            }
            let (thread_id, input) = commands::launch_thread(&commands::NewThread {
                project_id: &draft.project_id,
                model_selection: &model_selection,
                runtime_mode: &runtime_mode,
                interaction_mode: self.effective_interaction_mode(cx),
                text: &text,
                attachments: &attachments,
            });
            let calls = vec![(commands::LAUNCH_THREAD, input)];
            self.run(calls, true, Some(thread_id.into()), window, cx);
            return;
        }
        let Some(thread) = self.thread_id().map(str::to_owned) else {
            return;
        };
        let commands = match self.mode() {
            Mode::Approval(approval) => {
                if text.trim().is_empty() {
                    return;
                }
                if approval.supports_feedback {
                    vec![commands::respond_to_approval(
                        &thread,
                        &approval.request_id,
                        "decline",
                        Some(text.trim()),
                    )]
                } else {
                    // `declinePendingApprovals`: one left pending would block
                    // the agent from reading the redirect.
                    self.requests
                        .approvals
                        .iter()
                        .filter(|approval| approval.live)
                        .map(|approval| {
                            commands::respond_to_approval(
                                &thread,
                                &approval.request_id,
                                "decline",
                                None,
                            )
                        })
                        .chain(commands::send_turn(
                            &thread,
                            &text,
                            self.changed_model().as_ref(),
                            self.changed_runtime_mode(cx).as_deref(),
                            self.changed_interaction_mode(cx),
                            &[],
                        ))
                        .collect()
                }
            }
            Mode::Question(input, index) => {
                if input.capability == Capability::NotResumable {
                    return;
                }
                let Some(question) = input.questions.get(index) else {
                    return;
                };
                let draft = self.draft(&input.request_id, question);
                if requests::resolve_answer(question, Some(&draft)).is_none() {
                    return;
                }
                if index + 1 < input.questions.len() {
                    let request_id = input.request_id.clone();
                    self.go_to_question(&request_id, index + 1, window, cx);
                    return;
                }
                let drafts: HashMap<String, Draft> = input
                    .questions
                    .iter()
                    .map(|question| (question.id.clone(), self.draft(&input.request_id, question)))
                    .collect();
                let Some(answers) = requests::build_answers(&input.questions, &drafts) else {
                    return;
                };
                vec![commands::respond_to_user_input(
                    &thread,
                    &input.request_id,
                    answers,
                )]
            }
            Mode::Message => {
                if let Some(plan) = self.plan_follow_up(cx) {
                    let commands = commands::plan_follow_up(
                        &thread,
                        plan,
                        &text,
                        self.changed_model().as_ref(),
                        self.changed_runtime_mode(cx).as_deref(),
                    );
                    self.dispatch(commands, true, window, cx);
                    return;
                }
                if text.trim().is_empty() && attachments.is_empty() {
                    return;
                }
                // `persistThreadSettingsForNextTurn`: the mode lands
                // before the turn that should run under it.
                commands::send_turn(
                    &thread,
                    &text,
                    self.changed_model().as_ref(),
                    self.changed_runtime_mode(cx).as_deref(),
                    self.changed_interaction_mode(cx),
                    &attachments,
                )
            }
        };
        let clears_input = !matches!(self.mode(), Mode::Question(..));
        self.dispatch(commands, clears_input, window, cx);
    }

    fn go_to_question(
        &mut self,
        request_id: &str,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.question_drafts
            .indices
            .insert(request_id.to_owned(), index);
        self.store_question_drafts(cx);
        self.sync(window, cx);
    }

    fn toggle_option(
        &mut self,
        request_id: &str,
        question: &Question,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_mut(request_id, question).toggle(question, value);
        self.store_question_drafts(cx);
        // The typed answer would win over the pick, so it goes.
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn respond_to_approval(
        &mut self,
        request_id: String,
        decision: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_send(cx) {
            return;
        }
        let Some(thread) = self.thread_id() else {
            return;
        };
        let command = commands::respond_to_approval(thread, &request_id, &decision, None);
        self.dispatch(vec![command], false, window, cx);
    }

    fn dismiss(&mut self, request_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let Some(thread) = self.thread_id() else {
            return;
        };
        let command = commands::dismiss_user_input(thread, &request_id);
        self.dispatch(vec![command], false, window, cx);
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let Some(thread) = self.thread_id() else {
            return;
        };
        let command = commands::stop_thread(thread);
        self.dispatch(vec![command], false, window, cx);
    }

    fn dispatch(
        &mut self,
        commands: Vec<Value>,
        clears_input: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let calls = commands
            .into_iter()
            .map(|command| (commands::DISPATCH, command))
            .collect();
        self.run(calls, clears_input, None, window, cx);
    }

    /// Makes the calls in order and stops at the first failure. A sent message
    /// clears the box only once the server accepted it, so a failure keeps
    /// the text for another try.
    fn run(
        &mut self,
        calls: Vec<(&'static str, Value)>,
        clears_input: bool,
        launched: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            return;
        };
        let target = self.target.clone();
        let clears_text = clears_input && self.binding.is_none();
        let Some(submission) = self.env.update(cx, |env, cx| {
            let submission = target.drafts_mut(env).begin(target.draft_id(), clears_text);
            cx.notify();
            submission
        }) else {
            return;
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let sends_attachments = matches!(self.mode(), Mode::Message)
            && calls.iter().any(|(method, input)| {
                *method == commands::LAUNCH_THREAD || input["type"] == "message.dispatch"
            });
        let uploads = if sends_attachments {
            self.attachments.read(cx).ready().unwrap_or_default()
        } else {
            Vec::new()
        };
        if sends_attachments {
            self.attachments
                .update(cx, |draft, cx| draft.set_locked(true, cx));
        }
        let dispatched = tokio_bridge::spawn(cx, async move {
            for (method, input) in &calls {
                rpc.call::<_, Value>(method, input).await?;
            }
            for upload in &uploads {
                upload.mark_sent();
            }
            Ok::<_, t3_client::RpcError>(())
        });
        let env = self.env.clone();
        let attachments = self.attachments.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = match dispatched.await {
                Ok(result) => result.map_err(|error| error.detail()),
                Err(error) => Err(error.to_string()),
            };
            if sends_attachments {
                attachments.update(cx, |draft, cx| {
                    draft.set_locked(false, cx);
                    if result.is_ok() {
                        draft.clear(cx);
                    }
                });
            }
            // Finish against environment state even if navigation dropped this composer.
            env.update(cx, |env, cx| {
                let picks_submission = submission.clone();
                match &target {
                    Target::Thread(_) => env.message_drafts.finish(submission, result.is_ok()),
                    Target::Draft(draft) => {
                        env.new_thread_drafts
                            .finish(&draft.id, submission, result.is_ok())
                    }
                }
                if sends_attachments && result.is_ok() {
                    target
                        .drafts_mut(env)
                        .clear_submitted_interaction(&picks_submission);
                }
                cx.notify();
            });
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if sends_attachments {
                            this.picked_interaction_mode = None;
                        }
                        if clears_input && this.binding.is_some() {
                            this.input.update(cx, |input, cx| input.clean(window, cx));
                        }
                        this.sync(window, cx);
                        if let Some(thread_id) = launched {
                            cx.emit(ComposerEvent::Launched(thread_id));
                        }
                    }
                    Err(error) => {
                        tracing::warn!("command failed: {error}");
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_approval(&self, approval: &PendingApproval, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let count = self.requests.approvals.len();
        let detail: SharedString = if approval.live {
            approval
                .detail
                .clone()
                .unwrap_or_else(|| approval.label().into())
                .into()
        } else {
            PROVIDER_GONE.into()
        };
        let enabled = approval.live && self.can_send(cx);
        let buttons = approval.options.iter().map(|option| {
            let request_id = approval.request_id.clone();
            let decision = option.decision.clone();
            let button = Button::new(SharedString::from(format!("approval-{}", option.decision)))
                .label(option.label.clone())
                .small()
                .disabled(!enabled)
                .when_some(option.warning.clone(), |button, warning| {
                    button.tooltip(warning)
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.respond_to_approval(request_id.clone(), decision.clone(), window, cx)
                }));
            if option.decision == "accept" {
                button.primary()
            } else {
                button.outline()
            }
        });
        v_flex()
            .w_full()
            .gap_1()
            .px_1()
            .pb_2()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex_none()
                            .font_medium()
                            .text_color(theme.warning)
                            .child(approval.label()),
                    )
                    .when_some(approval.app_name.clone(), |row, app| {
                        row.child(div().min_w_0().truncate().child(app))
                    })
                    .when(count > 1, |row| {
                        row.child(div().ml_auto().child(format!("1/{count}")))
                    }),
            )
            .child(
                div()
                    .id("approval-detail")
                    .w_full()
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .text_xs()
                    .when(approval.detail_is_code() && approval.live, |detail| {
                        detail.font_family(theme.mono_font_family.clone())
                    })
                    .child(detail),
            )
            .when_some(approval.reason.clone(), |panel, reason| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(reason),
                )
            })
            .when_some(approval.full_access_override.clone(), |panel, kind| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(requests::full_access_override_explanation(&kind)),
                )
            })
            .child(h_flex().pt_1().gap_2().flex_wrap().children(buttons))
    }

    fn render_question(
        &self,
        input: &PendingUserInput,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let Some(question) = input.questions.get(index) else {
            return v_flex();
        };
        let draft = self.draft(&input.request_id, question);
        let live = input.capability != Capability::NotResumable;
        let enabled = live && self.can_send(cx);
        let options = question.options.iter().enumerate().map(|(ix, option)| {
            let value = option.answer_value().to_owned();
            let selected = draft.selected.contains(&value);
            let request_id = input.request_id.clone();
            let question = question.clone();
            h_flex()
                .id(SharedString::from(format!("option-{ix}")))
                .w_full()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_lg()
                .border_1()
                .border_color(if selected {
                    theme.primary
                } else {
                    theme.border
                })
                .when(selected, |row| row.bg(theme.secondary))
                .when(enabled, |row| {
                    row.cursor_pointer()
                        .hover(|row| row.bg(theme.secondary))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_option(&request_id, &question, &value, window, cx)
                        }))
                })
                .child(
                    div().flex_none().size_4().child(
                        Icon::new(IconName::Check)
                            .xsmall()
                            .when(!selected, |icon| icon.text_color(theme.transparent)),
                    ),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().text_sm().child(option.label.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(option.description.clone()),
                        ),
                )
        });
        let request_id = input.request_id.clone();
        let back_request = input.request_id.clone();
        v_flex()
            .w_full()
            .gap_1p5()
            .px_1()
            .pb_2()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().font_medium().child(question.header.clone()))
                    .when(input.questions.len() > 1, |row| {
                        row.child(div().ml_auto().child(format!(
                            "{}/{}",
                            index + 1,
                            input.questions.len()
                        )))
                    }),
            )
            .child(div().text_sm().child(if live {
                question.question.clone()
            } else {
                PROVIDER_GONE.into()
            }))
            .children(options)
            .child(
                h_flex()
                    .gap_2()
                    .when(index > 0, |row| {
                        row.child(
                            Button::new("question-back")
                                .label("Back")
                                .small()
                                .ghost()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.go_to_question(&back_request, index - 1, window, cx)
                                })),
                        )
                    })
                    .when(input.dismissible, |row| {
                        row.child(
                            Button::new("question-dismiss")
                                .label("Dismiss")
                                .small()
                                .ghost()
                                .disabled(!self.can_send(cx))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.dismiss(request_id.clone(), window, cx)
                                })),
                        )
                    }),
            )
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (danger, warning, border, background) = {
            let theme = cx.theme();
            (
                theme.danger,
                theme.warning,
                theme.border,
                crate::appearance::palette(cx).color("surfaceRaised"),
            )
        };
        let text = self.input.read(cx).value();
        let message_mode = matches!(self.mode(), Mode::Message);
        let has_attachments = message_mode && !self.attachments.read(cx).is_empty();
        let attachments_ready = !message_mode || self.attachments.read(cx).ready().is_some();
        let has_text = !text.trim().is_empty() || has_attachments;
        let can_send = self.can_send(cx);
        let pickers = self.render_pickers(cx);
        let plan_follow_up = self.plan_follow_up(cx).is_some();

        let (panel, action, input_disabled) = match self.mode() {
            Mode::Approval(approval) => {
                let approval = approval.clone();
                (
                    Some(self.render_approval(&approval, cx)),
                    Button::new("send")
                        .icon(Icon::new(Lucide::ArrowUp))
                        .tooltip(if approval.supports_feedback {
                            "Decline with feedback"
                        } else {
                            "Decline and send"
                        })
                        .disabled(!can_send || !has_text || !attachments_ready),
                    false,
                )
            }
            Mode::Question(input, index) => {
                let input = input.clone();
                let question = input.questions.get(index).cloned();
                let resolved = question.as_ref().is_some_and(|question| {
                    requests::resolve_answer(
                        question,
                        Some(&self.draft(&input.request_id, question)),
                    )
                    .is_some()
                });
                let last = index + 1 >= input.questions.len();
                let live = input.capability != Capability::NotResumable;
                (
                    Some(self.render_question(&input, index, cx)),
                    Button::new("send")
                        .icon(Icon::new(Lucide::ArrowUp))
                        .tooltip(if last {
                            "Submit answers"
                        } else {
                            "Next question"
                        })
                        .disabled(!can_send || !live || !resolved),
                    !live
                        || question
                            .as_ref()
                            .is_some_and(|question| !question.allows_custom_answer()),
                )
            }
            Mode::Message if plan_follow_up => (
                None,
                Button::new("send")
                    .label(if has_text { "Refine" } else { "Implement" })
                    .tooltip(if has_text {
                        "Refine the proposed plan"
                    } else {
                        "Implement the proposed plan in this thread"
                    })
                    .disabled(!can_send),
                false,
            ),
            Mode::Message if self.is_running(cx) && !has_text => (
                None,
                Button::new("stop")
                    .icon(Icon::new(Lucide::Square))
                    .tooltip("Stop generation")
                    .disabled(!can_send)
                    .on_click(cx.listener(|this, _, window, cx| this.stop(window, cx))),
                false,
            ),
            Mode::Message => (
                None,
                Button::new("send")
                    .icon(Icon::new(Lucide::ArrowUp))
                    .tooltip(if self.is_running(cx) {
                        "Send follow-up"
                    } else {
                        "Submit message"
                    })
                    .disabled(!can_send || !has_text || !attachments_ready),
                false,
            ),
        };
        let action = action
            .primary()
            .rounded(ButtonRounded::Size(px(999.)))
            .loading(self.busy);
        let action = if action_is_stop(&self.mode(), self.is_running(cx), has_text) {
            action
        } else {
            action.on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
        };

        let attach_enabled = message_mode && !self.busy && self.env.read(cx).rpc.is_some();
        h_flex()
            .w_full()
            .justify_center()
            .px_6()
            .pb_4()
            .when(self.plan_mode_enabled(cx), |row| {
                row.key_context("NativePlanComposer")
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if matches!(this.mode(), Mode::Message) && !this.busy {
                    this.attachments.update(cx, |draft, cx| {
                        draft.add_paths(paths.0.to_vec(), window, cx)
                    });
                }
            }))
            .child(
                v_flex()
                    .w_full()
                    .max_w(CONTENT_WIDTH)
                    .gap_1()
                    .when_some(self.env.read(cx).draft_storage_error.clone(), |column, error| {
                        column.child(
                            h_flex().gap_2().child(
                                div().px_1().text_xs().text_color(danger).child(error)
                            ).child(
                                Button::new("retry-draft-storage")
                                    .label("Retry")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.env.update(cx, |env, cx| env.retry_draft_storage(cx));
                                    }))
                            )
                        )
                    })
                    .when_some(self.error.clone(), |column, error| {
                        column.child(div().px_1().text_xs().text_color(danger).child(error))
                    })
                    .child(
                        v_flex()
                            .w_full()
                            .p_2()
                            .gap_1()
                            .rounded_3xl()
                            .border_1()
                            .border_color(if panel.is_some() {
                                warning.opacity(0.5)
                            } else {
                                border
                            })
                            .bg(background)
                            .children(panel)
                            .when(plan_follow_up, |column| column.child(div().px_1().text_xs().text_color(cx.theme().muted_foreground).child("Plan ready. Add feedback to refine it, or implement it.")))
                            .when(message_mode, |column| {
                                column.child(self.attachments.clone())
                            })
                            .child(
                                Textarea::new(&self.input)
                                    .appearance(false)
                                    .disabled(input_disabled || self.busy)
                                    .on_paste({
                                        let draft = self.attachments.clone();
                                        move |clipboard, window, cx| {
                                            attach_enabled
                                                && draft.update(cx, |draft, cx| {
                                                    draft.paste(clipboard, window, cx)
                                                })
                                        }
                                    }),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .justify_between()
                                    .child(
                                        Button::new("attach-files")
                                            .icon(Icon::new(Lucide::Paperclip))
                                            .ghost()
                                            .xsmall()
                                            .tooltip("Attach files")
                                            .accessibility_label("Attach files")
                                            .disabled(!attach_enabled)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.attachments
                                                    .update(cx, |draft, cx| draft.pick(window, cx));
                                            })),
                                    )
                                    .child(div().flex_1().min_w_0().children(pickers))
                                    .child(div().flex_none().child(action)),
                            ),
                    ),
            )
    }
}

fn action_is_stop(mode: &Mode<'_>, running: bool, has_text: bool) -> bool {
    matches!(mode, Mode::Message) && running && !has_text
}
