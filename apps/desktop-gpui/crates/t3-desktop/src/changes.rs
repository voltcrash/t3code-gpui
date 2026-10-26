//! The native review surface. The environment supplies every patch and file list.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Selectable, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::review::{self, Preview, Request, Scope};
use t3_host::preferences::DiffLayout;

use crate::environment::LocalEnvironment;
use crate::preferences;

#[derive(Clone)]
struct FileChoice {
    path: SharedString,
    label: SharedString,
}
impl SelectItem for FileChoice {
    type Value = SharedString;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &SharedString {
        &self.path
    }
}
type Files = SelectState<SearchableVec<FileChoice>>;
type Installed = (Scope, Option<String>, String);

pub struct Closed;
pub struct Changes {
    env: Entity<LocalEnvironment>,
    thread_id: SharedString,
    state: review::State,
    diff: Entity<DiffState>,
    files: Entity<Files>,
    focus: FocusHandle,
    installed: Option<Installed>,
    task: Option<Task<()>>,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    online: bool,
    refresh_key: Option<(String, Option<String>, Option<String>)>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<Closed> for Changes {}

impl Changes {
    pub fn new(
        env: Entity<LocalEnvironment>,
        thread_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preferences = preferences::global(cx);
        let value = preferences.read(cx).value.clone();
        let diff = cx.new(|cx| {
            let mut state = DiffState::new([], cx);
            state.set_mode(diff_mode(value.diff_layout), cx);
            state
        });
        let files = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<FileChoice>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let subscriptions = vec![
            cx.observe_in(&env, window, |this, _, window, cx| this.sync(window, cx)),
            cx.observe_in(&preferences, window, |this, model, window, cx| {
                let value = model.read(cx).value.clone();
                this.diff.update(cx, |diff, cx| {
                    diff.set_mode(diff_mode(value.diff_layout), cx)
                });
                if this.state.ignore_whitespace != value.diff_ignore_whitespace {
                    this.state.ignore_whitespace = value.diff_ignore_whitespace;
                    this.clear_diff(cx);
                    this.refresh(window, cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &files,
                window,
                |this, _, event: &SelectEvent<SearchableVec<FileChoice>>, window, cx| {
                    if let SelectEvent::Confirm(Some(path)) = event {
                        this.state
                            .select_file((!path.is_empty()).then(|| path.to_string()));
                        this.clear_diff(cx);
                        this.install_source(window, cx);
                    }
                },
            ),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh(window, cx);
                }
            }),
        ];
        let mut this = Self {
            env,
            thread_id,
            state: review::State::new(value.diff_ignore_whitespace),
            diff,
            files,
            focus: cx.focus_handle(),
            installed: None,
            task: None,
            error: None,
            notice: None,
            online: false,
            refresh_key: None,
            _subscriptions: subscriptions,
        };
        this.sync(window, cx);
        this.focus(window, cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let this = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |this, cx| this.focus.focus(window, cx));
        });
    }

    pub fn set_thread(
        &mut self,
        thread_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.thread_id == thread_id {
            return;
        }
        self.thread_id = thread_id;
        self.refresh_key = None;
        self.state.set_workspace(None);
        self.clear_diff(cx);
        self.sync(window, cx);
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env.read(cx);
        let thread = env.thread(&self.thread_id);
        let cwd = thread
            .and_then(|thread| review::workspace(&env.shell, thread))
            .map(str::to_owned);
        let key = cwd.clone().map(|cwd| {
            (
                cwd,
                thread.and_then(|thread| thread.latest_run_completed_at().map(str::to_owned)),
                thread.and_then(|thread| thread.active_run_id.clone()),
            )
        });
        let online = env.rpc.is_some();
        let reconnected = online && !self.online;
        self.online = online;
        let workspace_changed = self.state.set_workspace(cwd);
        if workspace_changed {
            self.clear_diff(cx);
        }
        if key != self.refresh_key || reconnected {
            self.refresh_key = key;
            self.refresh(window, cx);
        }
        cx.notify();
    }

    fn clear_diff(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        self.installed = None;
        self.error = None;
        self.notice = None;
        self.diff.update(cx, |diff, cx| diff.set_files([], cx));
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.task = None;
        let Some(request) = self.state.preview_request() else {
            cx.notify();
            return;
        };
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            self.error = Some("Changes are unavailable while disconnected.".into());
            cx.notify();
            return;
        };
        self.error = None;
        self.notice = None;
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = rpc
                .call::<_, Preview>(review::PREVIEW, &request.input)
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.state.accepts(&request) {
                    return;
                }
                this.task = None;
                match result {
                    Ok(preview) => {
                        let selected = this.state.selected_file.clone();
                        this.state.apply(&request, preview);
                        if selected != this.state.selected_file {
                            this.clear_diff(cx);
                        }
                        this.update_files(window, cx);
                        this.install_source(window, cx);
                    }
                    Err(error) => this.error = Some(error.detail().into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn update_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stats = self.state.source().and_then(|source| source.files.as_ref());
        let mut choices = vec![FileChoice {
            path: "".into(),
            label: format!("All files ({})", stats.map_or(0, Vec::len)).into(),
        }];
        if let Some(stats) = stats {
            choices.extend(stats.iter().map(|file| FileChoice {
                path: file.path.clone().into(),
                label: format!("{}  +{} −{}", file.path, file.additions, file.deletions).into(),
            }));
        }
        let selected: SharedString = self.state.selected_file.clone().unwrap_or_default().into();
        self.files.update(cx, |files, cx| {
            files.set_items(SearchableVec::new(choices), window, cx);
            files.set_selected_value(&selected, window, cx);
        });
    }

    fn select_scope(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.select_scope(scope) {
            return;
        }
        self.clear_diff(cx);
        if self.state.preview.is_some() {
            self.update_files(window, cx);
            self.install_source(window, cx);
        } else {
            self.refresh(window, cx);
        }
        cx.notify();
    }

    fn install_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.notice = None;
        if self.state.selected_file.is_some() {
            let Some(request) = self.state.file_request() else {
                self.refresh(window, cx);
                return;
            };
            let Some(rpc) = self.env.read(cx).rpc.clone() else {
                self.error = Some("Changes are unavailable while disconnected.".into());
                cx.notify();
                return;
            };
            self.task = Some(cx.spawn_in(window, async move |this, cx| {
                let result = rpc.call::<_, Preview>(review::PREVIEW, &request.input).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if !this.state.accepts(&request) {
                        return;
                    }
                    this.task = None;
                    match result {
                        Ok(preview) => {
                            let source = preview.sources.into_iter()
                                .find(|source| source.kind == this.state.scope.kind());
                            if let Some(source) = source {
                                if source.truncated {
                                    this.notice = Some("This file exceeds the server's diff preview limit. Choose another file.".into());
                                } else {
                                    this.install_patch(source.diff, source.diff_hash, request, window, cx);
                                }
                            } else {
                                this.error = Some("This comparison is no longer available. Refresh changes.".into());
                            }
                        }
                        Err(error) => this.error = Some(error.detail().into()),
                    }
                    cx.notify();
                });
            }));
        } else if let Some(source) = self.state.source() {
            if source.truncated {
                self.notice =
                    Some("This comparison is large. Choose a file to load its changes.".into());
            } else {
                let patch = source.diff.clone();
                let hash = source.diff_hash.clone();
                // The same comparison revision also guards background parsing.
                if let Some(request) = self.state.preview_request() {
                    self.install_patch(patch, hash, request, window, cx);
                }
            }
        } else {
            self.clear_diff(cx);
        }
        cx.notify();
    }

    fn install_patch(
        &mut self,
        patch: String,
        hash: String,
        request: Request,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = (self.state.scope, self.state.selected_file.clone(), hash);
        if self.installed.as_ref() == Some(&key) {
            return;
        }
        let parsed = cx
            .background_executor()
            .spawn(async move { DiffFile::parse(&patch) });
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = parsed.await;
            let _ = this.update_in(cx, |this, _, cx| {
                if !this.state.accepts(&request) {
                    return;
                }
                this.task = None;
                match result {
                    Ok(files) => {
                        this.diff.update(cx, |diff, cx| diff.set_files(files, cx));
                        this.installed = Some(key);
                    }
                    Err(error) => {
                        tracing::warn!("could not parse review patch: {error}");
                        this.error =
                            Some("Could not display these changes. Refresh to try again.".into());
                    }
                }
                cx.notify();
            });
        }));
    }
}

