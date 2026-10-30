use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{Value, json};
use t3_client::workspaces::{self, Branch, Strategy};

use crate::environment::LocalEnvironment;
use crate::tokio_bridge;

pub struct Changed;
impl EventEmitter<Changed> for DraftWorkspace {}

pub struct DraftWorkspace {
    env: Entity<LocalEnvironment>,
    id: String,
    project: String,
    cwd: Option<String>,
    loaded: bool,
    changing_branch: bool,
    current: Option<String>,
    is_repo: bool,
    has_remote: bool,
    base: String,
    file_mode: Option<bool>,
    branches: Vec<Branch>,
    cursor: Option<u64>,
    selected: usize,
    scroll: ScrollHandle,
    query: Entity<InputState>,
    open: bool,
    request: u64,
    loading: Option<Task<()>>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl DraftWorkspace {
    pub fn new(
        env: Entity<LocalEnvironment>,
        id: String,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Find a branch"));
        let subscriptions = vec![
            cx.subscribe(&query, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => this.fetch(false, true, cx),
                InputEvent::PressEnter { .. } => {
                    if let Some(branch) = this.branches.get(this.selected).cloned() {
                        this.select_branch(branch, cx);
                    }
                }
                _ => {}
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active()
                    && this.cwd.is_some()
                    && this.loading.is_none()
                    && !this.changing_branch
                {
                    this.loaded = false;
                    this.fetch(false, false, cx);
                    cx.emit(Changed);
                }
            }),
            cx.observe(&env, |this, _, cx| {
                this.sync(cx);
                cx.emit(Changed);
                cx.notify();
            }),
        ];
        let mut this = Self {
            env,
            id,
            project,
            cwd: None,
            loaded: false,
            changing_branch: false,
            current: None,
            is_repo: false,
            has_remote: false,
            base: "HEAD".into(),
            file_mode: None,
            branches: Vec::new(),
            cursor: None,
            selected: 0,
            scroll: ScrollHandle::new(),
            query,
            open: false,
            request: 0,
            loading: None,
            error: None,
            _subscriptions: subscriptions,
        };
        this.sync(cx);
        this
    }

    pub fn set_project(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        self.project = project;
        self.query
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.sync(cx);
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let env = self.env.read(cx);
        let cwd = env
            .shell
            .project(&self.project)
            .map(|project| project.workspace_root.clone());
        if self.cwd != cwd {
            self.request += 1;
            self.loading = None;
            self.cwd = cwd;
            self.loaded = false;
            self.changing_branch = false;
            self.current = None;
            self.is_repo = false;
            self.base = "HEAD".into();
            self.file_mode = None;
            self.branches.clear();
            self.cursor = None;
            self.error = None;
        }
        if !self.loaded
            && self.loading.is_none()
            && self.error.is_none()
            && env.rpc.is_some()
            && self.cwd.is_some()
        {
            self.fetch(false, false, cx);
        }
    }

    pub fn can_choose(&self, cx: &App) -> bool {
        self.ready()
            && self.is_repo
            && self.env.read(cx).rpc.is_some()
            && !self.env.read(cx).new_thread_drafts.messages.busy(&self.id)
    }

    pub fn ready(&self) -> bool {
        self.loaded && !self.changing_branch
    }

    pub fn strategy(&self, cx: &App) -> Strategy {
        let env = self.env.read(cx);
        if self.loaded && !self.is_repo {
            return Strategy::default();
        }
        if let Some(strategy) = env.new_thread_drafts.workspace(&self.id) {
            let mut strategy = strategy.clone();
            if let Strategy::Root { branch } | Strategy::ExistingWorktree { branch, .. } =
                &mut strategy
                && self.current.is_some()
                && self.loaded
            {
                *branch = self.current.clone();
            }
            return strategy;
        }
        if !self.is_repo {
            return Strategy::default();
        }
        let worktree = env
            .server
            .settings
            .as_ref()
            .and_then(|settings| {
                let project = env.shell.project(&self.project)?;
                Some(workspaces::default_worktree(
                    settings,
                    project,
                    self.file_mode,
                    env.server.new_projects_root.as_deref(),
                    env.shell
                        .threads
                        .values()
                        .chain(env.shell.archived.values())
                        .chain(env.archives.threads.values())
                        .any(|thread| thread.project_id == self.project),
                ))
            })
            .unwrap_or(false);
        if worktree {
            Strategy::Worktree {
                base_ref: self.base.clone(),
                start_from_origin: false,
            }
        } else {
            Strategy::Root {
                branch: self.current.clone(),
            }
        }
    }

