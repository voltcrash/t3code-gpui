//! The native Settings surface, sharing the existing client preference names
//! and defaults. Environment and provider settings remain server-owned.

use crate::preferences::{self, Model};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Selectable, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_host::preferences::Appearance;

pub struct Closed;
pub struct Settings {
    model: Entity<Model>,
    focus: FocusHandle,
    _observe: Subscription,
}
impl EventEmitter<Closed> for Settings {}

impl Settings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = preferences::global(cx);
        let observe = cx.observe(&model, |_, _, cx| cx.notify());
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            model,
            focus,
            _observe: observe,
        }
    }
}

fn confirmation_row(model: &Entity<Model>, archive: bool, cx: &App) -> AnyElement {
    let state = model.read(cx);
    let (id, label, description, checked) = if archive {
        (
            "confirm-archive",
            "Confirm thread archiving",
            "Ask before archiving a thread.",
            state.value.confirm_thread_archive,
        )
    } else {
        (
            "confirm-delete",
            "Confirm thread deletion",
            "Ask before deleting a thread and its chat history.",
            state.value.confirm_thread_delete,
        )
    };
    let disabled = state.saving || !state.loaded;
    let model = model.clone();
    h_flex()
        .w_full()
        .gap_4()
        .py_3()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().font_medium().child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                ),
        )
        .child(
            Switch::new(id)
                .accessibility_label(label)
                .checked(checked)
                .disabled(disabled)
                .on_change(move |checked, window, cx| {
                    model.update(cx, |state, cx| {
                        let mut value = state.value.clone();
                        if archive {
                            value.confirm_thread_archive = *checked;
                        } else {
                            value.confirm_thread_delete = *checked;
                        }
                        state.change(value, window, cx);
                    })
                }),
        )
        .into_any_element()
}

impl Render for Settings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.model.read(cx);
        let appearance = state.value.appearance;
        let disabled = state.saving || !state.loaded;
        let loaded = state.loaded;
        let saving = state.saving;
        let error = state.error.clone();
        let model = self.model.clone();
        v_flex()
            .id("settings")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(Closed);
                    cx.stop_propagation();
                }
            }))
            .child(
                h_flex()
                    .h_12()
                    .flex_none()
                    .px_4()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().text_sm().font_medium().child("Settings"))
                    .child(
                        Button::new("close-settings")
                            .accessibility_label("Close Settings")
                            .tooltip("Close Settings")
                            .icon(Icon::new(Lucide::X))
                            .ghost()
                            .small()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                    ),
            )
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(800.))
                            .p_6()
                            .gap_6()
                            .child(
                                v_flex()
                                    .gap_3()
                                    .child(div().text_lg().font_semibold().child("Appearance"))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Choose how T3 Code looks on this device."),
                                    )
                                    .child(
                                        h_flex().gap_2().children(
                                            [
                                                (Appearance::System, "System"),
                                                (Appearance::Light, "Light"),
                                                (Appearance::Dark, "Dark"),
                                            ]
                                            .into_iter()
                                            .map(
                                                |(mode, label)| {
                                                    let model = model.clone();
                                                    Button::new(label)
                                                        .label(label)
                                                        .selected(appearance == mode)
                                                        .small()
                                                        .disabled(disabled)
                                                        .on_click(move |_, window, cx| {
                                                            model.update(cx, |state, cx| {
                                                                let mut value = state.value.clone();
                                                                value.appearance = mode;
                                                                state.change(value, window, cx);
                                                            })
                                                        })
                                                },
                                            ),
                                        ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(div().text_lg().font_semibold().child("Confirmations"))
                                    .child(confirmation_row(&self.model, true, cx))
                                    .child(confirmation_row(&self.model, false, cx)),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(div().text_lg().font_semibold().child("Agents"))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_4()
                                            .py_3()
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .gap_1()
                                                    .child(div().text_sm().font_medium().child("Plan mode"))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(cx.theme().muted_foreground)
                                                            .child("Enable Plan/Build controls for supported providers."),
                                                    ),
                                            )
                                            .child(
                                                Switch::new("plan-mode-enabled")
                                                    .accessibility_label("Enable Plan mode")
                                                    .checked(state.value.plan_mode_enabled)
                                                    .disabled(disabled)
                                                    .on_change({
                                                        let model = model.clone();
                                                        move |enabled, window, cx| {
                                                            model.update(cx, |state, cx| {
                                                                let mut value = state.value.clone();
                                                                value.plan_mode_enabled = *enabled;
                                                                state.change(value, window, cx);
                                                            })
                                                        }
                                                    }),
                                            ),
                                    ),
                            )
                            .when(saving, |column| {
                                column.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Saving…"),
                                )
                            })
                            .when_some(error, |column, error| {
                                column.child(
                                    v_flex()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().danger)
                                                .child(error),
                                        )
                                        .when(!loaded, |column| {
                                            let model = model.clone();
                                            column.child(
                                                Button::new("retry-preferences")
                                                    .label("Retry")
                                                    .small()
                                                    .disabled(saving)
                                                    .on_click(move |_, window, cx| {
                                                        model.update(cx, |state, cx| {
                                                            state.retry_load(window, cx)
                                                        })
                                                    }),
                                            )
                                        }),
                                )
                            }),
                    ),
            )
    }
}