fn diff_mode(layout: DiffLayout) -> DiffMode {
    match layout {
        DiffLayout::Stacked => DiffMode::Unified,
        DiffLayout::Split => DiffMode::Split,
    }
}

impl Render for Changes {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let preferences = preferences::global(cx);
        let value = preferences.read(cx).value.clone();
        let preferences_busy = preferences.read(cx).saving || !preferences.read(cx).loaded;
        let busy = self.task.is_some();
        let source = self.state.source();
        let title = source
            .map(|source| source.title.clone())
            .unwrap_or_else(|| "Changes".into());
        let stats = source.and_then(|source| source.files.as_ref());
        let totals = stats.map(|files| {
            files
                .iter()
                .fold((0u64, 0u64), |(additions, deletions), file| {
                    (additions + file.additions, deletions + file.deletions)
                })
        });
        let mut body = Diff::new(&self.diff)
            .soft_wrap(value.word_wrap)
            .size_full()
            .into_any_element();
        let message = if let Some(error) = &self.error {
            Some(error.clone())
        } else if let Some(notice) = &self.notice {
            Some(notice.clone())
        } else if self.state.cwd.is_none() {
            Some("The thread's project is unavailable.".into())
        } else if busy && self.installed.is_none() {
            Some("Loading changes…".into())
        } else if self.state.preview.is_some() && source.is_none() {
            Some("Changes are available for Git projects.".into())
        } else if self.installed.is_some() && self.diff.read(cx).files().is_empty() {
            Some("No changes in this comparison.".into())
        } else {
            None
        };
        if let Some(message) = message {
            body = v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message),
                )
                .when(self.error.is_some(), |column| {
                    column.child(
                        Button::new("retry-changes")
                            .label("Retry")
                            .small()
                            .disabled(busy || !self.online)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    )
                })
                .into_any_element();
        }
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(Closed);
                    cx.stop_propagation();
                }
            }))
            .child(
                h_flex()
                    .h_10()
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_medium()
                            .child(title),
                    )
                    .when_some(totals, |row, (additions, deletions)| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().success)
                                .child(format!("+{additions}")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(format!("−{deletions}")),
                        )
                    })
                    .child(
                        Button::new("refresh-changes")
                            .icon(Icon::new(Lucide::RefreshCw))
                            .ghost()
                            .small()
                            .accessibility_label("Refresh changes")
                            .tooltip("Refresh changes")
                            .disabled(busy || !self.online)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    )
                    .child(
                        Button::new("close-changes")
                            .icon(Icon::new(Lucide::X))
                            .ghost()
                            .small()
                            .accessibility_label("Close changes")
                            .tooltip("Close changes (Escape)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                    ),
            )
            .child(
                h_flex().flex_none().p_2().gap_1().children(
                    [Scope::Branch, Scope::Uncommitted]
                        .into_iter()
                        .map(|scope| {
                            Button::new(scope.label())
                                .label(scope.label())
                                .small()
                                .ghost()
                                .selected(self.state.scope == scope)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_scope(scope, window, cx)
                                }))
                        }),
                ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .px_2()
                    .pb_2()
                    .gap_1()
                    .child(
                        div().flex_1().min_w_0().child(
                            Select::new(&self.files)
                                .small()
                                .search_placeholder("Find a changed file")
                                .accessibility_label("Changed files")
                                .disabled(busy || stats.is_none_or(Vec::is_empty)),
                        ),
                    )
                    .child(
                        Button::new("diff-layout")
                            .label(if value.diff_layout == DiffLayout::Stacked {
                                "Unified"
                            } else {
                                "Split"
                            })
                            .small()
                            .ghost()
                            .disabled(preferences_busy)
                            .on_click({
                                let preferences = preferences.clone();
                                cx.listener(move |_, _, window, cx| {
                                    preferences.update(cx, |model, cx| {
                                        let mut value = model.value.clone();
                                        value.diff_layout =
                                            if value.diff_layout == DiffLayout::Stacked {
                                                DiffLayout::Split
                                            } else {
                                                DiffLayout::Stacked
                                            };
                                        model.change(value, window, cx);
                                    })
                                })
                            }),
                    )
                    .child(
                        Button::new("diff-wrap")
                            .label("Wrap")
                            .small()
                            .ghost()
                            .selected(value.word_wrap)
                            .accessibility_label("Wrap diff lines")
                            .tooltip("Wrap lines")
                            .disabled(preferences_busy)
                            .on_click({
                                let preferences = preferences.clone();
                                cx.listener(move |_, _, window, cx| {
                                    preferences.update(cx, |model, cx| {
                                        let mut value = model.value.clone();
                                        value.word_wrap = !value.word_wrap;
                                        model.change(value, window, cx);
                                    })
                                })
                            }),
                    )
                    .child(
                        Button::new("diff-whitespace")
                            .label("±")
                            .small()
                            .ghost()
                            .selected(!value.diff_ignore_whitespace)
                            .tooltip("Show whitespace changes")
                            .accessibility_label("Show whitespace changes")
                            .disabled(preferences_busy)
                            .on_click({
                                let preferences = preferences.clone();
                                cx.listener(move |_, _, window, cx| {
                                    preferences.update(cx, |model, cx| {
                                        let mut value = model.value.clone();
                                        value.diff_ignore_whitespace =
                                            !value.diff_ignore_whitespace;
                                        model.change(value, window, cx);
                                    })
                                })
                            }),
                    ),
            )
            .when(busy && self.installed.is_some(), |column| {
                column.child(
                    div()
                        .px_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Refreshing…"),
                )
            })
            .when_some(preferences.read(cx).error.clone(), |column, error| {
                column.child(
                    div()
                        .px_3()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(div().flex_1().min_h_0().min_w_0().child(body))
    }
}