    fn set(&mut self, strategy: Strategy, cx: &mut Context<Self>) {
        let old_path = match self.env.read(cx).new_thread_drafts.workspace(&self.id) {
            Some(Strategy::ExistingWorktree { worktree_path, .. }) => Some(worktree_path.clone()),
            _ => self.cwd.clone(),
        };
        let next_path = match &strategy {
            Strategy::ExistingWorktree { worktree_path, .. } => Some(worktree_path.clone()),
            _ => self.cwd.clone(),
        };
        self.env.update(cx, |env, cx| {
            env.new_thread_drafts.set_workspace(&self.id, strategy);
            cx.notify();
        });
        self.open = false;
        if old_path != next_path {
            self.current = None;
            self.loaded = false;
            self.fetch(false, false, cx);
        }
        cx.emit(Changed);
        cx.notify();
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if !self.ready()
            || !self.is_repo
            || self.env.read(cx).new_thread_drafts.messages.busy(&self.id)
        {
            return;
        }
        let strategy = if matches!(self.strategy(cx), Strategy::Worktree { .. }) {
            Strategy::default()
        } else {
            Strategy::Worktree {
                base_ref: self.base.clone(),
                start_from_origin: false,
            }
        };
        self.set(strategy, cx);
    }

    pub fn open_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ready()
            || !self.is_repo
            || self.env.read(cx).new_thread_drafts.messages.busy(&self.id)
        {
            return;
        }
        self.open = !self.open;
        if self.open {
            self.query
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.fetch(false, false, cx);
            let query = self.query.clone();
            window
                .on_next_frame(move |window, cx| query.read(cx).focus_handle(cx).focus(window, cx));
        }
        cx.notify();
    }

    fn fetch(&mut self, more: bool, debounce: bool, cx: &mut Context<Self>) {
        let Some(project_cwd) = self.cwd.clone() else {
            return;
        };
        let cwd = match self.strategy(cx) {
            Strategy::ExistingWorktree { worktree_path, .. } => worktree_path,
            _ => project_cwd.clone(),
        };
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            return;
        };
        let query = self.query.read(cx).value().to_string();
        let cursor = more.then_some(self.cursor).flatten();
        let initial = !self.loaded;
        self.request += 1;
        let request = self.request;
        self.error = None;
        if !more {
            self.branches.clear();
            self.cursor = None;
            self.selected = 0;
        }
        let runtime = cx.global::<tokio_bridge::Tokio>().0.handle().clone();
        self.loading = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
            }
            let fetching = runtime.spawn(async move {
                let refs = workspaces::branches(&rpc, &cwd, &query, cursor).await?;
                let file = if initial {
                    rpc.call::<_, Value>(
                        "projects.readFile",
                        &json!({"cwd":project_cwd,"relativePath":"t3.json","allowMissing":true}),
                    )
                    .await
                    .ok()
                } else {
                    None
                };
                Ok::<_, t3_client::RpcError>((refs, file, query))
            });
            let result = fetching.await;
            let _ = this.update(cx, |this, cx| {
                if this.request != request {
                    return;
                }
                this.loading = None;
                match result {
                    Ok(Ok((refs, file, query))) => {
                        this.loaded = true;
                        this.is_repo = refs.is_repo;
                        this.has_remote = refs.has_primary_remote;
                        if query.is_empty() && !more {
                            this.base = workspaces::default_base(&refs.refs);
                            this.current = refs
                                .refs
                                .iter()
                                .find(|branch| branch.current)
                                .map(|branch| branch.name.clone());
                        }
                        if initial {
                            this.file_mode = file.as_ref().and_then(workspaces::project_file_mode);
                        }
                        for branch in refs.refs {
                            if !this.branches.iter().any(|old| old.name == branch.name) {
                                this.branches.push(branch);
                            }
                        }
                        this.cursor = refs.next_cursor;
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "loading draft workspace failed");
                        this.error = Some(
                            "Could not load branches. Check the project and try again.".into(),
                        );
                    }
                    Err(error) => {
                        tracing::warn!(%error, "loading draft workspace failed");
                        this.error = Some("Could not load branches. Try again.".into());
                    }
                }
                cx.emit(Changed);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn select_branch(&mut self, branch: Branch, cx: &mut Context<Self>) {
        if !self.ready() || self.env.read(cx).new_thread_drafts.messages.busy(&self.id) {
            return;
        }
        let Some(cwd) = self.cwd.clone() else { return };
        let selection = workspaces::select_branch(&cwd, &self.strategy(cx), &branch);
        let mut strategy = selection.strategy;
        let Some(checkout_cwd) = selection.checkout_cwd else {
            self.set(strategy, cx);
            return;
        };
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            return;
        };
        self.request += 1;
        let request = self.request;
        self.loading = None;
        self.changing_branch = true;
        self.open = false;
        self.error = None;
        let ref_name = branch.name.clone();
        let changing = tokio_bridge::spawn(cx, async move {
            rpc.call::<_, Value>(
                "vcs.switchRef",
                &json!({"cwd":checkout_cwd,"refName":ref_name}),
            )
            .await
        });
        cx.spawn(async move |this, cx| {
            let result = changing.await;
            let _ = this.update(cx, |this, cx| {
                if this.request != request {
                    return;
                }
                this.changing_branch = false;
                match result {
                    Ok(Ok(result)) => {
                        let name = result["refName"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| {
                                if branch.is_remote {
                                    branch
                                        .name
                                        .split_once('/')
                                        .map_or(branch.name.clone(), |(_, name)| name.into())
                                } else {
                                    branch.name.clone()
                                }
                            });
                        this.current = Some(name.clone());
                        match &mut strategy {
                            Strategy::Root { branch }
                            | Strategy::ExistingWorktree { branch, .. } => *branch = Some(name),
                            Strategy::Worktree { .. } => {}
                        }
                        this.set(strategy, cx);
                    }
                    error => {
                        tracing::warn!(?error, "switching draft branch failed");
                        this.error = Some(
                            "Could not switch branch. Check the working tree and try again.".into(),
                        );
                    }
                }
                cx.emit(Changed);
                cx.notify();
            });
        })
        .detach();
        cx.emit(Changed);
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.branches.is_empty() {
            self.selected =
                (self.selected as isize + delta).rem_euclid(self.branches.len() as isize) as usize;
            self.scroll.scroll_to_item(self.selected);
            cx.notify();
        }
    }

    fn picker(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .key_context("DraftBranches")
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.step(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.step(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.open = false;
                cx.notify();
                cx.stop_propagation();
            }))
            .w(px(340.))
            .gap_2()
            .child(Input::new(&self.query).small())
            .child(
                v_flex()
                    .id("workspace-branches")
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(self.branches.iter().enumerate().map(|(index, branch)| {
                        let branch = branch.clone();
                        h_flex()
                            .id(SharedString::from(branch.name.clone()))
                            .w_full()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .when(index == self.selected, |row| row.bg(cx.theme().accent))
                            .hover(|row| row.bg(cx.theme().accent))
                            .text_sm()
                            .child(div().min_w_0().truncate().child(branch.name.clone()))
                            .when(branch.current, |row| {
                                row.child(
                                    div()
                                        .ml_auto()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Current"),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_branch(branch.clone(), cx)
                            }))
                    })),
            )
            .when(self.loading.is_some(), |view| {
                view.child(div().text_xs().child("Loading branches…"))
            })
            .when(
                self.loading.is_none() && self.branches.is_empty() && self.error.is_none(),
                |view| view.child(div().text_xs().child("No branches found")),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(div().text_xs().text_color(cx.theme().danger).child(error))
            })
            .when(self.cursor.is_some() || self.error.is_some(), |view| {
                view.child(
                    Button::new("more-branches")
                        .label(if self.error.is_some() {
                            "Retry"
                        } else {
                            "Load more"
                        })
                        .ghost()
                        .small()
                        .disabled(self.loading.is_some())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.fetch(this.error.is_none(), false, cx)
                        })),
                )
            })
            .into_any_element()
    }
}

