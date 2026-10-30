//! The open thread's conversation, live from `orchestration.subscribeThread`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use futures_util::StreamExt;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::SelectableText;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::requests::{self, PendingRequests};
use t3_client::thread::{
    self, Role, ThreadState, ThreadStreamItem, TimelineContent, TimelineEntry, WorkStatus,
};

use crate::appearance::CONTENT_WIDTH;
use crate::attachment_draft::AttachmentDraft;
use crate::attachment_view::AttachmentView;
use crate::composer::{Composer, Target};
use crate::environment::LocalEnvironment;
use crate::thread_find::{FindEvent, FindText, ThreadFind};
use crate::tokio_bridge;
use crate::tool_detail::ToolDetail;

/// `subscribeDynamic` retries an expected failure after 250ms; backing off
/// keeps a thread that keeps failing from spinning.
const RESUBSCRIBE_INITIAL: Duration = Duration::from_millis(250);
const RESUBSCRIBE_MAX: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Row {
    entry: TimelineEntry,
    /// Kept across rebuilds while the entry is unchanged, so the markdown view
    /// recognizes its text without comparing it.
    text: SharedString,
    detail: Option<SharedString>,
    attachments: Vec<Entity<AttachmentView>>,
}

#[derive(Clone)]
struct ActiveFind {
    query: String,
    selected: t3_client::find::Match,
    text: Option<Entity<FindText>>,
}

pub struct ThreadView {
    env: Entity<LocalEnvironment>,
    thread_id: SharedString,
    state: ThreadState,
    rows: Rc<[Row]>,
    expanded_work: Rc<HashSet<String>>,
    outputs: HashMap<String, (Entity<ToolDetail>, Subscription)>,
    pending: PendingRequests,
    scroller: Entity<MessageScrollerState>,
    composer: Entity<Composer>,
    error: Option<SharedString>,
    subscription: Option<Task<()>>,
    retry: Duration,
    history_task: Option<Task<()>>,
    history_error: Option<SharedString>,
    find: Entity<ThreadFind>,
    active_find: Option<ActiveFind>,
    find_return_focus: Option<FocusHandle>,
    find_reveal: bool,
    checkpoints: Rc<[t3_client::checkpoint::Turn]>,
    proposed_plan: Option<Rc<t3_client::planning::ProposedPlan>>,
    tasks: Option<Rc<t3_client::tasks::Tasks>>,
    tasks_expanded: bool,
    _find_events: Subscription,
    _env: Subscription,
}

impl ThreadView {
    pub fn new(
        env: Entity<LocalEnvironment>,
        thread_id: SharedString,
        attachments: Entity<AttachmentDraft>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Row zero holds the history control, even when the entire history is loaded.
        let scroller = cx.new(|cx| MessageScrollerState::new(1, cx));
        let this = cx.entity();
        let find = cx.new(|cx| ThreadFind::new(env.clone(), thread_id.clone(), window, cx));
        let find_events =
            cx.subscribe_in(&find, window, |view, _, event: &FindEvent, window, cx| {
                match event {
                    FindEvent::Closed => {
                        view.active_find = None;
                        view.find_reveal = false;
                        if let Some(focus) = view.find_return_focus.take() {
                            focus.focus(window, cx);
                        }
                    }
                    FindEvent::Selected {
                        query,
                        selected,
                        reveal,
                    } => {
                        if *reveal {
                            view.history_error = None;
                        }
                        view.active_find = selected.clone().map(|selected| ActiveFind {
                            query: query.clone(),
                            selected,
                            text: None,
                        });
                        view.find_reveal = *reveal;
                        view.reveal_find(cx);
                    }
                }
                cx.notify();
            });
        let composer = cx.new(|cx| {
            Composer::new(
                env.clone(),
                Target::Thread(thread_id.clone()),
                Some(&this),
                attachments,
                window,
                cx,
            )
        });
        composer.update(cx, |composer, cx| composer.focus(window, cx));
        let observe = cx.observe(&env, |this, _, cx| {
            this.ensure_subscribed(cx);
            this.refresh_tasks(cx);
            cx.notify();
        });
        let mut view = Self {
            env,
            thread_id,
            state: ThreadState::default(),
            rows: Rc::from([]),
            expanded_work: Rc::default(),
            outputs: HashMap::new(),
            pending: PendingRequests::default(),
            scroller,
            composer,
            error: None,
            subscription: None,
            retry: RESUBSCRIBE_INITIAL,
            history_task: None,
            history_error: None,
            find,
            active_find: None,
            find_return_focus: None,
            find_reveal: false,
            checkpoints: Rc::from([]),
            proposed_plan: None,
            tasks: None,
            tasks_expanded: false,
            _find_events: find_events,
            _env: observe,
        };
        view.ensure_subscribed(cx);
        view
    }

