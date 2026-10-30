//! Draft uploads outlive an individual render, and are released by their last owner.

use crate::environment::LocalEnvironment;
use crate::tokio_bridge;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::PathBuf;
use std::sync::Arc;
use t3_client::attachments::{self, Metadata, Pending};

#[derive(Clone)]
enum Source {
    Path(PathBuf),
    Image { name: String, bytes: Arc<Vec<u8>> },
}
impl Source {
    fn name(&self) -> String {
        match self {
            Self::Path(path) => path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            Self::Image { name, .. } => name.clone(),
        }
    }
    fn read(self) -> Result<(Metadata, Vec<u8>), attachments::Error> {
        match self {
            Self::Path(path) => attachments::read_file(&path),
            Self::Image { name, bytes } => Ok((
                attachments::metadata(&name, bytes.len() as u64)?,
                bytes.as_ref().clone(),
            )),
        }
    }
}
struct Item {
    id: u64,
    source: Source,
    metadata: Option<Metadata>,
    pending: Option<Arc<Pending>>,
    preview: Option<Arc<RenderImage>>,
    error: Option<SharedString>,
    task: Option<Task<()>>,
}
pub struct AttachmentDraft {
    env: Entity<LocalEnvironment>,
    items: Vec<Item>,
    next_id: u64,
    picking: Option<Task<()>>,
    reading: Arc<tokio::sync::Semaphore>,
    error: Option<SharedString>,
    locked: bool,
}
impl AttachmentDraft {
    pub fn new(env: Entity<LocalEnvironment>) -> Self {
        Self {
            env,
            items: Vec::new(),
            next_id: 0,
            picking: None,
            reading: Arc::new(tokio::sync::Semaphore::new(4)),
            error: None,
            locked: false,
        }
    }
    pub fn set_locked(&mut self, locked: bool, cx: &mut Context<Self>) {
        self.locked = locked;
        cx.notify();
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn retain_on_navigation(&self) -> bool {
        // A picker can return after navigation, before any item exists.
        !self.items.is_empty() || self.picking.is_some()
    }
    pub fn ready(&self) -> Option<Vec<Arc<Pending>>> {
        self.items.iter().map(|item| item.pending.clone()).collect()
    }
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.items.clear();
        self.error = None;
        cx.notify();
    }
    pub fn pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking.is_some() {
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach files".into()),
        });
        self.picking = Some(cx.spawn_in(window, async move |this, cx| {
            let result = prompt.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.picking = None;
                match result {
                    Ok(Ok(Some(paths))) => this.add_paths(paths, window, cx),
                    Ok(Ok(None)) => {}
                    _ => {
                        this.error = Some(
                            "Could not open the file picker. You can also paste or drop files."
                                .into(),
                        );
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }
    pub fn add_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        for path in paths {
            self.add(Source::Path(path), window, cx);
        }
    }
    pub fn paste(
        &mut self,
        clipboard: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut handled = false;
        for entry in &clipboard.entries {
            match entry {
                ClipboardEntry::ExternalPaths(paths) => {
                    self.add_paths(paths.0.to_vec(), window, cx);
                    handled = true;
                }
                ClipboardEntry::Image(image) => {
                    let name = format!("pasted-image.{}", image.format.extension());
                    self.add(
                        Source::Image {
                            name,
                            bytes: Arc::new(image.bytes.clone()),
                        },
                        window,
                        cx,
                    );
                    handled = true;
                }
                _ => {}
            }
        }
        handled
    }
    fn add(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        if self.locked {
            return;
        }
        if let Source::Image { bytes, .. } = &source {
            let retained: u64 = self
                .items
                .iter()
                .filter_map(|item| match &item.source {
                    Source::Image { bytes, .. } => Some(bytes.len() as u64),
                    _ => None,
                })
                .sum();
            if bytes.len() as u64 > attachments::MAX_IMAGE_BYTES
                || retained + bytes.len() as u64 > attachments::MAX_TOTAL_IMAGE_BYTES
            {
                self.error =
                    Some("Images must be at most 10 MiB each and 80 MiB per message.".into());
                cx.notify();
                return;
            }
        }
        if self.items.len() >= attachments::MAX_COUNT {
            self.error = Some("You can attach up to 100 files per message.".into());
            cx.notify();
            return;
        }
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(Item {
            id,
            source,
            metadata: None,
            pending: None,
            preview: None,
            error: None,
            task: None,
        });
        self.retry(id, window, cx);
    }
    fn retry(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return;
        };
        item.error = None;
        item.metadata = None;
        let source = item.source.clone();
        let limiter = self.reading.clone();
        let reading = tokio_bridge::spawn(cx, async move {
            let permit = limiter
                .acquire_owned()
                .await
                .map_err(|error| error.to_string())?;
            tokio::task::spawn_blocking(move || source.read())
                .await
                .map_err(|error| error.to_string())?
                .map(|(metadata, bytes)| (metadata, bytes, permit))
                .map_err(|error| error.to_string())
        });
        item.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = reading.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(index) = this.items.iter().position(|item| item.id == id) else {
                    return;
                };
                this.items[index].task = None;
                match result {
                    Ok(Ok((metadata, bytes, permit))) => {
                        let validation = attachments::validate_batch(
                            this.items
                                .iter()
                                .filter_map(|item| item.metadata.as_ref())
                                .chain([&metadata]),
                        );
                        if let Err(error) = validation {
                            this.items[index].error = Some(error.to_string().into());
                        } else {
                            this.items[index].metadata = Some(metadata.clone());
                            this.upload(id, (metadata, bytes, permit), window, cx);
                        }
                    }
                    Ok(Err(error)) => this.items[index].error = Some(error.into()),
                    Err(_) => {
                        this.items[index].error =
                            Some("Could not read the attachment. Retry or remove it.".into())
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn upload(
        &mut self,
        id: u64,
        data: (Metadata, Vec<u8>, tokio::sync::OwnedSemaphorePermit),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (metadata, bytes, permit) = data;
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return;
        };
        let env = self.env.read(cx);
        let (Some(client), Some(rpc)) = (env.attachments.clone(), env.rpc.clone()) else {
            item.error = Some("Reconnect to upload this attachment, then retry.".into());
            return;
        };
        let uploading = tokio_bridge::spawn(cx, async move {
            let _permit = permit;
            let preview = if metadata.kind == "image" {
                let data = bytes.clone();
                tokio::task::spawn_blocking(move || crate::attachment_image::thumbnail(&data).ok())
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };
            client
                .upload(&rpc, metadata, bytes)
                .await
                .map(|pending| (pending, preview))
        });
        item.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = uploading.await;
            let _ = this.update_in(cx, |this, _, cx| {
                let Some(item) = this.items.iter_mut().find(|item| item.id == id) else {
                    return;
                };
                item.task = None;
                match result {
                    Ok(Ok((pending, preview))) => {
                        item.pending = Some(pending);
                        item.preview = preview;
                    }
                    Ok(Err(error)) => item.error = Some(error.to_string().into()),
                    Err(_) => {
                        item.error =
                            Some("Could not upload this attachment. Retry or remove it.".into())
                    }
                }
                cx.notify();
            });
        }));
    }
}
impl Render for AttachmentDraft {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .max_h(px(240.))
            .id("draft-attachments")
            .overflow_y_scroll()
            .when_some(self.error.clone(), |column, error| {
                column.child(div().text_xs().text_color(cx.theme().danger).child(error))
            })
            .children(self.items.iter().map(|item| {
                let id = item.id;
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .p_1()
                    .rounded_md()
                    .bg(cx.theme().secondary)
                    .when_some(item.preview.clone(), |row, image| {
                        let size = image.size(0);
                        let scale = (48. / size.width.0 as f32).min(48. / size.height.0 as f32);
                        row.child(
                            h_flex()
                                .w_12()
                                .h_12()
                                .flex_none()
                                .justify_center()
                                .items_center()
                                .child(
                                    img(image)
                                        .w(px(size.width.0 as f32 * scale))
                                        .h(px(size.height.0 as f32 * scale)),
                                ),
                        )
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .child(div().truncate().child(item.source.name()))
                            .child(
                                div()
                                    .text_color(if item.error.is_some() {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(item.error.clone().unwrap_or_else(|| {
                                        if item.pending.is_some() {
                                            "Ready"
                                        } else if item.metadata.is_some() {
                                            "Uploading…"
                                        } else {
                                            "Reading…"
                                        }
                                        .into()
                                    })),
                            ),
                    )
                    .when(item.error.is_some(), |row| {
                        row.child(
                            Button::new(("retry-attachment", id))
                                .label("Retry")
                                .disabled(self.locked)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.retry(id, window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(("remove-attachment", id))
                            .icon(Icon::new(Lucide::X))
                            .ghost()
                            .xsmall()
                            .accessibility_label("Remove attachment")
                            .disabled(self.locked)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.items.retain(|item| item.id != id);
                                cx.notify();
                            })),
                    )
            }))
    }
}
