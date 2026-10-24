//! The window's root: sidebar beside the open thread.

use gpui_kit::component::StyledExt;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::environment::{ConnectionState, LocalEnvironment};
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::thread_view::ThreadView;

const SIDEBAR_WIDTH: Pixels = px(280.);

pub struct Workspace {
    env: Entity<LocalEnvironment>,
    sidebar: Entity<Sidebar>,
    thread: Option<Entity<ThreadView>>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(env: Entity<LocalEnvironment>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| Sidebar::new(env.clone(), cx));
        let subscriptions = vec![
            cx.observe(&env, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &sidebar,
                window,
                |this, _, event: &SidebarEvent, window, cx| match event {
                    SidebarEvent::SelectionChanged => this.open_selected(window, cx),
                },
            ),
        ];
        Self {
            env,
            sidebar,
            thread: None,
            _subscriptions: subscriptions,
        }
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.sidebar.read(cx).selected().cloned();
        let current = self.thread.as_ref().map(|view| view.read(cx).thread_id());
        if selected.as_ref() != current {
            self.thread =
                selected.map(|id| cx.new(|cx| ThreadView::new(self.env.clone(), id, window, cx)));
        }
        cx.notify();
    }

    fn render_main(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let env = self.env.read(cx);
        let thread = self.sidebar.read(cx).selected().and_then(|id| {
            env.shell
                .threads
                .get(id.as_ref())
                .or_else(|| env.shell.archived.get(id.as_ref()))
        });

        let header = h_flex()
            .h_12()
            .flex_none()
            .px_4()
            .gap_2()
            .border_b_1()
            .border_color(theme.border);
        let header = match thread {
            Some(thread) => {
                let project = env
                    .shell
                    .project(&thread.project_id)
                    .map(|project| project.title.clone());
                header
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .truncate()
                            .child(thread.title.clone()),
                    )
                    .when_some(project, |header, project| {
                        header.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(project),
                        )
                    })
                    .when_some(thread.branch.clone(), |header, branch| {
                        header.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(branch),
                        )
                    })
            }
            None => header.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground.opacity(0.5))
                    .child("No active thread"),
            ),
        };

        let centered = |content: Div| {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p_8()
                .child(content)
                .into_any_element()
        };
        let body = match (&env.state, thread.and(self.thread.as_ref())) {
            (ConnectionState::Failed(error), _) => centered(
                v_flex()
                    .gap_2()
                    .max_w(px(720.))
                    .child(
                        div()
                            .text_base()
                            .font_medium()
                            .child("The T3 Code server stopped"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(error.clone()),
                    ),
            ),
            (_, Some(view)) => v_flex()
                .flex_1()
                .min_h_0()
                .child(view.clone())
                .into_any_element(),
            (_, None) => centered(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_base()
                            .font_medium()
                            .child("Pick a thread to continue"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .text_color(theme.muted_foreground)
                            .child("Select an existing thread or create a new one to get started."),
                    ),
            ),
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.background)
            .child(header)
            .child(body)
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .size_full()
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .child(
                div()
                    .w(SIDEBAR_WIDTH)
                    .h_full()
                    .flex_none()
                    .child(self.sidebar.clone()),
            )
            .child(self.render_main(cx))
    }
}
