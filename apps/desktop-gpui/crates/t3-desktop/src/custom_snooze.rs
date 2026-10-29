//! Custom snooze dialog with the same date/time and duration choices as web.

use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::snooze::{self, Custom, Unit};

pub enum Event {
    Submitted(String),
    Closed,
}

pub struct CustomSnooze {
    pub thread_id: String,
    title: String,
    date: Entity<InputState>,
    time: Entity<InputState>,
    amount: Entity<InputState>,
    duration_mode: bool,
    unit: Unit,
    busy: bool,
    error: Option<SharedString>,
    focus: FocusHandle,
    focus_pending: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Event> for CustomSnooze {}

impl CustomSnooze {
    pub fn new(
        thread_id: String,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (date_value, time_value) = snooze::initial_custom_date_time().unwrap_or_default();
        let field = |placeholder, value: String, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                input.set_value(value, window, cx);
                input
            })
        };
        let mut field = field;
        let date = field("YYYY-MM-DD", date_value, cx);
        let time = field("HH:MM", time_value, cx);
        let amount = field("Amount", "2".into(), cx);
        let subscriptions = [&date, &time, &amount]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit(cx);
                    }
                })
            })
            .collect();
        Self {
            thread_id,
            title,
            date,
            time,
            amount,
            duration_mode: false,
            unit: Unit::Hours,
            busy: false,
            error: None,
            focus: cx.focus_handle(),
            focus_pending: true,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.busy = busy;
        self.error = None;
        cx.notify();
    }

    pub fn failed(&mut self, error: SharedString, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(error);
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let date = self.date.read(cx).value();
        let time = self.time.read(cx).value();
        let amount = self.amount.read(cx).value();
        let input = if self.duration_mode {
            Custom::Duration {
                amount: &amount,
                unit: self.unit,
            }
        } else {
            Custom::DateTime {
                date: &date,
                time: &time,
            }
        };
        if let Some(until) = snooze::resolve_custom_now(input) {
            cx.emit(Event::Submitted(until));
        } else {
            self.error = Some(
                if self.duration_mode {
                    "Enter a positive duration."
                } else {
                    "Choose a valid local date and time in the future."
                }
                .into(),
            );
            cx.notify();
        }
    }
}

impl Render for CustomSnooze {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_pending {
            self.focus_pending = false;
            let this = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.date.read(cx).focus_handle(cx).focus(window, cx)
                });
            });
        }
        v_flex()
            .id("custom-snooze-dialog")
            .track_focus(&self.focus)
            .role(Role::Dialog)
            .aria_label("Custom snooze")
            .w_full()
            .max_w(px(420.))
            .p_4()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    if !this.busy {
                        cx.emit(Event::Closed);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(div().text_sm().font_medium().child("Custom snooze"))
            .child(div().text_sm().child(self.title.clone()))
            .child(
                h_flex()
                    .gap_2()
                    .children([false, true].into_iter().map(|duration| {
                        Button::new(if duration {
                            "snooze-duration"
                        } else {
                            "snooze-date-time"
                        })
                        .label(if duration {
                            "Duration"
                        } else {
                            "Date and time"
                        })
                        .small()
                        .when(self.duration_mode == duration, |button| button.primary())
                        .disabled(self.busy)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.duration_mode = duration;
                                this.error = None;
                                let input = if duration { &this.amount } else { &this.date };
                                input.read(cx).focus_handle(cx).focus(window, cx);
                                cx.notify();
                            },
                        ))
                    })),
            )
            .when(!self.duration_mode, |column| {
                column.child(
                    h_flex()
                        .gap_2()
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1()
                                .child(div().text_xs().child("Date (YYYY-MM-DD)"))
                                .child(Input::new(&self.date).disabled(self.busy)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1()
                                .child(div().text_xs().child("Local time (HH:MM)"))
                                .child(Input::new(&self.time).disabled(self.busy)),
                        ),
                )
            })
            .when(self.duration_mode, |column| {
                column
                    .child(div().text_xs().child("Snooze for"))
                    .child(Input::new(&self.amount).disabled(self.busy))
                    .child(
                        h_flex().gap_2().children(
                            [Unit::Minutes, Unit::Hours, Unit::Days]
                                .into_iter()
                                .enumerate()
                                .map(|(index, unit)| {
                                    Button::new(("snooze-unit", index))
                                        .label(unit.label())
                                        .small()
                                        .when(self.unit == unit, |button| button.primary())
                                        .disabled(self.busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.unit = unit;
                                            this.error = None;
                                            cx.notify();
                                        }))
                                }),
                        ),
                    )
            })
            .when_some(self.error.clone(), |column, error| {
                column.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel-snooze")
                            .label("Cancel")
                            .small()
                            .disabled(self.busy)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Closed))),
                    )
                    .child(
                        Button::new("submit-snooze")
                            .label("Snooze")
                            .primary()
                            .small()
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
            .focus_trap("custom-snooze", &self.focus)
    }
}
