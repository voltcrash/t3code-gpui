//! The open thread's conversation, live from `orchestration.subscribeThread`.

use std::rc::Rc;
use std::time::Duration;

use futures_util::StreamExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::requests::{self, PendingRequests};
use t3_client::thread::{
    self, Role, ThreadState, ThreadStreamItem, TimelineContent, TimelineEntry, WorkStatus,
};

use crate::composer::{Composer, Target};
use crate::environment::LocalEnvironment;
use crate::tokio_bridge;

/// `subscribeDynamic` retries an expected failure after 250ms; backing off
/// keeps a thread that keeps failing from spinning.
const RESUBSCRIBE_INITIAL: Duration = Duration::from_millis(250);
const RESUBSCRIBE_MAX: Duration = Duration::from_secs(5);
const CONTENT_MAX_WIDTH: Pixels = px(768.);

#[derive(Clone)]
struct Row {
    entry: TimelineEntry,
    /// Kept across rebuilds while the entry is unchanged, so the markdown view
    /// recognizes its text without comparing it.
    text: SharedString,
}

pub struct ThreadView {
    env: Entity<LocalEnvironment>,
    thread_id: SharedString,
    state: ThreadState,
    rows: Rc<[Row]>,
    pending: PendingRequests,
    scroller: Entity<MessageScrollerState>,
    composer: Entity<Composer>,
    error: Option<SharedString>,
    subscription: Option<Task<()>>,
    retry: Duration,
    history_task: Option<Task<()>>,
    history_error: Option<SharedString>,
    _env: Subscription,
}

impl ThreadView {
    pub fn new(
        env: Entity<LocalEnvironment>,
        thread_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Row zero holds the history control, even when the entire history is loaded.
        let scroller = cx.new(|cx| MessageScrollerState::new(1, cx));
        let this = cx.entity();
        let composer = cx.new(|cx| {
            Composer::new(
                env.clone(),
                Target::Thread(thread_id.clone()),
                Some(&this),
                window,
                cx,
            )
        });
        composer.update(cx, |composer, cx| composer.focus(window, cx));
        let observe = cx.observe(&env, |this, _, cx| {
            this.ensure_subscribed(cx);
            cx.notify();
        });
        let mut view = Self {
            env,
            thread_id,
            state: ThreadState::default(),
            rows: Rc::from([]),
            pending: PendingRequests::default(),
            scroller,
            composer,
            error: None,
            subscription: None,
            retry: RESUBSCRIBE_INITIAL,
            history_task: None,
            history_error: None,
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
        if snapshot {
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
        }
        self.rebuild_rows(cx);
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
        let fetching =
            tokio_bridge::spawn(cx, async move { client.page(&thread_id, &cursor).await });
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
                        view.rebuild_rows(cx);
                    }
                    Ok(Err(error)) => view.history_error = Some(error.to_string().into()),
                    Err(error) => {
                        view.history_error =
                            Some(format!("Could not load earlier messages: {error}").into())
                    }
                }
                view.remeasure_history(cx);
                cx.notify();
            });
        }));
        self.remeasure_history(cx);
        cx.notify();
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
                    .map(|entry| Row {
                        text: row_text(&entry.content),
                        entry,
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

fn render_row(rows: &[Row], ix: usize, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let row = &rows[ix];
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
                    .px_3()
                    .py_2()
                    .rounded_xl()
                    .bg(theme.secondary)
                    .text_sm()
                    .whitespace_normal()
                    .child(row.text.clone()),
            )
            .into_any_element(),
        TimelineContent::Message {
            role: Role::Assistant,
            ..
        } => div()
            .w_full()
            .text_sm()
            .child(TextView::markdown(id, row.text.clone()).selectable(true))
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
                div()
                    .text_sm()
                    .child(TextView::markdown(id, row.text.clone()).selectable(true)),
            )
            .into_any_element(),
        TimelineContent::Work {
            detail,
            status,
            thinking,
            ..
        } => {
            let color = match status {
                WorkStatus::InProgress => theme.foreground,
                WorkStatus::Failed => theme.danger,
                _ => theme.muted_foreground,
            };
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .text_xs()
                .text_color(color)
                .child(div().flex_none().child(row.text.clone()))
                .when_some(detail.clone().filter(|_| !thinking), |line, detail| {
                    line.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground.opacity(0.75))
                            .child(first_line(&detail)),
                    )
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
        .child(div().w_full().max_w(CONTENT_MAX_WIDTH).child(body))
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
                        return render_row(&rows, ix - 1, cx);
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
            .child(timeline)
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
