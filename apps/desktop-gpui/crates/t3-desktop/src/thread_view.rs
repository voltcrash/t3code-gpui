//! The open thread's conversation, live from `orchestration.subscribeThread`.

use std::rc::Rc;
use std::time::Duration;

use futures_util::StreamExt;
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::requests::{self, PendingRequests};
use t3_client::thread::{
    self, Role, ThreadState, ThreadStreamItem, TimelineContent, TimelineEntry, WorkStatus,
};

use crate::composer::{Composer, Target};
use crate::environment::LocalEnvironment;

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
    _env: Subscription,
}

impl ThreadView {
    pub fn new(
        env: Entity<LocalEnvironment>,
        thread_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
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
        let observe = cx.observe(&env, |this, _, cx| this.ensure_subscribed(cx));
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
            _env: observe,
        };
        view.ensure_subscribed(cx);
        view
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
        if !self.state.apply(item) {
            return;
        }
        self.error = None;
        self.retry = RESUBSCRIBE_INITIAL;
        if let Some(projection) = &self.state.projection {
            self.pending = requests::pending_requests(projection);
        }
        self.rebuild_rows(cx);
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
        let rows: Vec<Row> = self.rows[..unchanged]
            .iter()
            .cloned()
            .chain(entries.into_iter().skip(unchanged).map(|entry| Row {
                text: row_text(&entry.content),
                entry,
            }))
            .collect();
        let new_len = rows.len();
        self.rows = rows.into();
        self.scroller.update(cx, |scroller, cx| {
            scroller.splice(unchanged..old_len, new_len - unchanged, cx);
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
            MessageScroller::new(
                SharedString::from(format!("thread-{}", self.thread_id)),
                self.scroller.clone(),
                move |ix, _, cx| render_row(&rows, ix, cx),
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
            .child(self.composer.clone())
    }
}
