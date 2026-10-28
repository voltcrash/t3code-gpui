//! Mounted only for expanded commands; output is never executed or parsed as markup.
use crate::{environment::LocalEnvironment, tokio_bridge};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{base::SelectableText, *};
use t3_client::command_output::{self, Response, Source};

pub struct CommandOutput {
    env: Entity<LocalEnvironment>,
    source: Source,
    output: Option<SharedString>,
    exit_code: Option<i64>,
    error: Option<SharedString>,
    task: Option<Task<()>>,
}
impl CommandOutput {
    pub fn new(env: Entity<LocalEnvironment>, source: Source, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            env,
            exit_code: source.exit_code,
            output: source.output.clone().map(Into::into),
            source,
            error: None,
            task: None,
        };
        if view.source.omitted {
            view.load(cx);
        }
        view
    }
    pub fn set_source(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.source == source {
            return;
        }
        self.task = None;
        self.exit_code = source.exit_code;
        self.output = source.output.clone().map(Into::into);
        self.source = source;
        self.error = None;
        if self.source.omitted {
            self.load(cx);
        }
        cx.notify();
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() {
            return;
        }
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            self.error = Some("Reconnect to load command output.".into());
            cx.notify();
            return;
        };
        self.error = None;
        let request = self.source.request.clone();
        let reading = tokio_bridge::spawn(cx, async move {
            rpc.call::<_, Response>(command_output::GET, &request).await
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = reading.await;
            let _ = this.update(cx, |view, cx| {
                view.task = None;
                match result {
                    Ok(Ok(Response { item: Some(item) })) if view.source.request.accepts(&item) => {
                        view.output = command_output::output(&item).map(Into::into);
                        view.exit_code = item
                            .fields
                            .get("exitCode")
                            .and_then(serde_json::Value::as_i64);
                    }
                    Ok(Ok(_)) => {
                        view.error =
                            Some("Command output is no longer available at this revision.".into())
                    }
                    Ok(Err(error)) => {
                        view.error = Some(format!("Could not load command output: {error}").into())
                    }
                    Err(error) => {
                        view.error = Some(format!("Could not load command output: {error}").into())
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}
impl Render for CommandOutput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Output")
                    .when_some(self.exit_code, |header, code| {
                        header.child(format!("Exit {code}"))
                    })
                    .when_some(self.output.clone(), |header, output| {
                        header.child(
                            Clipboard::new("copy-command-output")
                                .value(output)
                                .tooltip("Copy command output")
                                .accessibility_label("Copy command output"),
                        )
                    }),
            )
            .when(self.task.is_some(), |body| {
                body.child(div().text_xs().child("Loading output…"))
            })
            .when_some(self.error.clone(), |body, error| {
                body.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().text_color(cx.theme().danger).child(error))
                        .child(
                            Button::new("retry-output")
                                .ghost()
                                .xsmall()
                                .label("Retry loading output")
                                .disabled(self.task.is_some())
                                .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                        ),
                )
            })
            .when(self.task.is_none() && self.error.is_none(), |body| {
                body.child(
                    div()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .whitespace_normal()
                        .child(SelectableText::new(
                            "command-output",
                            self.output.clone().unwrap_or_else(|| "No output.".into()),
                        )),
                )
            })
    }
}