    /// The model and runtime mode the next turn runs with unless the
    /// composer picks others.
    pub fn settings(&self) -> Option<thread::ThreadSettings> {
        self.state.projection.as_ref()?.settings()
    }

    /// What the agent is waiting on the person for.
    pub fn pending(&self) -> &PendingRequests {
        &self.pending
    }

    pub fn thread_id(&self) -> &SharedString {
        &self.thread_id
    }

    pub fn checkpoints(&self) -> Rc<[t3_client::checkpoint::Turn]> {
        self.checkpoints.clone()
    }

    pub fn proposed_plan(&self) -> Option<Rc<t3_client::planning::ProposedPlan>> {
        self.proposed_plan.clone()
    }

    pub fn can_toggle_plan_mode(&self, cx: &App) -> bool {
        self.composer.read(cx).can_toggle_plan_mode(cx)
    }

    pub fn toggle_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.composer
            .update(cx, |composer, cx| composer.toggle_plan_mode(window, cx))
    }

    pub fn toggle_tasks(&mut self, cx: &mut Context<Self>) {
        self.tasks_expanded = !self.tasks_expanded;
        cx.notify();
    }
    fn refresh_tasks(&mut self, cx: &mut Context<Self>) {
        let next = self.state.projection.as_ref().and_then(|projection| {
            let env = self.env.read(cx);
            let shell = env.thread(self.thread_id.as_ref());
            t3_client::tasks::select(
                projection,
                shell.and_then(|thread| thread.active_run_id.as_deref()),
                shell.and_then(|thread| thread.latest_run_id.as_deref()),
            )
        });
        if self.tasks.as_deref() != next.as_ref() {
            if self.tasks.as_ref().map(|tasks| &tasks.id) != next.as_ref().map(|tasks| &tasks.id) {
                self.tasks_expanded = false;
            }
            self.tasks = next.map(Rc::new);
            cx.notify();
        }
    }

    fn refresh_proposed_plan(&mut self) {
        let plan = self
            .state
            .projection
            .as_ref()
            .and_then(t3_client::planning::active_plan);
        if self.proposed_plan.as_deref() != plan.as_ref() {
            self.proposed_plan = plan.map(Rc::new);
        }
    }

    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.read(cx).open {
            self.find_return_focus = window.focused(cx);
        }
        self.find.update(cx, |find, cx| find.open(window, cx));
        cx.notify();
    }

    fn reveal_find(&mut self, cx: &mut Context<Self>) {
        let Some(active) = &mut self.active_find else {
            return;
        };
        let Some(index) = self
            .rows
            .iter()
            .position(|row| row.entry.id == active.selected.entry_id)
        else {
            if self.find_reveal {
                if self.history_error.is_some() || (!self.state.partial && self.state.synchronized)
                {
                    self.find.update(cx, |find, cx| find.navigation_failed(cx));
                } else {
                    self.load_earlier(cx);
                }
            }
            return;
        };
        let row = &self.rows[index];
        if !matches!(
            row.entry.content,
            TimelineContent::Message {
                role: Role::User,
                ..
            }
        ) {
            if let Some(text) = &active.text {
                text.update(cx, |text, cx| text.set_source(row.text.clone(), cx));
            } else {
                active.text = Some(cx.new(|cx| {
                    FindText::new(
                        row.text.clone(),
                        active.query.clone(),
                        active.selected.occurrence,
                        self.find_reveal,
                        cx,
                    )
                }));
            }
        }
        if self.find_reveal {
            self.find_reveal = false;
            self.scroller.update(cx, |scroller, cx| {
                scroller.scroll_to_item(index + 1, cx);
            });
        }
    }

    /// Opens the stream when the environment has a socket and none is open.
    /// A reconnect resumes from the last applied sequence.
    fn ensure_subscribed(&mut self, cx: &mut Context<Self>) {
        if self.subscription.is_some() {
            return;
        }
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            return;
        };
        let input = thread::subscribe_input(&self.thread_id, self.state.sequence);
        self.state.synchronized = false;
        let retry = self.retry;
        self.subscription = Some(cx.spawn(async move |this, cx| {
            let mut items = rpc.subscribe::<_, ThreadStreamItem>(thread::SUBSCRIBE, &input);
            let mut failure = None;
            while let Some(item) = items.next().await {
                match item {
                    Ok(item) => {
                        let applied = this.update(cx, |view, cx| view.apply(item, cx));
                        if applied.is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            // A closed socket resubscribes once the environment reconnects.
            let retrying = !rpc.is_closed();
            if let Some(error) = &failure {
                tracing::warn!("thread stream ended: {error}");
            }
            if retrying {
                cx.background_executor().timer(retry).await;
            }
            let _ = this.update(cx, |view, cx| {
                view.subscription = None;
                if retrying {
                    view.retry = (view.retry * 2).min(RESUBSCRIBE_MAX);
                }
                if let Some(error) = failure.filter(|_| !rpc.is_closed()) {
                    view.error = Some(error.to_string().into());
                    cx.notify();
                }
                view.ensure_subscribed(cx);
            });
        }));
    }

    fn apply(&mut self, item: ThreadStreamItem, cx: &mut Context<Self>) {
        let snapshot = matches!(item, ThreadStreamItem::Snapshot { .. });
        let plan_changed = snapshot
            || matches!(&item, ThreadStreamItem::Event { event, .. } if event.kind == "plan.updated" || event.kind == "run.created" || event.kind == "run.updated" || (event.kind == "turn-item.updated" && event.payload["type"] == "proposed_plan"));
        let checkpoints_changed = snapshot
            || matches!(&item,
            ThreadStreamItem::Event { event, .. } if event.kind == "checkpoint.captured");
        if snapshot {
            self.outputs.clear();
            self.history_task = None;
            self.history_error = None;
        }
        if !self.state.apply(item) {
            return;
        }
        self.error = None;
        self.retry = RESUBSCRIBE_INITIAL;
        if let Some(projection) = &self.state.projection {
            self.pending = requests::pending_requests(projection);
            if checkpoints_changed {
                self.checkpoints = t3_client::checkpoint::turns(&projection.checkpoints).into();
            }
        }
        if plan_changed {
            self.refresh_proposed_plan();
        }
        self.refresh_tasks(cx);
        self.rebuild_rows(cx);
        self.sync_outputs(cx);
        self.reveal_find(cx);
        self.find.update(cx, |find, cx| find.refresh(cx));
        if snapshot {
            self.remeasure_history(cx);
        }
        cx.notify();
    }

    fn remeasure_history(&mut self, cx: &mut Context<Self>) {
        self.scroller.update(cx, |scroller, cx| {
            scroller.remeasure_items(0..1, cx);
        });
    }

    fn load_earlier(&mut self, cx: &mut Context<Self>) {
        if self.history_task.is_some() {
            return;
        }
        let Some(request) = self.state.history_request() else {
            return;
        };
        let Some(client) = self.env.read(cx).history.clone() else {
            return;
        };
        let thread_id = self.thread_id.clone();
        let cursor = request.cursor.clone();
        let through = self
            .active_find
            .as_ref()
            .filter(|_| self.find_reveal)
            .map(|active| active.selected.entry_id.clone());
        let fetching = tokio_bridge::spawn(cx, async move {
            client
                .page_through(&thread_id, &cursor, through.as_deref())
                .await
        });
        self.history_error = None;
        self.history_task = Some(cx.spawn(async move |this, cx| {
            let result = fetching.await;
            let _ = this.update(cx, |view, cx| {
                view.history_task = None;
                if !view.state.is_current_history_request(&request) {
                    return;
                }
                match result {
                    Ok(Ok(page)) => {
                        view.state.apply_history(&request, page);
                        view.refresh_proposed_plan();
                        view.rebuild_rows(cx);
                    }
                    Ok(Err(error)) => view.history_error = Some(error.to_string().into()),
                    Err(error) => {
                        view.history_error =
                            Some(format!("Could not load earlier messages: {error}").into())
                    }
                }
                view.remeasure_history(cx);
                view.reveal_find(cx);
                cx.notify();
            });
        }));
        self.remeasure_history(cx);
        cx.notify();
    }

    fn toggle_work(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(index) = self.rows.iter().position(|row| {
            row.entry.id == id
                && (row.detail.is_some()
                    || matches!(row.entry.content, TimelineContent::Work { tool: true, .. }))
        }) else {
            return;
        };
        let expanded = Rc::make_mut(&mut self.expanded_work);
        if !expanded.remove(id) {
            expanded.insert(id.to_owned());
        }
        self.sync_outputs(cx);
        self.scroller.update(cx, |scroller, cx| {
            scroller.remeasure_items(index + 1..index + 2, cx);
        });
        cx.notify();
    }

    fn sync_outputs(&mut self, cx: &mut Context<Self>) {
        let sources: HashMap<_, _> = self
            .state
            .projection
            .iter()
            .flat_map(|projection| &projection.visible_turn_items)
            .filter(|row| self.expanded_work.contains(&row.item.id))
            .filter_map(|row| {
                t3_client::tool_detail::source(row).map(|source| (row.item.id.clone(), source))
            })
            .collect();
        self.outputs.retain(|id, _| sources.contains_key(id));
        for (id, source) in sources {
            if let Some((view, _)) = self.outputs.get(&id) {
                view.update(cx, |view, cx| view.set_source(source, cx));
            } else {
                let view = cx.new(|cx| ToolDetail::new(self.env.clone(), source, cx));
                let item_id = id.clone();
                let observer = cx.observe(&view, move |this, _, cx| {
                    if let Some(index) = this.rows.iter().position(|row| row.entry.id == item_id) {
                        this.scroller.update(cx, |scroller, cx| {
                            scroller.remeasure_items(index + 1..index + 2, cx);
                        });
                    }
                    cx.notify();
                });
                self.outputs.insert(id, (view, observer));
            }
        }
    }

    /// Rebuilds the rows and remeasures only from the first one that changed,
    /// which during streaming is the last row.
    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let entries = self.state.timeline();
        let unchanged = self
            .rows
            .iter()
            .zip(&entries)
            .take_while(|(row, entry)| &row.entry == *entry)
            .count();
        if unchanged == entries.len() && unchanged == self.rows.len() {
            return;
        }
        if !self.expanded_work.is_empty() {
            let expandable: HashSet<&str> = entries.iter().filter(|entry| matches!(&entry.content, TimelineContent::Work { detail: Some(detail), .. } if !detail.trim().is_empty()) || matches!(entry.content, TimelineContent::Work { tool: true, .. })).map(|entry| entry.id.as_str()).collect();
            Rc::make_mut(&mut self.expanded_work).retain(|id| expandable.contains(id.as_str()));
        }
        let old_len = self.rows.len();
        // Keep the unchanged suffix when a history page prepends rows. Splicing only
        // the changed range preserves GPUI's measured rows and logical scroll anchor.
        let suffix = self.rows[unchanged..]
            .iter()
            .rev()
            .zip(entries[unchanged..].iter().rev())
            .take_while(|(row, entry)| &row.entry == *entry)
            .count();
        let new_len = entries.len();
        let prepended = old_len > 0 && unchanged == 0 && suffix == old_len;
        let rows: Vec<Row> = self.rows[..unchanged]
            .iter()
            .cloned()
            .chain(
                entries
                    .into_iter()
                    .skip(unchanged)
                    .take(new_len - unchanged - suffix)
                    .map(|entry| {
                        let attachments = match &entry.content {
                            TimelineContent::Message { attachments, .. } => attachments
                                .iter()
                                .map(|attachment| {
                                    cx.new(|cx| {
                                        AttachmentView::new(
                                            self.env.clone(),
                                            attachment.clone(),
                                            cx,
                                        )
                                    })
                                })
                                .collect(),
                            _ => Vec::new(),
                        };
                        Row {
                            detail: match &entry.content {
                                TimelineContent::Work { detail, .. } => detail
                                    .as_ref()
                                    .filter(|detail| !detail.trim().is_empty())
                                    .map(|detail| SharedString::from(detail.clone())),
                                _ => None,
                            },
                            text: row_text(&entry.content),
                            entry,
                            attachments,
                        }
                    }),
            )
            .chain(self.rows[old_len - suffix..].iter().cloned())
            .collect();
        self.rows = rows.into();
        self.scroller.update(cx, |scroller, cx| {
            let at_history_control = scroller.logical_scroll_top().item_ix == 0;
            scroller.splice(
                unchanged + 1..old_len - suffix + 1,
                new_len - unchanged - suffix,
                cx,
            );
            // The history control is not a message anchor. After clicking it at
            // the top, keep the first previously loaded message in view.
            if prepended && at_history_control {
                scroller.scroll_to_item(new_len - old_len + 1, cx);
            }
            // The first preserved row now has a different predecessor and gap.
            if suffix > 0 {
                let first = new_len - suffix + 1;
                scroller.remeasure_items(first..first + 1, cx);
            }
        });
    }
}

