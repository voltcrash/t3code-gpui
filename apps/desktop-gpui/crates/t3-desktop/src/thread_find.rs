//! Native find controls. Search ordering and counting stay on the existing server.

use std::time::Duration;

use futures_util::StreamExt;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::text::{RangeHighlight, RenderedText, TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::find::{self, Match, Request, SearchResult, State};

use crate::environment::LocalEnvironment;

pub enum FindEvent {
    Selected {
        query: String,
        selected: Option<Match>,
        reveal: bool,
    },
    Closed,
}

pub struct ThreadFind {
    env: Entity<LocalEnvironment>,
    thread_id: SharedString,
    input: Entity<InputState>,
    pub open: bool,
    state: State,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    refresh_pending: bool,
    available: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FindEvent> for ThreadFind {}

impl ThreadFind {
    pub fn new(
        env: Entity<LocalEnvironment>,
        thread_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in thread"));
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change if this.open => {
                        let value = this.input.read(cx).value();
                        if this.state.set_query(&value) {
                            this.task = None;
                            this.error = None;
                            cx.emit(FindEvent::Selected {
                                query: this.state.query.clone(),
                                selected: None,
                                reveal: false,
                            });
                            this.schedule(0, true, Duration::from_millis(150), cx);
                        }
                    }
                    InputEvent::PressEnter { shift, .. } => {
                        this.step(if *shift { -1 } else { 1 }, cx)
                    }
                    _ => {}
                },
            ),
            cx.observe(&env, |this, _, cx| {
                let env = this.env.read(cx);
                let available = env.rpc.is_some() && env.server.thread_find;
                if available != this.available {
                    this.available = available;
                    if this.open {
                        this.task = None;
                        if available {
                            let reveal = this.state.result.is_none();
                            this.schedule(0, reveal, Duration::ZERO, cx);
                        } else {
                            this.error = Some("Search is unavailable while disconnected.".into());
                        }
                        cx.notify();
                    }
                }
            }),
        ];
        let available = env.read(cx).rpc.is_some() && env.read(cx).server.thread_find;
        Self {
            env,
            thread_id,
            input,
            open: false,
            state: State::default(),
            error: None,
            task: None,
            refresh_pending: false,
            available,
            _subscriptions: subscriptions,
        }
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.available {
            return;
        }
        self.open = true;
        self.input.update(cx, |input, cx| {
            input.focus_handle(cx).focus(window, cx);
            input.select_all(window, cx);
        });
        // The application menu restores its old focus after dispatching the action.
        // Focus again once it has closed, provided this find bar is still open.
        let this = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.open {
                    this.input.update(cx, |input, cx| {
                        input.focus_handle(cx).focus(window, cx);
                        input.select_all(window, cx);
                    });
                }
            });
        });
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.task = None;
        self.refresh_pending = false;
        self.state.clear();
        self.error = None;
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.emit(FindEvent::Closed);
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.open || !self.state.valid_query() {
            return;
        }
        if self.task.is_some() {
            self.refresh_pending = true;
        } else {
            self.schedule(0, false, Duration::from_millis(300), cx);
        }
    }

    pub fn navigation_failed(&mut self, cx: &mut Context<Self>) {
        self.error = Some("Could not load the search result. Retry to search again.".into());
        cx.notify();
    }

    fn step(&mut self, offset: i32, cx: &mut Context<Self>) {
        if self.task.is_some()
            || self
                .state
                .result
                .as_ref()
                .is_none_or(|result| result.total_matches == 0 || result.complete == Some(false))
        {
            return;
        }
        self.schedule(offset, true, Duration::ZERO, cx);
    }

    fn schedule(&mut self, offset: i32, reveal: bool, delay: Duration, cx: &mut Context<Self>) {
        self.refresh_pending = false;
        self.error = None;
        if !self.open {
            return;
        }
        let Some(request) = self.state.begin(offset) else {
            cx.notify();
            return;
        };
        let Some(rpc) = self.env.read(cx).rpc.clone() else {
            self.error = Some("Search is unavailable while disconnected.".into());
            cx.notify();
            return;
        };
        let progressive = self.env.read(cx).server.thread_find_progressive;
        let input = request.input(&self.thread_id);
        self.task = Some(cx.spawn(async move |this, cx| {
            if delay != Duration::ZERO {
                cx.background_executor().timer(delay).await;
            }
            let result = if progressive {
                let mut updates = rpc.subscribe::<_, SearchResult>(find::SEARCH_STREAM, &input);
                let mut completed = false;
                let mut reveal = reveal;
                let mut failure = None;
                while let Some(item) = updates.next().await {
                    match item {
                        Ok(result) => {
                            let complete = result.complete != Some(false);
                            if this
                                .update(cx, |this, cx| this.apply(&request, result, reveal, cx))
                                .is_err()
                            {
                                return;
                            }
                            reveal = false;
                            if complete {
                                completed = true;
                                break;
                            }
                        }
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                }
                failure.map_or_else(
                    || {
                        if completed {
                            Ok(())
                        } else {
                            Err("Search ended before returning results.".to_owned())
                        }
                    },
                    |error| Err(error.detail()),
                )
            } else {
                match rpc.call::<_, SearchResult>(find::SEARCH, &input).await {
                    Ok(result) => {
                        let _ =
                            this.update(cx, |this, cx| this.apply(&request, result, reveal, cx));
                        Ok(())
                    }
                    Err(error) => Err(error.detail()),
                }
            };
            let _ = this.update(cx, |this, cx| {
                if !this.state.is_current(&request) {
                    return;
                }
                this.task = None;
                if let Err(error) = result {
                    tracing::warn!("thread search failed: {error}");
                    this.error = Some("Search failed. Check the connection and retry.".into());
                } else if this.refresh_pending {
                    this.schedule(0, false, Duration::from_millis(300), cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn apply(
        &mut self,
        request: &Request,
        result: SearchResult,
        reveal: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = self
            .state
            .result
            .as_ref()
            .and_then(|result| result.selected.as_ref())
            != result.selected.as_ref();
        if !self.state.apply(request, result) {
            return;
        }
        if reveal || changed {
            cx.emit(FindEvent::Selected {
                query: self.state.query.clone(),
                selected: self
                    .state
                    .result
                    .as_ref()
                    .and_then(|result| result.selected.clone()),
                reveal,
            });
        }
        cx.notify();
    }
}

impl Render for ThreadFind {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let result = self.state.result.as_ref();
        let busy = self.task.is_some();
        let disabled = busy
            || result
                .is_none_or(|result| result.total_matches == 0 || result.complete == Some(false))
            || !self.available;
        let label = if self.error.is_some() {
            "Search failed".to_owned()
        } else if !self.state.query.is_empty() && !self.state.valid_query() {
            "200 character limit".to_owned()
        } else if busy && result.is_none() {
            "Searching…".to_owned()
        } else if let Some(result) = result {
            if result.complete == Some(false) {
                "1/…".to_owned()
            } else if result.total_matches == 0 {
                "0/0".to_owned()
            } else {
                format!("{}/{}", result.active_index + 1, result.total_matches)
            }
        } else {
            String::new()
        };
        v_flex()
            .w_full()
            .flex_none()
            .px_3()
            .py_2()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.input).small()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .flex_none()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .when(self.error.is_some(), |row| {
                        row.child(
                            Button::new("retry-find")
                                .label("Retry")
                                .ghost()
                                .small()
                                .disabled(busy || !self.available)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.schedule(0, true, Duration::ZERO, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("previous-match")
                            .icon(Icon::new(Lucide::ChevronUp))
                            .ghost()
                            .small()
                            .tooltip("Previous match (Shift+Enter)")
                            .accessibility_label("Previous match")
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, _, cx| this.step(-1, cx))),
                    )
                    .child(
                        Button::new("next-match")
                            .icon(Icon::new(Lucide::ChevronDown))
                            .ghost()
                            .small()
                            .tooltip("Next match (Enter)")
                            .accessibility_label("Next match")
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, _, cx| this.step(1, cx))),
                    )
                    .child(
                        Button::new("close-find")
                            .icon(Icon::new(Lucide::X))
                            .ghost()
                            .small()
                            .tooltip("Close find (Escape)")
                            .accessibility_label("Close find")
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
            .when_some(self.error.clone(), |column, error| {
                column.child(div().text_xs().text_color(cx.theme().danger).child(error))
            })
            .into_any_element()
    }
}

/// Only the selected message needs an owned text state. The rest of a long
/// transcript keeps its existing lazy, virtualized Markdown rendering.
pub struct FindText {
    text: Entity<TextViewState>,
    source: SharedString,
    query: String,
    occurrence: usize,
    rendered: Option<RenderedText>,
    reveal: bool,
    _observe: Subscription,
}

impl FindText {
    pub fn new(
        source: SharedString,
        query: String,
        occurrence: usize,
        reveal: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = cx.new(|cx| TextViewState::markdown(&source, cx).selectable(true));
        let observe = cx.observe(&text, |this, _, cx| {
            this.highlight(cx);
            cx.notify();
        });
        let mut this = Self {
            text,
            source,
            query,
            occurrence,
            rendered: None,
            reveal,
            _observe: observe,
        };
        this.highlight(cx);
        this
    }

    pub fn set_source(&mut self, source: SharedString, cx: &mut Context<Self>) {
        if self.source == source {
            return;
        }
        self.source = source;
        self.text
            .update(cx, |text, cx| text.set_text(&self.source, cx));
    }

    fn highlight(&mut self, cx: &mut Context<Self>) {
        let rendered = self.text.read(cx).rendered_text();
        if self.rendered.as_ref() == Some(&rendered) {
            return;
        }
        let ranges = find::ranges(rendered.as_str(), &self.query);
        let selected = ranges.get(self.occurrence).cloned();
        self.rendered = Some(rendered);
        self.text.update(cx, |text, cx| {
            let _ = text.set_range_highlights(
                ranges.into_iter().enumerate().map(|(index, range)| {
                    RangeHighlight::new(range, highlight_color(index == self.occurrence))
                }),
                cx,
            );
            if self.reveal
                && let Some(selected) = selected
            {
                let _ = text.reveal_range(selected, cx);
                self.reveal = false;
            }
        });
    }
}

pub fn highlight_color(active: bool) -> Hsla {
    hsla(0.12, 0.9, 0.5, if active { 0.6 } else { 0.25 })
}

impl Render for FindText {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        TextView::new(&self.text).selectable(true)
    }
}
