//! Persisted attachments are resolved by the environment, including after reconnect.

use crate::environment::LocalEnvironment;
use crate::tokio_bridge;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::sync::Arc;
use t3_client::attachments::Attachment;

pub struct AttachmentView {
    env: Entity<LocalEnvironment>,
    attachment: Attachment,
    image: Option<Arc<RenderImage>>,
    task: Option<Task<()>>,
    error: Option<SharedString>,
    saved: bool,
    _env: Subscription,
}
impl AttachmentView {
    pub fn new(
        env: Entity<LocalEnvironment>,
        attachment: Attachment,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&env, |_, _, cx| cx.notify());
        Self {
            env,
            attachment,
            image: None,
            task: None,
            error: None,
            saved: false,
            _env: observe,
        }
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env.read(cx);
        let (Some(client), Some(rpc)) = (env.attachments.clone(), env.rpc.clone()) else {
            return;
        };
        let attachment = self.attachment.clone();
        let reading = tokio_bridge::spawn(cx, async move {
            let bytes = client
                .download(&rpc, &attachment)
                .await
                .map_err(|error| error.to_string())?;
            tokio::task::spawn_blocking(move || crate::attachment_image::thumbnail(&bytes))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())
        });
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.task = None;
                match result {
                    Ok(Ok(image)) => {
                        this.image = Some(image);
                        this.error = None;
                    }
                    _ => {
                        this.error =
                            Some("Could not load this image. Retry after reconnecting.".into())
                    }
                }
                cx.notify();
            });
        }));
    }
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&dir, Some(&self.attachment.name));
        self.error = None;
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = prompt.await;
            let _ = this.update_in(cx, |this, _window, cx| {
                this.task = None;
                match result {
                    Ok(Ok(Some(path))) => {
                        let env = this.env.read(cx);
                        let (Some(client), Some(rpc)) = (env.attachments.clone(), env.rpc.clone())
                        else {
                            this.error = Some("Reconnect to save this attachment.".into());
                            cx.notify();
                            return;
                        };
                        let attachment = this.attachment.clone();
                        let saving = tokio_bridge::spawn(cx, async move {
                            let bytes = client
                                .download(&rpc, &attachment)
                                .await
                                .map_err(|error| error.to_string())?;
                            tokio::task::spawn_blocking(move || std::fs::write(path, bytes))
                                .await
                                .map_err(|error| error.to_string())?
                                .map_err(|error| error.to_string())
                        });
                        this.task = Some(cx.spawn(async move |this, cx| {
                            let result = saving.await;
                            let _ = this.update(cx, |this, cx| {
                                this.task = None;
                                match result {
                                    Ok(Ok(())) => this.saved = true,
                                    _ => {
                                        this.error =
                                            Some("Could not save the attachment. Try again.".into())
                                    }
                                }
                                cx.notify();
                            });
                        }));
                    }
                    Ok(Ok(None)) => {}
                    _ => this.error = Some("Could not open the save dialog.".into()),
                }
                cx.notify();
            });
        }));
    }
}
impl Render for AttachmentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let preview = self.attachment.kind == "image"
            && matches!(
                self.attachment.mime_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp" | "image/gif"
            );
        if preview && self.image.is_none() && self.task.is_none() && self.error.is_none() {
            self.load(window, cx);
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .when(preview, |column| {
                column.child(
                    h_flex()
                        .w_full()
                        .h(px(128.))
                        .justify_center()
                        .items_center()
                        .overflow_hidden()
                        .when_some(self.image.clone(), |image, data| {
                            image.child(img(data).h(px(128.)).w_auto().flex_none())
                        })
                        .when(self.image.is_none(), |image| {
                            image.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Image preview"),
                            )
                        }),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().text_xs().child(format!(
                        "{} · {} KiB",
                        self.attachment.name,
                        self.attachment.size_bytes.div_ceil(1024)
                    )))
                    .when(self.saved, |row| row.child(div().text_xs().child("Saved")))
                    .child(
                        Button::new("save-attachment")
                            .label("Save")
                            .ghost()
                            .xsmall()
                            .disabled(self.task.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
            .when_some(self.error.clone(), |column, error| {
                column.child(
                    h_flex()
                        .gap_1()
                        .child(div().text_xs().text_color(cx.theme().danger).child(error))
                        .when(preview, |row| {
                            row.child(
                                Button::new("retry-image")
                                    .label("Retry")
                                    .ghost()
                                    .xsmall()
                                    .disabled(self.task.is_some())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.error = None;
                                        this.load(window, cx);
                                    })),
                            )
                        }),
                )
            })
    }
}