fn row_text(content: &TimelineContent) -> SharedString {
    match content {
        TimelineContent::Message { text, .. } => text.clone().into(),
        TimelineContent::ProposedPlan { markdown, .. } => markdown.clone().into(),
        TimelineContent::Work { label, .. } | TimelineContent::Event { label, .. } => {
            label.clone().into()
        }
    }
}

fn is_work(row: &Row) -> bool {
    matches!(row.entry.content, TimelineContent::Work { .. })
}

/// Consecutive work rows read as one block; everything else gets room. The
/// gap depends only on the row before, which a tail splice never changes.
fn gap_before(rows: &[Row], ix: usize) -> Pixels {
    match ix.checked_sub(1).map(|previous| &rows[previous]) {
        None => px(0.),
        Some(previous) if is_work(previous) && is_work(&rows[ix]) => px(6.),
        Some(_) => px(20.),
    }
}

fn render_row(
    rows: &[Row],
    ix: usize,
    active_find: Option<&ActiveFind>,
    expanded: bool,
    owner: WeakEntity<ThreadView>,
    output: Option<Entity<ToolDetail>>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let row = &rows[ix];
    let active = active_find.filter(|active| active.selected.entry_id == row.entry.id);
    let id = SharedString::from(row.entry.id.clone());
    let body = match &row.entry.content {
        TimelineContent::Message {
            role: Role::User, ..
        } => h_flex()
            .w_full()
            .justify_end()
            .child(
                div()
                    .max_w(relative(0.8))
                    .p_3()
                    .rounded_2xl()
                    .bg(crate::appearance::palette(cx).color("messageSurface"))
                    .text_color(crate::appearance::palette(cx).color("messageForeground"))
                    .text_sm()
                    .whitespace_normal()
                    .child(if active.is_none() {
                        SelectableText::new(id.clone(), row.text.clone()).into_any_element()
                    } else {
                        // SelectableText has no range-highlighting API.
                        StyledText::new(row.text.clone())
                            .with_highlights(active.into_iter().flat_map(|active| {
                                t3_client::find::ranges(&row.text, &active.query)
                                    .into_iter()
                                    .enumerate()
                                    .map(|(index, range)| {
                                        (
                                            range,
                                            HighlightStyle {
                                                background_color: Some(
                                                    crate::thread_find::highlight_color(
                                                        index == active.selected.occurrence,
                                                    ),
                                                ),
                                                ..Default::default()
                                            },
                                        )
                                    })
                            }))
                            .into_any_element()
                    }),
            )
            .into_any_element(),
        TimelineContent::Message {
            role: Role::Assistant,
            ..
        } => div()
            .w_full()
            .text_sm()
            .child(
                active
                    .and_then(|active| active.text.clone())
                    .map(|view| view.into_any_element())
                    .unwrap_or_else(|| {
                        TextView::markdown(id.clone(), row.text.clone())
                            .selectable(true)
                            .into_any_element()
                    }),
            )
            .into_any_element(),
        TimelineContent::ProposedPlan { status, .. } => v_flex()
            .w_full()
            .gap_2()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().font_medium().child("Plan"))
                    .when(status != "active", |header| header.child(status.clone())),
            )
            .child(
                div().text_sm().child(
                    active
                        .and_then(|active| active.text.clone())
                        .map(|view| view.into_any_element())
                        .unwrap_or_else(|| {
                            TextView::markdown(id.clone(), row.text.clone())
                                .selectable(true)
                                .into_any_element()
                        }),
                ),
            )
            .into_any_element(),
        TimelineContent::Work {
            status,
            thinking,
            tool,
            ..
        } => {
            let color = match status {
                WorkStatus::InProgress => theme.foreground,
                WorkStatus::Failed => theme.danger,
                _ => theme.muted_foreground,
            };
            let detail = row.detail.clone();
            let header = h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .text_xs()
                .text_color(color)
                .child(if detail.is_some() || *tool {
                    let entry_id = row.entry.id.clone();
                    Button::new((ElementId::from(id.clone()), "toggle-details"))
                        .ghost()
                        .xsmall()
                        .icon(if expanded {
                            Lucide::ChevronDown
                        } else {
                            Lucide::ChevronRight
                        })
                        .child(div().text_color(color).child(row.text.clone()))
                        .tooltip(if expanded {
                            "Hide activity details"
                        } else {
                            "Show activity details"
                        })
                        .accessibility_label(format!(
                            "{} {} details",
                            if expanded { "Hide" } else { "Show" },
                            row.text
                        ))
                        .on_click(move |_, _, cx| {
                            let _ = owner.update(cx, |view, cx| view.toggle_work(&entry_id, cx));
                        })
                        .into_any_element()
                } else {
                    div().child(row.text.clone()).into_any_element()
                })
                .when_some(
                    detail.clone().filter(|_| !thinking && !expanded),
                    |line, detail| {
                        line.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground.opacity(0.75))
                                .child(first_line(&detail)),
                        )
                    },
                );
            v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(header)
                .when(expanded, |column| {
                    column
                        .when_some(detail, |column, detail| {
                            column.child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .pl_4()
                                    .text_sm()
                                    .when(!thinking, |body| {
                                        body.font_family(theme.mono_font_family.clone())
                                    })
                                    .child(if *thinking {
                                        TextView::markdown(
                                            (ElementId::from(id.clone()), "work-detail"),
                                            detail,
                                        )
                                        .selectable(true)
                                        .into_any_element()
                                    } else {
                                        div()
                                            .whitespace_normal()
                                            .child(SelectableText::new(
                                                (ElementId::from(id.clone()), "work-detail"),
                                                detail,
                                            ))
                                            .into_any_element()
                                    }),
                            )
                        })
                        .when_some(output, |column, output| {
                            column.child(div().pl_4().child(output))
                        })
                })
                .into_any_element()
        }
        TimelineContent::Event { .. } => h_flex()
            .w_full()
            .justify_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(row.text.clone())
            .into_any_element(),
    };
    h_flex()
        .w_full()
        .justify_center()
        .px_6()
        .pt(gap_before(rows, ix))
        .child(
            div()
                .w_full()
                .max_w(CONTENT_WIDTH)
                .when(active.is_some(), |row| row.bg(theme.accent.opacity(0.1)))
                .child(body)
                .children(row.attachments.iter().map(|attachment| {
                    h_flex().w_full().justify_end().mt_2().child(
                        div()
                            .w_full()
                            .max_w(relative(0.8))
                            .child(attachment.clone()),
                    )
                }))
                .when(
                    matches!(
                        row.entry.content,
                        TimelineContent::Message { copyable: true, .. }
                    ),
                    |column| {
                        column.child(
                            h_flex()
                                .w_full()
                                .mt_1()
                                .when(
                                    matches!(
                                        row.entry.content,
                                        TimelineContent::Message {
                                            role: Role::User,
                                            ..
                                        }
                                    ),
                                    |actions| actions.justify_end(),
                                )
                                .child(
                                    Clipboard::new((ElementId::from(id), "copy-message"))
                                        .value(row.text.clone())
                                        .tooltip("Copy message")
                                        .accessibility_label("Copy message"),
                                ),
                        )
                    },
                ),
        )
        .into_any_element()
}

