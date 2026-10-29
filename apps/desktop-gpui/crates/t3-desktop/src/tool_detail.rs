//! Mounted only for expanded tools; arguments and output remain selectable plain text.
use crate::{attachment_view::AttachmentView, environment::LocalEnvironment, tokio_bridge};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{base::SelectableText, *};
use t3_client::tool_detail::{self, Response, Source};

pub struct ToolDetail {
    env: Entity<LocalEnvironment>,
    source: Source,
    input: Option<SharedString>,
    output: Option<SharedString>,
    images: Vec<Entity<AttachmentView>>,
    exit_code: Option<i64>,
    error: Option<SharedString>,
    task: Option<Task<()>>,
}
impl ToolDetail {
    pub fn new(env: Entity<LocalEnvironment>, source: Source, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            env,
            input: source.input.clone().map(Into::into),
            exit_code: source.exit_code,
            output: source.output.clone().map(Into::into),
            images: Vec::new(),
            source,
            error: None,
            task: None,
        };
        view.set_images(view.source.images.clone(), cx);
        if view.source.omitted {
            view.load(cx);
        }
        view
    }
    pub fn set_source(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.source == source {
            return;
        }
        // Streamed arguments can change without changing the live revision.
        // Keep its existing read until completion gives us a new revision.
        if self.source.request == source.request && self.source.omitted && source.omitted {
            self.source = source;
            return;
        }
        self.task = None;
        self.set_images(source.images.clone(), cx);
        self.input = source.input.clone().map(Into::into);
        self.exit_code = source.exit_code;
        self.output = source.output.clone().map(Into::into);
        self.source = source;
        self.error = None;
        if self.source.omitted {
            self.load(cx);
        }
        cx.notify();
    }
    fn set_images(&mut self, images: Vec<t3_client::tool_images::Image>, cx: &mut Context<Self>) {
        self.images = images
            .into_iter()
            .map(|image| cx.new(|cx| AttachmentView::tool_image(self.env.clone(), image, cx)))
            .collect();
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() {
            return;
        }
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            self.error = Some("Reconnect to load tool details.".into());
            cx.notify();
            return;
        };
        self.error = None;
        let request = self.source.request.clone();
        let reading = tokio_bridge::spawn(cx, async move {
            rpc.call::<_, Response>(tool_detail::GET, &request).await
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = reading.await;
            let _ = this.update(cx, |view, cx| {
                view.task = None;
                match result {
                    Ok(Ok(Response { item: Some(item) })) if view.source.request.accepts(&item) => {
                        view.set_images(
                            t3_client::tool_images::images(
                                &item,
                                &view.source.request.thread_id,
                                &view.source.request.item_id,
                            ),
                            cx,
                        );
                        view.input = tool_detail::input(&item).map(Into::into);
                        view.output = tool_detail::output(&item).map(Into::into);
                        view.exit_code = item
                            .fields
                            .get("exitCode")
                            .and_then(serde_json::Value::as_i64);
                    }
                    Ok(Ok(_)) => {
                        view.error =
                            Some("Tool detail is no longer available at this revision.".into())
                    }
                    Ok(Err(error)) => {
                        view.error = Some(format!("Could not load tool details: {error}").into())
                    }
                    Err(error) => {
                        view.error = Some(format!("Could not load tool details: {error}").into())
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}
impl Render for ToolDetail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .when_some(self.input.clone(), |body, input| {
                body.child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Arguments")
                                .child(
                                    Clipboard::new("copy-tool-arguments")
                                        .value(input.clone())
                                        .tooltip("Copy tool arguments")
                                        .accessibility_label("Copy tool arguments"),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .whitespace_normal()
                                .child(SelectableText::new("tool-arguments", input)),
                        ),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.source.request.kind.heading())
                    .when_some(self.exit_code, |header, code| {
                        header.child(format!("Exit {code}"))
                    })
                    .when_some(self.output.clone(), |header, output| {
                        header.child(
                            Clipboard::new("copy-command-output")
                                .value(output)
                                .tooltip(self.source.request.kind.copy_label())
                                .accessibility_label(self.source.request.kind.copy_label()),
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
                                .label("Retry loading details")
                                .disabled(self.task.is_some())
                                .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                        ),
                )
            })
            .children(self.images.iter().cloned())
            .when(self.task.is_none() && self.error.is_none(), |body| {
                body.child(
                    div()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .whitespace_normal()
                        .child(SelectableText::new(
                            "command-output",
                            self.output.clone().unwrap_or_else(|| {
                                if self.source.request.kind.is_search() {
                                    if self.source.request.revision == "live" {
                                        "Searching…".into()
                                    } else {
                                        "No results.".into()
                                    }
                                } else if self.source.request.kind == tool_detail::Kind::FileChange
                                {
                                    "No file details.".into()
                                } else {
                                    "No output.".into()
                                }
                            }),
                        )),
                )
            })
    }
}
