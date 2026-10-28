//! Server plans drive this drawer; only expansion state belongs to the client.
use crate::{appearance::CONTENT_WIDTH, thread_view::ThreadView};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{base::SelectableText, *};
use std::rc::Rc;
use t3_client::tasks::Tasks;

pub fn render(
    tasks: Rc<Tasks>,
    expanded: bool,
    owner: WeakEntity<ThreadView>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let completed = tasks.completed();
    let total = tasks.steps.len();
    h_flex().w_full().justify_center().px_6().pb_2().child(
        v_flex()
            .id(SharedString::from(format!("tasks-{}", tasks.id)))
            .w_full()
            .max_w(CONTENT_WIDTH)
            .min_w_0()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .child(
                Button::new("toggle-tasks")
                    .ghost()
                    .small()
                    .w_full()
                    .accessibility_label(format!(
                        "{} tasks: {completed} of {total} complete. Current task: {}",
                        if expanded { "Collapse" } else { "Expand" },
                        tasks.current()
                    ))
                    .tooltip(if expanded {
                        "Collapse tasks"
                    } else {
                        "Expand tasks"
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .text_xs()
                            .child(Icon::new(Lucide::ListTodo).xsmall())
                            .child(div().text_color(theme.muted_foreground).child("Tasks"))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(tasks.current().to_owned()),
                            )
                            .child(
                                div()
                                    .text_color(if completed == total {
                                        theme.success
                                    } else {
                                        theme.muted_foreground
                                    })
                                    .child(format!("{completed}/{total}")),
                            )
                            .when((2..=10).contains(&total), |row| {
                                row.child(h_flex().w(px(64.)).gap(px(2.)).children(
                                    tasks.steps.iter().map(|step| {
                                        div().flex_1().h(px(3.)).rounded_full().bg(
                                            match step.status.as_str() {
                                                "completed" => theme.success,
                                                "running" => theme.primary,
                                                _ => theme.muted_foreground.opacity(0.25),
                                            },
                                        )
                                    }),
                                ))
                            })
                            .child(
                                Icon::new(if expanded {
                                    Lucide::ChevronDown
                                } else {
                                    Lucide::ChevronRight
                                })
                                .xsmall(),
                            ),
                    )
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |view, cx| view.toggle_tasks(cx));
                    }),
            )
            .when(expanded, |column| {
                column.child(
                    v_flex()
                        .id("task-steps")
                        .w_full()
                        .min_w_0()
                        .max_h(px(160.))
                        .overflow_y_scroll()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .children(tasks.steps.iter().map(|step| {
                            let (icon, color) = match step.status.as_str() {
                                "completed" => (Lucide::Check, theme.success),
                                "running" => (Lucide::CircleDot, theme.primary),
                                _ => (Lucide::Circle, theme.muted_foreground),
                            };
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .items_start()
                                .gap_2()
                                .child(Icon::new(icon).xsmall().text_color(color))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .whitespace_normal()
                                        .text_color(if step.status == "completed" {
                                            theme.muted_foreground
                                        } else {
                                            theme.foreground
                                        })
                                        .child(SelectableText::new(
                                            SharedString::from(step.id.clone()),
                                            step.text.clone(),
                                        )),
                                )
                                .child(div().text_xs().text_color(theme.muted_foreground).child(
                                    step.duration_ms.map(duration).unwrap_or_else(|| {
                                        if tasks.live && step.status == "running" {
                                            "now".into()
                                        } else {
                                            String::new()
                                        }
                                    }),
                                ))
                        })),
                )
            }),
    )
}

fn duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{}ms", ms.max(1));
    }
    if ms < 10_000 {
        let tenths = (ms + 50) / 100;
        return if tenths >= 100 {
            "10s".into()
        } else {
            format!("{}.{:01}s", tenths / 10, tenths % 10)
        };
    }
    let seconds = (ms + 500) / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let mut parts = Vec::new();
    if seconds / 3600 > 0 {
        parts.push(format!("{}h", seconds / 3600));
    }
    if seconds % 3600 / 60 > 0 {
        parts.push(format!("{}m", seconds % 3600 / 60));
    }
    if !seconds.is_multiple_of(60) {
        parts.push(format!("{}s", seconds % 60));
    }
    parts.join(" ")
}
#[cfg(test)]
mod tests {
    use super::duration;
    #[test]
    fn durations_match_the_existing_composer_rounding_boundaries() {
        for (ms, label) in [
            (0, "1ms"),
            (999, "999ms"),
            (1050, "1.1s"),
            (9950, "10s"),
            (59500, "1m"),
            (3661000, "1h 1m 1s"),
        ] {
            assert_eq!(duration(ms), label);
        }
    }
}
