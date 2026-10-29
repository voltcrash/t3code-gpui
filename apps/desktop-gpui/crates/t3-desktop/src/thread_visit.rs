//! A window-local visit tracker; hidden conversations never advance read state.

use std::time::{Duration, Instant};

use gpui_kit::*;
use t3_client::visits;

use crate::{environment::LocalEnvironment, tokio_bridge};

pub struct ThreadVisit {
    env: Entity<LocalEnvironment>,
    thread_id: Option<SharedString>,
    visible: bool,
    state: visits::State,
    clock: Instant,
    task: Option<Task<()>>,
    in_flight: bool,
    _subscriptions: Vec<Subscription>,
}

impl ThreadVisit {
    pub fn new(env: Entity<LocalEnvironment>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe_in(&env, window, |this, _, window, cx| this.refresh(window, cx)),
            cx.observe_window_activation(window, |this, window, cx| this.refresh(window, cx)),
        ];
        Self {
            env,
            thread_id: None,
            visible: false,
            state: visits::State::default(),
            clock: Instant::now(),
            task: None,
            in_flight: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_target(
        &mut self,
        thread_id: Option<SharedString>,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.thread_id == thread_id && self.visible == visible {
            return;
        }
        if self.thread_id != thread_id {
            self.cancel();
            self.state = visits::State::default();
            self.thread_id = thread_id;
        }
        self.visible = visible;
        self.refresh(window, cx);
    }

    fn cancel(&mut self) {
        self.task = None;
        if self.in_flight {
            self.state.failed();
            self.in_flight = false;
        }
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env.read(cx);
        if !self.visible || !window.is_window_active() || !env.server.thread_visited_tracking {
            self.cancel();
            return;
        }
        let Some((rpc, thread)) = env
            .rpc
            .clone()
            .zip(self.thread_id.as_ref().and_then(|id| env.thread(id)))
        else {
            self.cancel();
            return;
        };
        if self.in_flight {
            return;
        }
        let delay = self.state.delay(thread, self.clock.elapsed());
        self.task = None;
        let Some(delay) = delay else {
            return;
        };
        if delay != Duration::ZERO {
            self.task = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update_in(cx, |this, window, cx| this.refresh(window, cx));
            }));
            return;
        }
        let command = visits::command(&thread.id, &thread.updated_at);
        self.state
            .dispatched(thread.updated_at.clone(), self.clock.elapsed());
        self.in_flight = true;
        let operation = tokio_bridge::spawn(cx, async move {
            t3_client::commands::dispatch(&rpc, &command).await
        });
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = operation.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.task = None;
                this.in_flight = false;
                match result {
                    Ok(Ok(_)) => this.refresh(window, cx),
                    result => {
                        this.state.failed();
                        tracing::warn!(?result, "thread visit failed");
                    }
                }
            });
        }));
    }
}