impl Render for DraftWorkspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let env = self.env.read(cx);
        let strategy = self.strategy(cx);
        let disabled =
            env.rpc.is_none() || !self.ready() || env.new_thread_drafts.messages.busy(&self.id);
        let mode_disabled = env.rpc.is_none()
            || self.changing_branch
            || env.new_thread_drafts.messages.busy(&self.id);
        let local_selected = matches!(strategy, Strategy::Root { .. });
        let worktree_selected = matches!(strategy, Strategy::Worktree { .. });
        let can_create_worktree = self.loaded && self.is_repo;
        let this = cx.weak_entity();
        let menu_owner = this.clone();
        let content_owner = this.clone();
        let base = self.base.clone();
        let is_repo = self.is_repo;
        let label = match &strategy {
            Strategy::Root { branch } | Strategy::ExistingWorktree { branch, .. } => branch
                .clone()
                .or_else(|| self.current.clone())
                .unwrap_or_else(|| "Current branch".into()),
            Strategy::Worktree { base_ref, .. } => base_ref.clone(),
        };
        h_flex()
            .gap_1()
            .flex_wrap()
            .child(
                Button::new("draft-workspace")
                    .label(strategy.label())
                    .ghost()
                    .small()
                    .disabled(mode_disabled)
                    .dropdown_caret(true)
                    .accessibility_label("Workspace")
                    .dropdown_menu(move |menu, _, _| {
                        let local = menu_owner.clone();
                        let worktree = menu_owner.clone();
                        let base = base.clone();
                        menu.item(
                            PopupMenuItem::new("Local")
                                .checked(local_selected)
                                .on_click(move |_, _, cx| {
                                    let _ = local
                                        .update(cx, |this, cx| this.set(Strategy::default(), cx));
                                }),
                        )
                        .item(
                            PopupMenuItem::new("New worktree")
                                .checked(worktree_selected)
                                .disabled(!can_create_worktree)
                                .on_click(move |_, _, cx| {
                                    let _ = worktree.update(cx, |this, cx| {
                                        this.set(
                                            Strategy::Worktree {
                                                base_ref: base.clone(),
                                                start_from_origin: false,
                                            },
                                            cx,
                                        )
                                    });
                                }),
                        )
                    }),
            )
            .when(is_repo, |row| {
                row.child(
                    Popover::new("draft-branches")
                        .open(self.open)
                        .anchor(Anchor::TopLeft)
                        .on_open_change(move |open, window, cx| {
                            let _ = this.update(cx, |this, cx| {
                                if this.open != *open {
                                    this.open_branches(window, cx);
                                }
                            });
                        })
                        .trigger(
                            Button::new("draft-branch")
                                .label(label)
                                .ghost()
                                .small()
                                .disabled(disabled)
                                .dropdown_caret(true)
                                .accessibility_label("Branch"),
                        )
                        .content(move |_, _, cx| {
                            content_owner
                                .update(cx, |this, cx| this.picker(cx))
                                .unwrap_or_else(|_| div().into_any_element())
                        }),
                )
            })
            .when(
                matches!(strategy, Strategy::Worktree { .. }) && self.has_remote,
                |row| {
                    row.child(
                        Switch::new("worktree-origin")
                            .label("Start from origin")
                            .checked(matches!(
                                strategy,
                                Strategy::Worktree {
                                    start_from_origin: true,
                                    ..
                                }
                            ))
                            .disabled(disabled)
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                if let Strategy::Worktree { base_ref, .. } = this.strategy(cx) {
                                    this.set(
                                        Strategy::Worktree {
                                            base_ref,
                                            start_from_origin: *checked,
                                        },
                                        cx,
                                    );
                                }
                            })),
                    )
                },
            )
            .when(self.changing_branch, |row| {
                row.child(div().text_xs().child("Switching branch…"))
            })
            .when(self.error.is_some() && self.loaded && !self.open, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(self.error.clone().unwrap()),
                )
            })
            .when(!self.loaded, |row| {
                row.child(div().text_xs().child(if self.error.is_some() {
                    "Could not load workspace"
                } else {
                    "Loading workspace…"
                }))
            })
            .when(self.error.is_some() && !self.loaded, |row| {
                row.child(
                    Button::new("retry-workspace")
                        .label("Retry")
                        .ghost()
                        .small()
                        .disabled(self.loading.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.fetch(false, false, cx))),
                )
            })
    }
}
