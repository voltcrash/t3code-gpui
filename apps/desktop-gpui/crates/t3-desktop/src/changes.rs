//! The native review surface. The environment supplies every patch and file list.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Selectable, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::checkpoint::{self, Turn};
use t3_client::review::{self, FileStat, Preview, Request, Scope};
use t3_host::preferences::DiffLayout;

use crate::environment::LocalEnvironment;
use crate::preferences;
use crate::thread_view::ThreadView;

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
type Installed = (Option<String>, Scope, Option<String>, String);

pub struct Closed;
pub struct Changes {
    env: Entity<LocalEnvironment>,
    thread: Entity<ThreadView>,
    thread_id: SharedString,
    checkpoints: Rc<[Turn]>,
    turns: Entity<Files>,
    turn_mode: bool,
    selected_run: Option<String>,
    checkpoint_files: Vec<DiffFile>,
    checkpoint_stats: Vec<FileStat>,
    _thread_subscription: Subscription,
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
        thread: Entity<ThreadView>,
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
        let turns = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<FileChoice>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let thread_subscription = cx.observe_in(&thread, window, |this, _, window, cx| {
            this.sync_turns(window, cx);
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &turns,
                window,
                |this, _, event: &SelectEvent<SearchableVec<FileChoice>>, window, cx| {
                    if let SelectEvent::Confirm(Some(run)) = event {
                        this.select_turn(Some(run.to_string()), window, cx);
                    }
                },
            ),
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
                        if this.turn_mode {
                            this.show_checkpoint_files(cx);
                        } else {
                            this.clear_diff(cx);
                            this.install_source(window, cx);
                        }
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
            thread_id: thread.read(cx).thread_id().clone(),
            checkpoints: Rc::from([]),
            turns,
            turn_mode: false,
            selected_run: None,
            checkpoint_files: Vec::new(),
            checkpoint_stats: Vec::new(),
            _thread_subscription: thread_subscription,
            thread,
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
        this.sync_turns(window, cx);
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
        thread: Entity<ThreadView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let thread_id = thread.read(cx).thread_id().clone();
        if self.thread_id == thread_id {
            return;
        }
        self.thread_id = thread_id;
        self._thread_subscription = cx.observe_in(&thread, window, |this, _, window, cx| {
            this.sync_turns(window, cx);
        });
        self.thread = thread;
        self.selected_run = None;
        self.checkpoints = Rc::from([]);
        self.refresh_key = None;
        self.state.set_workspace(None);
        self.clear_diff(cx);
        self.update_files(window, cx);
        self.sync(window, cx);
        self.sync_turns(window, cx);
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
        self.state.invalidate();
        self.task = None;
        self.installed = None;
        self.checkpoint_files.clear();
        self.checkpoint_stats.clear();
        self.error = None;
        self.notice = None;
        self.diff.update(cx, |diff, cx| diff.set_files([], cx));
    }

    fn selected_turn(&self) -> Option<&Turn> {
        self.checkpoints
            .iter()
            .find(|turn| Some(&turn.run_id) == self.selected_run.as_ref())
    }

    fn file_stats(&self) -> Option<&Vec<FileStat>> {
        if self.turn_mode {
            Some(&self.checkpoint_stats)
        } else {
            self.state.source().and_then(|source| source.files.as_ref())
        }
    }

