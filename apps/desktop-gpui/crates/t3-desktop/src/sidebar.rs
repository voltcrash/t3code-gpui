//! Thread list, ported from `apps/web/src/components/Sidebar.tsx`: pinned and
//! active threads, then the Snoozed and Settled shelves, collapsed by default.

use std::ops::Range;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::thread_actions::{self, ThreadActions};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::StyledExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::shell::{ThreadShell, ThreadStatus, sidebar_sections, timestamp_ms};

use crate::environment::{ConnectionState, LocalEnvironment};
use crate::format::relative_time;

/// `SETTLED_TAIL_INITIAL_COUNT` and `SETTLED_TAIL_PAGE_COUNT` in the web sidebar.
const SETTLED_INITIAL: usize = 10;
const SETTLED_PAGE: usize = 25;
const ROW_HEIGHT: Pixels = px(36.);

pub enum SidebarEvent {
    SelectionChanged,
    NewThread,
    AddProject,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shelf {
    Snoozed,
    Settled,
    Archived,
}

#[derive(Clone, Copy)]
enum Indicator {
    Working,
    NeedsYou,
    Failed,
    Unread,
}

#[derive(Clone)]
struct ThreadRow {
    id: SharedString,
    title: SharedString,
    indicator: Option<Indicator>,
    time: SharedString,
    pinned: bool,
}

#[derive(Clone)]
enum Row {
    Thread(ThreadRow),
    Shelf {
        shelf: Shelf,
        count: usize,
        expanded: bool,
    },
    ShowMore,
    ArchiveNotice(SharedString),
}

pub struct Sidebar {
    env: Entity<LocalEnvironment>,
    selected: Option<SharedString>,
    snoozed_expanded: bool,
    settled_expanded: bool,
    archived_expanded: bool,
    actions: Entity<ThreadActions>,
    _actions: Subscription,
    settled_visible: usize,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
    _observe: Subscription,
    _clock: Task<()>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    pub fn new(
        env: Entity<LocalEnvironment>,
        actions: Entity<ThreadActions>,
        cx: &mut Context<Self>,
    ) -> Self {
        let actions_observe = cx.observe(&actions, |_, _, cx| cx.notify());
        let observe = cx.observe(&env, |_, _, cx| cx.notify());
        // Ages and snooze wake-ups are minute-grained, like the web sidebar.
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(60))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            env,
            selected: None,
            snoozed_expanded: false,
            settled_expanded: false,
            archived_expanded: false,
            actions,
            _actions: actions_observe,
            settled_visible: SETTLED_INITIAL,
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            _observe: observe,
            _clock: clock,
        }
    }

    pub fn selected(&self) -> Option<&SharedString> {
        self.selected.as_ref()
    }

    pub fn select(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.selected = Some(id);
        cx.emit(SidebarEvent::SelectionChanged);
        cx.notify();
    }

    /// Drops the selection without announcing it, for a draft taking the pane.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        cx.notify();
    }

    fn toggle(&mut self, shelf: Shelf, cx: &mut Context<Self>) {
        match shelf {
            Shelf::Snoozed => self.snoozed_expanded = !self.snoozed_expanded,
            Shelf::Settled => self.settled_expanded = !self.settled_expanded,
            Shelf::Archived => {
                self.archived_expanded = !self.archived_expanded;
                if self.archived_expanded {
                    self.env.update(cx, |env, cx| env.load_archives(cx));
                }
            }
        }
        cx.notify();
    }

    fn rebuild_rows(&mut self, cx: &App) {
        let now = now_ms();
        let env = self.env.read(cx);
        let sections = sidebar_sections(&env.shell, env.capabilities, now);
        let selected = self.selected.as_deref();
        let row =
            |thread: &ThreadShell, settled: bool| Row::Thread(thread_row(thread, settled, now));
        // A collapsed shelf still shows the open thread so it never vanishes.
        let shown =
            |threads: &[&ThreadShell], expanded: bool, limit: usize, settled: bool| -> Vec<Row> {
                if expanded {
                    let mut rows: Vec<Row> = threads
                        .iter()
                        .take(limit)
                        .map(|thread| row(thread, settled))
                        .collect();
                    if let Some(selected) = threads
                        .iter()
                        .skip(limit)
                        .find(|thread| Some(thread.id.as_str()) == selected)
                    {
                        rows.push(row(selected, settled));
                    }
                    rows
                } else {
                    threads
                        .iter()
                        .filter(|thread| Some(thread.id.as_str()) == selected)
                        .map(|thread| row(thread, settled))
                        .collect()
                }
            };

        let mut rows: Vec<Row> = sections
            .pinned
            .iter()
            .chain(&sections.active)
            .map(|thread| row(thread, false))
            .collect();
        if !sections.snoozed.is_empty() {
            rows.push(Row::Shelf {
                shelf: Shelf::Snoozed,
                count: sections.snoozed.len(),
                expanded: self.snoozed_expanded,
            });
            rows.extend(shown(
                &sections.snoozed,
                self.snoozed_expanded,
                usize::MAX,
                false,
            ));
        }
        if !sections.settled.is_empty() {
            rows.push(Row::Shelf {
                shelf: Shelf::Settled,
                count: sections.settled.len(),
                expanded: self.settled_expanded,
            });
            rows.extend(shown(
                &sections.settled,
                self.settled_expanded,
                self.settled_visible,
                true,
            ));
            if self.settled_expanded && sections.settled.len() > self.settled_visible {
                rows.push(Row::ShowMore);
            }
        }
        let archived_map = if env.archives.sequence.is_some() {
            &env.archives.threads
        } else {
            &env.shell.archived
        };
        let mut archived: Vec<_> = archived_map
            .values()
            .filter(|thread| !thread.is_subagent())
            .collect();
        archived.sort_by_key(|thread| {
            (
                std::cmp::Reverse(timestamp_ms(thread.archived_at.as_deref()).unwrap_or(0)),
                &thread.id,
            )
        });
        rows.push(Row::Shelf {
            shelf: Shelf::Archived,
            count: archived.len(),
            expanded: self.archived_expanded,
        });
        rows.extend(shown(&archived, self.archived_expanded, usize::MAX, false));
        if self.archived_expanded {
            if let Some(error) = &env.archive_error {
                rows.push(Row::ArchiveNotice(error.clone()));
            } else if env.archives.sequence.is_none() {
                rows.push(Row::ArchiveNotice("Loading archive…".into()));
            } else if archived.is_empty() {
                rows.push(Row::ArchiveNotice("No archived threads".into()));
            }
        }
        self.rows = rows;
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let base = h_flex()
            .id(ix)
            .w_full()
            .h(ROW_HEIGHT)
            .px_2p5()
            .gap_2p5()
            .rounded_md()
            .text_sm()
            .cursor_pointer();
        match &self.rows[ix] {
            Row::Thread(thread) => {
                let selected = self.selected.as_ref() == Some(&thread.id);
                let id = thread.id.clone();
                let actions = self.actions.clone();
                let menu_id = thread.id.clone();
                base.id(thread.id.clone())
                    .when(selected, |row| {
                        row.bg(theme.sidebar_accent)
                            .text_color(theme.sidebar_accent_foreground)
                    })
                    .when(!selected, |row| {
                        row.hover(|row| row.bg(theme.sidebar_accent.opacity(0.5)))
                    })
                    .child(indicator(thread.indicator, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(thread.title.clone()),
                    )
                    .when(thread.pinned, |row| {
                        row.child(
                            Icon::new(Lucide::Pin)
                                .xsmall()
                                .text_color(theme.muted_foreground.opacity(0.65)),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(thread.time.clone()),
                    )
                    .child(thread_actions::button(&self.actions, &thread.id))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
                    .context_menu(move |menu, _, cx| {
                        thread_actions::menu(menu, &actions, &menu_id, cx)
                    })
                    .into_any_element()
            }
            Row::Shelf {
                shelf,
                count,
                expanded,
            } => {
                let shelf = *shelf;
                let label = match shelf {
                    Shelf::Snoozed => "Snoozed",
                    Shelf::Settled => "Settled",
                    Shelf::Archived => "Archived",
                };
                base.text_xs()
                    .text_color(theme.muted_foreground)
                    .hover(|row| row.text_color(theme.sidebar_foreground))
                    .child(
                        Icon::new(if *expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    )
                    .child(label)
                    .when(*count > 0, |row| {
                        row.child(div().ml_auto().child(count.to_string()))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(shelf, cx)))
                    .into_any_element()
            }
            Row::ArchiveNotice(message) => base
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(message.clone())
                .into_any_element(),
            Row::ShowMore => base
                .text_xs()
                .text_color(theme.muted_foreground)
                .hover(|row| row.text_color(theme.sidebar_foreground))
                .child("Show more")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settled_visible += SETTLED_PAGE;
                    cx.notify();
                }))
                .into_any_element(),
        }
    }
}

