//! The Add Project palette: existing folders, named projects, and registered
//! projects. All filesystem and project operations go to the environment.

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::input::Enter;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::projects::{self, Config, Folders};
use t3_client::shell::Project;

use crate::environment::LocalEnvironment;
use crate::tokio_bridge;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Home,
    Browse,
    New,
}

#[derive(Clone)]
enum Choice {
    Browse,
    New,
    Project(Project),
    Folder(String),
    Add,
    Create,
}

pub enum ProjectEvent {
    Closed,
    Opened(String, Option<String>),
}
impl EventEmitter<ProjectEvent> for ProjectPicker {}

pub struct ProjectPicker {
    env: Entity<LocalEnvironment>,
    command: Entity<CommandState>,
    focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    cwd: Option<String>,
    config: Option<Config>,
    mode: Mode,
    busy: bool,
    removal: Option<(Project, usize)>,
    restore_focus: bool,
    loading_config: bool,
    error: Option<SharedString>,
    folder_query: Option<String>,
    folders: Option<Folders>,
    generation: u64,
    browse_task: Option<Task<()>>,
    _env: Subscription,
}

impl ProjectPicker {
    pub fn new(
        env: Entity<LocalEnvironment>,
        cwd: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let return_focus = window.focused(cx);
        let command = cx.new(|cx| CommandState::new(window, cx));
        command.update(cx, |command, cx| command.focus(window, cx));
        let observe = cx.observe(&env, |this, env, cx| {
            if env.read(cx).rpc.is_some()
                && this.config.is_none()
                && !this.loading_config
                && this.error.is_none()
            {
                this.load_config(cx);
            }
            cx.notify();
        });
        let mut picker = Self {
            env,
            command,
            focus: cx.focus_handle(),
            return_focus,
            cwd,
            config: None,
            mode: Mode::Home,
            busy: false,
            removal: None,
            restore_focus: false,
            loading_config: false,
            error: None,
            folder_query: None,
            folders: None,
            generation: 0,
            browse_task: None,
            _env: observe,
        };
        picker.load_config(cx);
        picker
    }

    fn online(&self, cx: &App) -> bool {
        self.env.read(cx).rpc.is_some()
    }

