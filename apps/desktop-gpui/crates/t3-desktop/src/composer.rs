//! The message box under a thread: send a follow-up, stop the running turn,
//! and answer what the agent is blocked on.
//!
//! An approval or a question takes over the box the way it does on the web
//! (`ChatComposer.tsx`): text typed under an approval declines it, and text
//! typed under a question is that question's own answer.

use std::collections::HashMap;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;
use t3_client::commands;
use t3_client::providers::{self, NewThreadDefaults};
use t3_client::requests::{
    self, Capability, Draft, PendingApproval, PendingRequests, PendingUserInput, Question,
};
use t3_client::thread::ThreadSettings;

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
const MAX_WIDTH: Pixels = px(768.);

/// The question whose answer the text box currently holds.
#[derive(Clone, PartialEq)]
struct Binding {
    request_id: String,
    question_id: String,
}

/// Where a message goes: an existing thread, or a new one in a project.
pub enum Target {
    Thread(SharedString),
    Draft(NewThread),
}

/// A draft's project and the defaults it launches with; `defaults` stays
/// `None` while they load and when no provider can run.
pub struct NewThread {
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
    requests: PendingRequests,
    /// Per request, per question.
    drafts: HashMap<String, HashMap<String, Draft>>,
    question_index: HashMap<String, usize>,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder(PLACEHOLDER)
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
                    }
                    cx.notify();
                }
                let pending = thread.read(cx).pending();
                if *pending != this.requests {
                    this.requests = pending.clone();
                    this.sync(window, cx);
                }
            }));
        }
        let mut composer = Self {
            env,
            target,
            input,
            requests: PendingRequests::default(),
            drafts: HashMap::new(),
            question_index: HashMap::new(),
            binding: None,
            stashed_message: None,
            busy: false,
            error: None,
            placeholder: PLACEHOLDER,
            thread_settings: None,
            picked_model: None,
            picked_runtime_mode: None,
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
        if let Target::Draft(_) = self.target {
            if let Target::Draft(previous) = &self.target
                && previous.project_id != new_thread.project_id
            {
                self.picked_model = None;
                self.picked_runtime_mode = None;
                self.error = None;
            }
            self.target = Target::Draft(new_thread);
            self.sync(window, cx);
        }
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
        let base = self.base_settings()?.0;
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
                .child(mode_menu),
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
                .question_index
                .get(&input.request_id)
                .copied()
                .unwrap_or(0)
                .min(last);
            return Mode::Question(input, index);
        }
        Mode::Message
    }

    fn draft(&self, request_id: &str, question: &Question) -> Draft {
        self.drafts
            .get(request_id)
            .and_then(|drafts| drafts.get(&question.id))
            .cloned()
            .unwrap_or_else(|| Draft::seeded(question))
    }

    fn draft_mut(&mut self, request_id: &str, question: &Question) -> &mut Draft {
        self.drafts
            .entry(request_id.to_owned())
            .or_default()
            .entry(question.id.clone())
            .or_insert_with(|| Draft::seeded(question))
    }

    /// Points the text box and placeholder at whatever the mode needs.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live_ids: Vec<&String> = self
            .requests
            .user_inputs
            .iter()
            .map(|input| &input.request_id)
            .collect();
        self.drafts.retain(|id, _| live_ids.contains(&id));
        self.question_index.retain(|id, _| live_ids.contains(&id));

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
        }
        cx.notify();
    }

    fn can_send(&self, cx: &App) -> bool {
        let env = self.env.read(cx);
        let ready = match &self.target {
            Target::Thread(_) => true,
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
            if text.trim().is_empty() {
                return;
            }
            let (thread_id, input) = commands::launch_thread(&commands::NewThread {
                project_id: &draft.project_id,
                model_selection: &model_selection,
                runtime_mode: &runtime_mode,
                text: &text,
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
                if text.trim().is_empty() {
                    return;
                }
                // `persistThreadSettingsForNextTurn`: the mode lands
                // before the turn that should run under it.
                commands::send_turn(
                    &thread,
                    &text,
                    self.changed_model().as_ref(),
                    self.changed_runtime_mode(cx).as_deref(),
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
        self.question_index.insert(request_id.to_owned(), index);
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
        self.busy = true;
        self.error = None;
        cx.notify();
        let dispatched = tokio_bridge::spawn(cx, async move {
            for (method, input) in &calls {
                rpc.call::<_, Value>(method, input).await?;
            }
            Ok::<_, t3_client::RpcError>(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match dispatched.await {
                Ok(result) => result.map_err(|error| error.detail()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if clears_input {
                            this.input.update(cx, |input, cx| input.clean(window, cx));
                        }
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
            (theme.danger, theme.warning, theme.border, theme.background)
        };
        let text = self.input.read(cx).value();
        let has_text = !text.trim().is_empty();
        let can_send = self.can_send(cx);
        let pickers = self.render_pickers(cx);

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
                        .disabled(!can_send || !has_text),
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
                    .disabled(!can_send || !has_text),
                false,
            ),
        };
        let action = action
            .primary()
            .rounded(ButtonRounded::Large)
            .loading(self.busy);
        let action = if action_is_stop(&self.mode(), self.is_running(cx), has_text) {
            action
        } else {
            action.on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
        };

        h_flex().w_full().justify_center().px_6().pb_4().child(
            v_flex()
                .w_full()
                .max_w(MAX_WIDTH)
                .gap_1()
                .when_some(self.error.clone(), |column, error| {
                    column.child(div().px_1().text_xs().text_color(danger).child(error))
                })
                .child(
                    v_flex()
                        .w_full()
                        .p_2()
                        .gap_1()
                        .rounded_xl()
                        .border_1()
                        .border_color(if panel.is_some() {
                            warning.opacity(0.5)
                        } else {
                            border
                        })
                        .bg(background)
                        .children(panel)
                        .child(
                            Textarea::new(&self.input)
                                .appearance(false)
                                .disabled(input_disabled),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .justify_between()
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
