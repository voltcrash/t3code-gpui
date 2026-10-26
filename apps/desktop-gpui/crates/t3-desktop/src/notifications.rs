//! Native OS notification boundary. Thread content is never interpreted as a command.
use gpui_kit::App;
use t3_client::notifications::Notice;

#[cfg(target_os = "linux")]
pub fn present(notice: Notice, cx: &mut App) {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    if !crate::preferences::global(cx)
        .read(cx)
        .value
        .desktop_notifications_enabled
        || crate::desktop::is_active(cx)
    {
        return;
    }
    type NotificationIds = Arc<Mutex<HashMap<String, (u32, u64)>>>;
    static IDS: OnceLock<NotificationIds> = OnceLock::new();
    let ids = IDS.get_or_init(Default::default).clone();
    let thread_id = notice.thread_id.clone();
    let (send, receive) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let body = notice
            .thread_title
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let mut notification = notify_rust::Notification::new();
        notification
            .appname("T3 Code")
            .summary(notice.title)
            .body(&body)
            .hint(notify_rust::Hint::SuppressSound(true))
            .action("default", "Open thread");
        let result = {
            let mut ids = ids.lock().expect("notification ids lock");
            if let Some((id, _)) = ids.get(&thread_id) {
                notification.id(*id);
            }
            let generation = ids
                .get(&thread_id)
                .map_or(1, |(_, generation)| generation + 1);
            notification.show().map(|handle| {
                ids.insert(thread_id.clone(), (handle.id(), generation));
                (handle, generation)
            })
        };
        match result {
            Ok((handle, generation)) => {
                let id = handle.id();
                handle.wait_for_action(|action| {
                    let mut ids = ids.lock().expect("notification ids lock");
                    let current = ids.get(&thread_id) == Some(&(id, generation));
                    if current {
                        ids.remove(&thread_id);
                    }
                    let _ = send.send(current && action == "default");
                });
            }
            Err(error) => {
                tracing::warn!(%error, "desktop notification failed");
            }
        }
    });
    cx.spawn(async move |cx| {
        if receive.await == Ok(true) {
            cx.update(|cx| crate::desktop::open_notification_thread(notice.thread_id, cx));
        }
    })
    .detach();
}

#[cfg(not(target_os = "linux"))]
pub fn present(_notice: Notice, _cx: &mut App) {}
