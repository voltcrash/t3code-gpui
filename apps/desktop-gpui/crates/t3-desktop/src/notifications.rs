//! Native OS notification boundary. Thread content is never interpreted as a command.

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::Arc;

    use gpui_kit::{App, Global, Subscription, Task};
    use notify_rust::{Notification, NotificationHandle, NotificationResponse};
    use t3_client::notifications::Notice;

    struct Pending {
        generation: u64,
        handle: Option<Arc<NotificationHandle>>,
        _task: Option<Task<()>>,
    }

    struct Notifications {
        pending: HashMap<String, Pending>,
        generation: u64,
        _preferences: Subscription,
        _quit: Subscription,
    }

    impl Global for Notifications {}

    fn take_handles(cx: &mut App) -> Vec<Arc<NotificationHandle>> {
        std::mem::take(&mut cx.global_mut::<Notifications>().pending)
            .into_values()
            .filter_map(|entry| entry.handle)
            .collect()
    }

    pub fn init(cx: &mut App) {
        let preferences = crate::preferences::global(cx);
        let observer = cx.observe(&preferences, |preferences, cx| {
            if !preferences.read(cx).value.desktop_notifications_enabled {
                clear(cx);
            }
        });
        let quit = cx.on_app_quit(|cx| {
            let handles = take_handles(cx);
            async move {
                for handle in handles {
                    handle.close_async().await;
                }
            }
        });
        cx.set_global(Notifications {
            pending: HashMap::new(),
            generation: 0,
            _preferences: observer,
            _quit: quit,
        });
    }

    pub fn clear(cx: &mut App) {
        let handles = take_handles(cx);
        if handles.is_empty() {
            return;
        }
        cx.spawn(async move |_| {
            for handle in handles {
                handle.close_async().await;
            }
        })
        .detach();
    }

    pub fn present(notice: Notice, cx: &mut App) {
        if !crate::preferences::global(cx)
            .read(cx)
            .value
            .desktop_notifications_enabled
            || crate::desktop::is_active(cx)
        {
            return;
        }
        let state = cx.global_mut::<Notifications>();
        state.generation += 1;
        let generation = state.generation;
        // Drop the previous task before showing its replacement, so a thread has
        // at most one action listener even if it completes many background turns.
        let previous = state
            .pending
            .remove(&notice.thread_id)
            .and_then(|entry| entry.handle);
        let previous_id = previous.as_ref().map(|handle| handle.id());
        let previous_on_error = previous.clone();
        let thread_id = notice.thread_id.clone();
        state.pending.insert(
            thread_id,
            Pending {
                generation,
                handle: previous,
                _task: None,
            },
        );
        // Let Notify finish when focus or preferences change during the D-Bus
        // call, so its stale result can still be closed.
        cx.spawn(async move |cx| {
            let body = notice
                .thread_title
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            let mut notification = Notification::new();
            notification
                .appname("T3 Code")
                .summary(notice.title)
                .body(&body)
                .hint(notify_rust::Hint::SuppressSound(true))
                .action("default", "Open thread");
            if let Some(id) = previous_id {
                notification.id(id);
            }
            let handle = match notification.show_async().await {
                Ok(handle) => Arc::new(handle),
                Err(error) => {
                    tracing::warn!(%error, "desktop notification failed");
                    if let Some(handle) = previous_on_error {
                        handle.close_async().await;
                    }
                    cx.update(|cx| {
                        let state = cx.global_mut::<Notifications>();
                        if state
                            .pending
                            .get(&notice.thread_id)
                            .is_some_and(|entry| entry.generation == generation)
                        {
                            state.pending.remove(&notice.thread_id);
                        }
                    });
                    return;
                }
            };
            let current = cx.update(|cx| {
                if let Some(entry) = cx
                    .global_mut::<Notifications>()
                    .pending
                    .get_mut(&notice.thread_id)
                    .filter(|entry| entry.generation == generation)
                {
                    entry.handle = Some(handle.clone());
                    true
                } else {
                    false
                }
            });
            if !current {
                handle.close_async().await;
                return;
            }
            let thread_id = notice.thread_id.clone();
            let listener = cx.spawn(async move |cx| {
                let mut clicked = false;
                handle
                    .wait_for_action_async(|response: &NotificationResponse| {
                        clicked = matches!(response, NotificationResponse::Default);
                    })
                    .await;
                if clicked {
                    handle.close_async().await;
                }
                cx.update(|cx| {
                    let state = cx.global_mut::<Notifications>();
                    if state
                        .pending
                        .get(&notice.thread_id)
                        .is_some_and(|entry| entry.generation == generation)
                    {
                        state.pending.remove(&notice.thread_id);
                        if clicked {
                            crate::desktop::open_notification_thread(notice.thread_id, cx);
                        }
                    }
                });
            });
            cx.update(|cx| {
                if let Some(entry) = cx.global_mut::<Notifications>().pending.get_mut(&thread_id) {
                    entry._task = Some(listener);
                }
            });
        })
        .detach();
    }
}

#[cfg(target_os = "linux")]
pub use linux::{clear, init, present};

#[cfg(not(target_os = "linux"))]
pub fn init(_cx: &mut gpui_kit::App) {}

#[cfg(not(target_os = "linux"))]
pub fn clear(_cx: &mut gpui_kit::App) {}

#[cfg(not(target_os = "linux"))]
pub fn present(_notice: t3_client::notifications::Notice, _cx: &mut gpui_kit::App) {}