fn first_line(text: &str) -> SharedString {
    text.lines().next().unwrap_or_default().to_owned().into()
}

impl Render for ThreadView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let archived = self
            .env
            .read(cx)
            .thread(self.thread_id.as_ref())
            .is_some_and(|thread| thread.archived_at.is_some());
        let timeline = if self.state.projection.is_none() {
            let message = self
                .error
                .clone()
                .unwrap_or_else(|| "Loading thread…".into());
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(message)
                .into_any_element()
        } else {
            let rows = self.rows.clone();
            let expanded_work = self.expanded_work.clone();
            let outputs: HashMap<_, _> = self
                .outputs
                .iter()
                .map(|(id, (view, _))| (id.clone(), view.clone()))
                .collect();
            let active_find = self.active_find.clone();
            let partial = self.state.partial;
            let loading = self.history_task.is_some();
            let history_error = self.history_error.clone();
            let can_load =
                self.state.history_request().is_some() && self.env.read(cx).history.is_some();
            let this = cx.weak_entity();
            MessageScroller::new(
                SharedString::from(format!("thread-{}", self.thread_id)),
                self.scroller.clone(),
                move |ix, _, cx| {
                    if ix > 0 {
                        return render_row(
                            &rows,
                            ix - 1,
                            active_find.as_ref(),
                            expanded_work.contains(&rows[ix - 1].entry.id),
                            this.clone(),
                            outputs.get(&rows[ix - 1].entry.id).cloned(),
                            cx,
                        );
                    }
                    if !partial {
                        return div().into_any_element();
                    }
                    let this = this.clone();
                    v_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .pb_5()
                        .px_6()
                        .child(
                            Button::new("load-earlier")
                                .ghost()
                                .small()
                                .label(if loading {
                                    "Loading earlier messages…"
                                } else if history_error.is_some() {
                                    "Retry loading earlier messages"
                                } else {
                                    "Load earlier messages"
                                })
                                .disabled(loading || !can_load)
                                .on_click(move |_, _, cx| {
                                    let _ = this.update(cx, |view, cx| view.load_earlier(cx));
                                }),
                        )
                        .when_some(history_error.clone(), |column, error| {
                            column.child(div().text_xs().text_color(cx.theme().danger).child(error))
                        })
                        .into_any_element()
                },
            )
            .with_row_style(StyleRefinement::default().pb_0())
            .with_content_style(StyleRefinement::default().pt_6().pb_8())
            .with_bottom_fade(theme.background)
            .flex_1()
            .into_any_element()
        };
        v_flex()
            .size_full()
            .min_h_0()
            .child(self.find.clone())
            .child(timeline)
            .when(!archived, |column| {
                column.when_some(self.tasks.clone(), |column, tasks| {
                    column.child(crate::task_progress::render(
                        tasks,
                        self.tasks_expanded,
                        cx.weak_entity(),
                        cx,
                    ))
                })
            })
            .when(!archived, |column| column.child(self.composer.clone()))
            .when(archived, |column| {
                column.child(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(
                            "This thread is archived. Restore it from Thread actions to continue.",
                        ),
                )
            })
    }
}
