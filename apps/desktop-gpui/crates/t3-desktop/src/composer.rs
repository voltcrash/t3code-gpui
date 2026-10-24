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
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;
use t3_client::commands;
use t3_client::requests::{
    self, Capability, Draft, PendingApproval, PendingRequests, PendingUserInput, Question,
};

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
const MAX_WIDTH: Pixels = px(768.);

/// The question whose answer the text box currently holds.
#[derive(Clone, PartialEq)]
struct Binding {
    request_id: String,
    question_id: String,
}

pub struct Composer {
    env: Entity<LocalEnvironment>,
    thread_id: SharedString,
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
        thread: Entity<ThreadView>,
        thread_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder(PLACEHOLDER)
        });
        let subscriptions =
            vec![
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                        InputEvent::Change => this.input_changed(cx),
                        _ => {}
                    }
                }),
                cx.observe_in(&env, window, |this, _, window, cx| this.sync(window, cx)),
                cx.observe_in(&thread, window, |this, thread, window, cx| {
                    let pending = thread.read(cx).pending();
                    if *pending != this.requests {
                        this.requests = pending.clone();
                        this.sync(window, cx);
                    }
                }),
            ];
        let mut composer = Self {
            env,
            thread_id,
            input,
            requests: PendingRequests::default(),
            drafts: HashMap::new(),
            question_index: HashMap::new(),
            binding: None,
            stashed_message: None,
            busy: false,
            error: None,
            placeholder: PLACEHOLDER,
            _subscriptions: subscriptions,
        };
        composer.sync(window, cx);
        composer
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
            (_, _, Mode::Message) => PLACEHOLDER,
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
        env.rpc.is_some() && env.commands_supported && !self.busy
    }

    /// Whether the thread has a turn to stop, as the shell reports it.
    fn is_running(&self, cx: &App) -> bool {
        self.env
            .read(cx)
            .shell
            .threads
            .get(self.thread_id.as_ref())
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
        let thread = self.thread_id.to_string();
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
                        .chain([commands::send_message(&thread, &text)])
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
                vec![commands::send_message(&thread, &text)]
            }
        };
        let clears_input = !matches!(self.mode(), Mode::Question(..));
        self.run(commands, clears_input, window, cx);
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
        let command = commands::respond_to_approval(&self.thread_id, &request_id, &decision, None);
        self.run(vec![command], false, window, cx);
    }

    fn dismiss(&mut self, request_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let command = commands::dismiss_user_input(&self.thread_id, &request_id);
        self.run(vec![command], false, window, cx);
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let command = commands::stop_thread(&self.thread_id);
        self.run(vec![command], false, window, cx);
    }

    /// Sends commands in order and stops at the first failure. A sent message
    /// clears the box only once the server accepted it, so a failure keeps
    /// the text for another try.
    fn run(
        &mut self,
        commands: Vec<Value>,
        clears_input: bool,
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
            for command in &commands {
                commands::dispatch(&rpc, command).await?;
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
                    Ok(()) if clears_input => {
                        this.input.update(cx, |input, cx| input.clean(window, cx));
                    }
                    Ok(()) => {}
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
                        .child(h_flex().w_full().justify_end().child(action)),
                ),
        )
    }
}

fn action_is_stop(mode: &Mode<'_>, running: bool, has_text: bool) -> bool {
    matches!(mode, Mode::Message) && running && !has_text
}