    fn load_config(&mut self, cx: &mut Context<Self>) {
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            return;
        };
        self.loading_config = true;
        self.error = None;
        let loaded = tokio_bridge::spawn(cx, async move { projects::config(&rpc).await });
        cx.spawn(async move |this, cx| {
            let result = match loaded.await {
                Ok(result) => result.map_err(|error| error.detail()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update(cx, |this, cx| {
                this.loading_config = false;
                match result {
                    Ok(config) => this.config = Some(config),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.mode = mode;
        self.error = None;
        self.folder_query = None;
        self.folders = None;
        self.browse_task = None;
        self.generation += 1;
        let query = if mode == Mode::Browse {
            self.config
                .as_ref()
                .map(|config| config.initial_path.clone())
                .unwrap_or_else(|| "~/".into())
        } else {
            String::new()
        };
        self.command.update(cx, |command, cx| {
            command.set_loading(false, window, cx);
            command.set_query(query, window, cx);
            command.focus(window, cx);
        });
        if mode == Mode::Browse {
            self.query_changed(window, cx);
        }
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.removal.is_some() {
            self.cancel_remove(cx);
            return;
        }
        if let Some(focus) = &self.return_focus {
            focus.focus(window, cx);
        }
        cx.emit(ProjectEvent::Closed);
    }

    fn query_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode != Mode::Browse || self.busy {
            cx.notify();
            return;
        }
        let query = self.command.read(cx).query(cx);
        let (directory, _) = projects::browse_query(&query);
        if self.folder_query.as_ref() == Some(&directory) {
            cx.notify();
            return;
        }
        self.folder_query = Some(directory.clone());
        self.folders = None;
        self.error = None;
        self.generation += 1;
        let generation = self.generation;
        self.browse_task = None;
        self.command
            .update(cx, |command, cx| command.set_loading(false, window, cx));
        if directory.is_empty() || !projects::is_browse_path(&query) {
            cx.notify();
            return;
        }
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            cx.notify();
            return;
        };
        let cwd = self.cwd.clone();
        self.command
            .update(cx, |command, cx| command.set_loading(true, window, cx));
        self.browse_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let Ok(browsed) = cx.update(|_, cx| {
                tokio_bridge::spawn(cx, async move {
                    projects::browse(&rpc, &directory, cwd.as_deref()).await
                })
            }) else {
                return;
            };
            let result = match browsed.await {
                Ok(result) => result.map_err(|error| error.detail()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.command
                    .update(cx, |command, cx| command.set_loading(false, window, cx));
                match result {
                    Ok(folders) => this.folders = Some(folders),
                    Err(error) => this.error = Some(error.into()),
                }
                this.browse_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match choice {
            Choice::Browse => self.set_mode(Mode::Browse, window, cx),
            Choice::New => self.set_mode(Mode::New, window, cx),
            Choice::Project(project) => cx.emit(ProjectEvent::Opened(project.id, None)),
            Choice::Folder(path) => {
                self.command.update(cx, |command, cx| {
                    command.set_query(projects::directory(&path), window, cx)
                });
                self.query_changed(window, cx);
            }
            Choice::Add | Choice::Create => self.submit(window, cx),
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.mode == Mode::Home {
            return;
        }
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            self.error = Some("Environment unavailable".into());
            cx.notify();
            return;
        };
        let query = self.command.read(cx).query(cx);
        let config = match &self.config {
            Some(config) => config,
            None => return,
        };
        let creating = self.mode == Mode::New;
        let input = if creating {
            let name = query.trim();
            if name.is_empty() || name.encode_utf16().count() > 200 {
                self.error = Some("Enter a project name of at most 200 characters.".into());
                cx.notify();
                return;
            }
            name.to_owned()
        } else {
            if !projects::is_browse_path(&query) {
                self.error =
                    Some("Enter an absolute folder path or a path starting with ~/ or ./".into());
                cx.notify();
                return;
            }
            let canonical = self.folders.as_ref().map(|folders| {
                let (_, leaf) = projects::browse_query(&query);
                format!("{}{leaf}", projects::directory(&folders.parent_path))
            });
            match projects::resolve(
                canonical.as_deref().unwrap_or(&query),
                self.cwd.as_deref(),
                &config.platform,
            ) {
                Ok(path) => path,
                Err(error) => {
                    self.error = Some(error.into());
                    cx.notify();
                    return;
                }
            }
        };
        if !creating
            && let Some(project) = self
                .env
                .read(cx)
                .shell
                .projects
                .iter()
                .find(|project| projects::same_path(&project.workspace_root, &input))
        {
            cx.emit(ProjectEvent::Opened(project.id.clone(), None));
            return;
        }
        self.busy = true;
        self.error = None;
        self.browse_task = None;
        self.generation += 1;
        let result = tokio_bridge::spawn(cx, async move {
            if creating {
                projects::create(&rpc, &input)
                    .await
                    .map(|result| (result.project_id, result.commit_error))
            } else {
                projects::add(&rpc, &input)
                    .await
                    .map(|project| (project.id, None))
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match result.await {
                Ok(result) => result.map_err(|error| error.detail()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                this.command.update(cx, |command, cx| {
                    command.set_loading(false, window, cx);
                });
                this.restore_focus = true;
                match result {
                    Ok((id, warning)) => cx.emit(ProjectEvent::Opened(id, warning)),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn request_remove(&mut self, project: Project, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.online(cx) {
            return;
        }
        let env = self.env.read(cx);
        let count = env
            .shell
            .threads
            .values()
            .chain(env.shell.archived.values())
            .filter(|thread| thread.project_id == project.id)
            .count();
        self.removal = Some((project, count));
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn cancel_remove(&mut self, cx: &mut Context<Self>) {
        self.removal = None;
        self.restore_focus = true;
        cx.notify();
    }

    fn remove(&mut self, project: Project, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let removed =
            tokio_bridge::spawn(cx, async move { projects::remove(&rpc, &project.id).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = match removed.await {
                Ok(result) => result.map_err(|error| error.detail()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                if let Err(error) = result {
                    this.error = Some(error.into());
                }
                this.restore_focus = true;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for ProjectPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((project, count)) = self.removal.clone() {
            return v_flex()
                .id("remove-project-dialog")
                .track_focus(&self.focus)
                .role(Role::AlertDialog)
                .aria_label(format!("Remove {}?", project.title))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape"
                        || (event.keystroke.key == "enter" && this.focus.is_focused(window))
                    {
                        this.cancel_remove(cx);
                        cx.stop_propagation();
                    }
                }))
                .w_full()
                .max_w(px(560.))
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .shadow_lg()
                .p_4()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .child(format!("Remove {}?", project.title)),
                )
                .child(
                    div()
                        .text_sm()
                        .child(projects::removal_message(&project, count)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel-project-removal")
                                .label("Cancel")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_remove(cx))),
                        )
                        .child(
                            Button::new("confirm-project-removal")
                                .label("Remove project")
                                .danger()
                                .small()
                                .disabled(!self.online(cx))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.removal = None;
                                    this.remove(project.clone(), window, cx);
                                })),
                        ),
                )
                .focus_trap("project-picker", &self.focus)
                .into_any_element();
        }
        if self.restore_focus {
            self.restore_focus = false;
            let picker = cx.entity().downgrade();
            // Command chooses its focus target from its rendered search mode.
            // Wait until the search input has been mounted again.
            window.on_next_frame(move |window, cx| {
                let _ = picker.update(cx, |this, cx| {
                    if !this.busy && this.removal.is_none() {
                        this.command
                            .update(cx, |command, cx| command.focus(window, cx));
                    }
                });
            });
        }
        let offline = !self.online(cx);
        let disabled = self.busy || offline;
        let query = self.command.read(cx).query(cx);
        let mut choices = Vec::new();
        let mut items = Vec::new();
        match self.mode {
            Mode::Home => {
                choices.extend([Choice::Browse, Choice::New]);
                items.push(
                    CommandItem::new()
                        .label("Open folder")
                        .icon(Icon::new(Lucide::FolderOpen))
                        .disabled(disabled || self.config.is_none()),
                );
                items.push(
                    CommandItem::new()
                        .label("New project")
                        .icon(Icon::new(Lucide::FolderPlus))
                        .disabled(
                            disabled
                                || self
                                    .config
                                    .as_ref()
                                    .and_then(|config| config.new_projects_root.as_ref())
                                    .is_none(),
                        ),
                );
                let env = self.env.read(cx);
                for project in &env.shell.projects {
                    if self
                        .config
                        .as_ref()
                        .and_then(|config| config.scratch_workspace_root.as_deref())
                        .is_some_and(|scratch| {
                            projects::same_path(&project.workspace_root, scratch)
                        })
                    {
                        continue;
                    }
                    let project = project.clone();
                    choices.push(Choice::Project(project.clone()));
                    let picker = cx.entity().downgrade();
                    items.push(
                        CommandItem::new()
                            .label(project.title.clone())
                            .keywords([project.workspace_root.clone()])
                            .disabled(disabled)
                            .child(move |_, cx| {
                                let name = project.title.clone();
                                let path = project.workspace_root.clone();
                                let project = project.clone();
                                let picker = picker.clone();
                                h_flex()
                                    .gap_2()
                                    .w_full()
                                    .child(Icon::new(Lucide::Folder).small())
                                    .child(
                                        v_flex()
                                            .min_w_0()
                                            .flex_1()
                                            .child(div().text_sm().truncate().child(name))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .truncate()
                                                    .child(path),
                                            ),
                                    )
                                    .child(
                                        Button::new("remove-project")
                                            .accessibility_label("Remove project")
                                            .icon(Icon::new(Lucide::Trash))
                                            .tooltip("Remove project")
                                            .ghost()
                                            .xsmall()
                                            .disabled(disabled)
                                            .on_click(move |_, window, cx| {
                                                cx.stop_propagation();
                                                let _ = picker.update(cx, |this, cx| {
                                                    this.request_remove(project.clone(), window, cx)
                                                });
                                            }),
                                    )
                            }),
                    );
                }
            }
            Mode::Browse => {
                let (_, filter) = projects::browse_query(&query);
                if let Some(folders) = &self.folders {
                    for folder in projects::filter_folders(&folders.entries, &filter) {
                        choices.push(Choice::Folder(folder.full_path.clone()));
                        items.push(
                            CommandItem::new()
                                .label(folder.name.clone())
                                .icon(Icon::new(Lucide::Folder))
                                .disabled(disabled),
                        );
                    }
                }
                choices.push(Choice::Add);
                items.push(
                    CommandItem::new()
                        .label("Add this folder")
                        .icon(Icon::new(Lucide::FolderPlus))
                        .disabled(disabled || query.trim().is_empty()),
                );
            }
            Mode::New => {
                choices.push(Choice::Create);
                items.push(
                    CommandItem::new()
                        .label(if query.trim().is_empty() {
                            "Enter a project name".to_owned()
                        } else {
                            format!("Create {}", query.trim())
                        })
                        .icon(Icon::new(Lucide::FolderPlus))
                        .disabled(disabled || query.trim().is_empty()),
                );
            }
        }
        let this = cx.entity().downgrade();
        let query_entity = this.clone();
        let cancel_entity = this.clone();
        let command = Command::new(&self.command)
            .filterable(self.mode == Mode::Home)
            .searchable(!self.busy)
            .max_h((window.viewport_size().height * 0.45).min(px(320.)))
            .bordered(false)
            .items(items)
            .placeholder(match self.mode {
                Mode::Home => "Search projects…",
                Mode::Browse => "Enter a folder path…",
                Mode::New => "Project name…",
            })
            .on_query(move |_, window, cx| {
                let _ = query_entity.update(cx, |this, cx| this.query_changed(window, cx));
            })
            .on_confirm(move |index, window, cx| {
                if let Some(choice) = choices.get(index.row).cloned() {
                    let _ = this.update(cx, |this, cx| this.choose(choice, window, cx));
                }
            })
            .on_cancel(move |window, cx| {
                let _ = cancel_entity.update(cx, |this, cx| {
                    if this.mode == Mode::Home {
                        this.close(window, cx);
                    } else {
                        this.set_mode(Mode::Home, window, cx);
                    }
                });
            });
        let title = match self.mode {
            Mode::Home => "Add project",
            Mode::Browse => "Open folder",
            Mode::New => "New project",
        };
        let hint: SharedString = if offline {
            "Environment unavailable".into()
        } else if self.busy {
            "Saving project…".into()
        } else if self.loading_config {
            "Loading project folders…".into()
        } else if self.mode == Mode::New {
            self.config
                .as_ref()
                .and_then(|config| config.new_projects_root.as_ref())
                .map_or_else(
                    || "New projects are unavailable on this server".into(),
                    |root| format!("Creates a Git repository in {root}").into(),
                )
        } else {
            "".into()
        };
        let parent = self
            .folders
            .as_ref()
            .and_then(|folders| projects::parent(&folders.parent_path));
        v_flex()
            .id("project-dialog")
            .role(Role::Dialog)
            .aria_label(title)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w_full()
            .max_w(px(720.))
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if action.secondary && this.mode != Mode::Home {
                    this.submit(window, cx);
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .child(
                h_flex()
                    .p_3()
                    .gap_2()
                    .child(
                        Button::new("project-back")
                            .accessibility_label("Back")
                            .icon(Icon::new(Lucide::ArrowLeft))
                            .ghost()
                            .small()
                            .disabled(self.busy || self.mode == Mode::Home)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_mode(Mode::Home, window, cx)
                            })),
                    )
                    .child(div().flex_1().text_sm().font_medium().child(title))
                    .child(
                        Button::new("project-close")
                            .accessibility_label("Close Add project")
                            .icon(Icon::new(Lucide::X))
                            .ghost()
                            .small()
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
            .when(self.mode == Mode::Browse, |column| {
                column.child(
                    h_flex()
                        .px_3()
                        .gap_2()
                        .child(
                            Button::new("folder-up")
                                .label("Up")
                                .ghost()
                                .xsmall()
                                .disabled(disabled || parent.is_none())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(parent) = &parent {
                                        this.command.update(cx, |command, cx| {
                                            command.set_query(parent.clone(), window, cx)
                                        });
                                        this.query_changed(window, cx);
                                    }
                                })),
                        )
                        .child(
                            Button::new("folder-refresh")
                                .label("Refresh")
                                .ghost()
                                .xsmall()
                                .disabled(disabled)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.folder_query = None;
                                    this.query_changed(window, cx);
                                })),
                        ),
                )
            })
            .child(command)
            .when(!hint.is_empty(), |column| {
                column.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(hint),
                )
            })
            .when_some(self.error.clone(), |column, error| {
                column.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when(self.config.is_none() && !self.loading_config, |column| {
                column.child(
                    Button::new("retry-project-config")
                        .label("Retry")
                        .ghost()
                        .small()
                        .disabled(offline)
                        .on_click(cx.listener(|this, _, _, cx| this.load_config(cx))),
                )
            })
            .child(
                h_flex()
                    .p_3()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("project-cancel")
                            .label("Cancel")
                            .ghost()
                            .small()
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .when(self.mode != Mode::Home, |row| {
                        row.child(
                            Button::new("project-submit")
                                .label(if self.mode == Mode::New {
                                    "Create project"
                                } else {
                                    "Add project"
                                })
                                .primary()
                                .small()
                                .disabled(disabled || query.trim().is_empty())
                                .loading(self.busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                ),
                        )
                    }),
            )
            .focus_trap("project-picker", &self.focus)
            .into_any_element()
    }
}