fn thread_row(thread: &ThreadShell, settled: bool, now: i64) -> ThreadRow {
    let indicator = match thread.status() {
        ThreadStatus::Working | ThreadStatus::Waiting => Some(Indicator::Working),
        ThreadStatus::Approval | ThreadStatus::Input => Some(Indicator::NeedsYou),
        ThreadStatus::Failed | ThreadStatus::Limited => Some(Indicator::Failed),
        ThreadStatus::Ready if thread.has_unseen_completion() => Some(Indicator::Unread),
        ThreadStatus::Ready => None,
    };
    // Settled rows date the settle; the rest follow `threadTimeTimestamp`.
    let at = if settled {
        Some(thread.settled_at_ms())
    } else {
        timestamp_ms(Some(
            thread
                .latest_user_message_at
                .as_deref()
                .unwrap_or(&thread.updated_at),
        ))
    };
    let time = at.map(|at| relative_time(at, now)).unwrap_or_default();
    ThreadRow {
        id: thread.id.clone().into(),
        title: thread.title.clone().into(),
        indicator,
        time: time.into(),
        pinned: thread.pinned_at.is_some(),
    }
}

/// A static dot: continuously animating indicators keep the GPU busy on
/// high-refresh displays.
fn indicator(indicator: Option<Indicator>, cx: &App) -> Div {
    let theme = cx.theme();
    let color = indicator.map(|indicator| match indicator {
        Indicator::Working => theme.info,
        Indicator::NeedsYou => theme.warning,
        Indicator::Failed => theme.danger,
        Indicator::Unread => theme.primary,
    });
    div()
        .size_1p5()
        .flex_none()
        .rounded_full()
        .when_some(color, |dot, color| dot.bg(color))
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.rebuild_rows(cx);
        let theme = cx.theme();
        let status = match &self.env.read(cx).state {
            ConnectionState::Connected => None,
            ConnectionState::StartingServer => Some("Starting server…".into()),
            ConnectionState::Connecting => Some("Connecting…".into()),
            ConnectionState::Reconnecting => Some("Reconnecting…".into()),
            ConnectionState::Failed(_) => Some(SharedString::from("Server stopped")),
        };
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .h_12()
                    .px_4()
                    .justify_between()
                    .child(div().text_sm().font_semibold().child("T3 Code"))
                    .child(
                        h_flex()
                            .gap_2()
                            .when_some(status, |header, status| {
                                header.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(status),
                                )
                            })
                            .child(
                                Button::new("add-project")
                                    .accessibility_label("Add project")
                                    .icon(Icon::new(Lucide::FolderPlus))
                                    .ghost()
                                    .small()
                                    .tooltip("Add project")
                                    .on_click(
                                        cx.listener(|_, _, _, cx| {
                                            cx.emit(SidebarEvent::AddProject)
                                        }),
                                    ),
                            )
                            .child(
                                Button::new("new-thread")
                                    .icon(Icon::new(Lucide::SquarePen))
                                    .ghost()
                                    .small()
                                    .tooltip("New thread")
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::NewThread)),
                                    ),
                            ),
                    ),
            )
            .child(
                uniform_list(
                    "sidebar-rows",
                    self.rows.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        range.map(|ix| this.render_row(ix, cx)).collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .px_2()
                .track_scroll(&self.scroll),
            )
            .child(
                div().p_2().child(
                    Button::new("open-settings")
                        .label("Settings")
                        .icon(Icon::new(Lucide::Settings))
                        .ghost()
                        .small()
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::Settings))),
                ),
            )
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}
