//! The message box under a thread: send a follow-up, or stop the running turn.

use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;
use t3_client::commands;

use crate::environment::LocalEnvironment;
use crate::tokio_bridge;
use gpui_kit::assets::IconName as Lucide;

const PLACEHOLDER: &str = "Ask anything...";
/// `DISCONNECTED_COMPOSER_PLACEHOLDER`.
const DISCONNECTED_PLACEHOLDER: &str = "Ask for changes, send follow-ups, or attach images";
const UNSUPPORTED_PLACEHOLDER: &str = "Update the T3 Code server to send messages from here";
const MAX_WIDTH: Pixels = px(768.);

pub struct Composer {
    env: Entity<LocalEnvironment>,
    thread_id: SharedString,
    input: Entity<TextareaState>,
    /// A command is in flight; its button shows that instead of guessing.
    busy: bool,
    error: Option<SharedString>,
    placeholder: &'static str,
    _subscriptions: Vec<Subscription>,
}

impl Composer {
    pub fn new(
        env: Entity<LocalEnvironment>,
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
                        InputEvent::Change => cx.notify(),
                        _ => {}
                    }
                }),
                cx.observe_in(&env, window, |this, _, window, cx| {
                    this.sync_placeholder(window, cx)
                }),
            ];
        let mut composer = Self {
            env,
            thread_id,
            input,
            busy: false,
            error: None,
            placeholder: PLACEHOLDER,
            _subscriptions: subscriptions,
        };
        composer.sync_placeholder(window, cx);
        composer
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env.read(cx);
        let placeholder = match (&env.rpc, env.commands_supported) {
            (None, _) => DISCONNECTED_PLACEHOLDER,
            (Some(_), false) => UNSUPPORTED_PLACEHOLDER,
            (Some(_), true) => PLACEHOLDER,
        };
        if placeholder != self.placeholder {
            self.placeholder = placeholder;
            self.input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
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
        let text = self.input.read(cx).value();
        if text.trim().is_empty() || !self.can_send(cx) {
            return;
        }
        let command = commands::send_message(&self.thread_id, &text);
        self.run(command, true, window, cx);
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let command = commands::stop_thread(&self.thread_id);
        self.run(command, false, window, cx);
    }

    /// Sends one command. A sent message clears the box only once the server
    /// accepted it, so a failure keeps the text for another try.
    fn run(
        &mut self,
        command: Value,
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
        let dispatched =
            tokio_bridge::spawn(cx, async move { commands::dispatch(&rpc, &command).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = match dispatched.await {
                Ok(result) => result.map_err(|error| error.detail()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(_) if clears_input => {
                        this.input.update(cx, |input, cx| input.clean(window, cx));
                    }
                    Ok(_) => {}
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
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let has_text = !self.input.read(cx).value().trim().is_empty();
        let can_send = self.can_send(cx);
        let stopping = self.is_running(cx) && !has_text;

        let action = if stopping {
            Button::new("stop")
                .icon(Icon::new(Lucide::Square))
                .tooltip("Stop generation")
                .on_click(cx.listener(|this, _, window, cx| this.stop(window, cx)))
        } else {
            Button::new("send")
                .icon(Icon::new(Lucide::ArrowUp))
                .tooltip(if self.is_running(cx) {
                    "Send follow-up"
                } else {
                    "Submit message"
                })
                .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
        }
        .primary()
        .rounded(ButtonRounded::Large)
        .loading(self.busy)
        .disabled(!can_send || (!stopping && !has_text));

        h_flex().w_full().justify_center().px_6().pb_4().child(
            v_flex()
                .w_full()
                .max_w(MAX_WIDTH)
                .gap_1()
                .when_some(self.error.clone(), |column, error| {
                    column.child(div().px_1().text_xs().text_color(theme.danger).child(error))
                })
                .child(
                    v_flex()
                        .w_full()
                        .p_2()
                        .gap_1()
                        .rounded_xl()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .child(Textarea::new(&self.input).appearance(false))
                        .child(h_flex().w_full().justify_end().child(action)),
                ),
        )
    }
}