    fn sync_turns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let checkpoints = self.thread.read(cx).checkpoints();
        if Rc::ptr_eq(&checkpoints, &self.checkpoints) {
            return;
        }
        let before = self.selected_turn().cloned();
        self.checkpoints = checkpoints;
        if self.selected_turn().is_none() {
            self.selected_run = self.checkpoints.first().map(|turn| turn.run_id.clone());
        }
        let choices: Vec<_> = self
            .checkpoints
            .iter()
            .enumerate()
            .map(|(index, turn)| FileChoice {
                path: turn.run_id.clone().into(),
                label: format!(
                    "Turn {}{} · {}",
                    turn.count,
                    if index == 0 { " (latest)" } else { "" },
                    turn.captured_at
                )
                .into(),
            })
            .collect();
        let selected: SharedString = self.selected_run.clone().unwrap_or_default().into();
        self.turns.update(cx, |turns, cx| {
            turns.set_items(SearchableVec::new(choices), window, cx);
            turns.set_selected_value(&selected, window, cx);
        });
        if self.turn_mode && before.as_ref() != self.selected_turn() {
            self.state.select_file(None);
            self.clear_diff(cx);
            self.update_files(window, cx);
            self.refresh_checkpoint(window, cx);
        }
        cx.notify();
    }

    fn select_turn(&mut self, run: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.turn_mode = true;
        self.selected_run = run;
        let selected: SharedString = self.selected_run.clone().unwrap_or_default().into();
        self.turns.update(cx, |turns, cx| {
            turns.set_selected_value(&selected, window, cx)
        });
        self.state.select_file(None);
        self.clear_diff(cx);
        self.update_files(window, cx);
        self.refresh_checkpoint(window, cx);
    }

    fn show_checkpoint_files(&mut self, cx: &mut Context<Self>) {
        let files = self
            .checkpoint_files
            .iter()
            .filter(|file| {
                self.state
                    .selected_file
                    .as_ref()
                    .is_none_or(|path| file.path().as_str() == path)
            })
            .cloned()
            .collect::<Vec<_>>();
        self.diff.update(cx, |diff, cx| diff.set_files(files, cx));
        cx.notify();
    }

    fn refresh_checkpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.task = None;
        let Some(turn) = self.selected_turn().cloned() else {
            self.state.invalidate();
            cx.notify();
            return;
        };
        let request = self.state.checkpoint_request(&self.thread_id, &turn);
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            self.error = Some("Changes are unavailable while disconnected.".into());
            cx.notify();
            return;
        };
        self.error = None;
        self.notice = None;
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = rpc.call::<_, checkpoint::Diff>(checkpoint::TURN_DIFF, &request.input).await;
            let result = cx.background_executor().spawn(async move {
                result.map(|diff| {
                    let hash = diff.hash();
                    (diff, hash)
                })
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.state.accepts(&request) {
                    return;
                }
                this.task = None;
                match result {
                    Ok((diff, _)) if !diff.matches(&request.input) => {
                        this.error = Some("The server returned a different checkpoint comparison. Refresh to try again.".into());
                    }
                    Ok((diff, _)) if diff.truncated() => {
                        this.notice = Some("This turn exceeds the server's checkpoint diff preview limit.".into());
                    }
                    Ok((diff, hash)) => {
                        this.install_patch(diff.diff, hash, request, window, cx);
                    }
                    Err(error) => this.error = Some(error.detail().into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.turn_mode {
            self.refresh_checkpoint(window, cx);
            return;
        }
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
        let stats = self.file_stats();
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
        let was_turn = self.turn_mode;
        self.turn_mode = false;
        if !self.state.select_scope(scope) && !was_turn {
            return;
        }
        self.state.select_file(None);
        self.clear_diff(cx);
        if was_turn {
            self.refresh(window, cx);
            return;
        }
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
        let key = (
            self.turn_mode.then(|| self.selected_run.clone()).flatten(),
            self.state.scope,
            self.state.selected_file.clone(),
            hash,
        );
        if self.installed.as_ref() == Some(&key) {
            return;
        }
        let parsed = cx
            .background_executor()
            .spawn(async move { DiffFile::parse(&patch) });
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = parsed.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.state.accepts(&request) {
                    return;
                }
                this.task = None;
                match result {
                    Ok(files) => {
                        if this.turn_mode {
                            this.checkpoint_stats = files
                                .iter()
                                .map(|file| FileStat {
                                    path: file.path().to_string(),
                                    previous_path: file
                                        .original_path()
                                        .filter(|path| *path != file.path())
                                        .map(ToString::to_string),
                                    additions: file.additions() as u64,
                                    deletions: file.deletions() as u64,
                                })
                                .collect();
                            this.checkpoint_files = files;
                            if this.state.selected_file.as_ref().is_some_and(|path| {
                                !this.checkpoint_stats.iter().any(|file| &file.path == path)
                            }) {
                                this.state.select_file(None);
                            }
                            this.update_files(window, cx);
                            this.show_checkpoint_files(cx);
                        } else {
                            this.diff.update(cx, |diff, cx| diff.set_files(files, cx));
                        }
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
        let source = (!self.turn_mode).then(|| self.state.source()).flatten();
        let title = if self.turn_mode {
            self.selected_turn()
                .map(|turn| format!("Turn {}", turn.count))
                .unwrap_or_else(|| "Latest turn".into())
        } else {
            source
                .map(|source| source.title.clone())
                .unwrap_or_else(|| "Changes".into())
        };
        let stats = self.file_stats();
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
        } else if self.turn_mode && self.selected_turn().is_none() {
            Some("No checkpoints are available for this thread yet.".into())
        } else if !self.turn_mode && self.state.cwd.is_none() {
            Some("The thread's project is unavailable.".into())
        } else if busy && self.installed.is_none() {
            Some("Loading changes…".into())
        } else if !self.turn_mode && self.state.preview.is_some() && source.is_none() {
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
                h_flex()
                    .flex_none()
                    .p_2()
                    .gap_1()
                    .children(
                        [Scope::Branch, Scope::Uncommitted]
                            .into_iter()
                            .map(|scope| {
                                Button::new(scope.label())
                                    .label(scope.label())
                                    .small()
                                    .ghost()
                                    .selected(!self.turn_mode && self.state.scope == scope)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.select_scope(scope, window, cx)
                                    }))
                            }),
                    )
                    .child(
                        Button::new("latest-turn")
                            .label("Latest turn")
                            .small()
                            .ghost()
                            .selected(
                                self.turn_mode
                                    && self.selected_run.as_ref()
                                        == self.checkpoints.first().map(|turn| &turn.run_id),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select_turn(
                                    this.checkpoints.first().map(|turn| turn.run_id.clone()),
                                    window,
                                    cx,
                                )
                            })),
                    ),
            )
            .when(self.turn_mode && !self.checkpoints.is_empty(), |column| {
                column.child(
                    div().flex_none().px_2().pb_2().child(
                        Select::new(&self.turns)
                            .small()
                            .search_placeholder("Find a turn")
                            .accessibility_label("Checkpoint turn"),
                    ),
                )
            })
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
