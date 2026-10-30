//! Keep resize events in memory; write once on close or quit, never during a drag.

use gpui_kit::*;
use t3_host::window_state::{Placement, Rectangle, Store};

struct WindowState {
    store: Store,
    saved: Option<Placement>,
    current: Option<Placement>,
    #[cfg(target_os = "linux")]
    requested: Option<Placement>,
    dirty: bool,
}
impl Global for WindowState {}

pub fn init(home: &std::path::Path, cx: &mut App) {
    let store = Store::new(home);
    let saved = match store.load() {
        Ok(saved) => saved,
        Err(error) => {
            tracing::warn!(%error, "unable to load native window placement");
            None
        }
    };
    cx.set_global(WindowState {
        store,
        saved,
        current: None,
        #[cfg(target_os = "linux")]
        requested: None,
        dirty: false,
    });
    cx.on_app_quit(|cx| {
        save(cx);
        async {}
    })
    .detach();
}

fn rectangle(bounds: Bounds<Pixels>) -> Rectangle {
    Rectangle {
        x: bounds.origin.x.into(),
        y: bounds.origin.y.into(),
        width: bounds.size.width.into(),
        height: bounds.size.height.into(),
    }
}

pub fn initial(cx: &mut App) -> WindowBounds {
    let displays = cx
        .displays()
        .iter()
        .map(|display| rectangle(display.bounds()))
        .collect::<Vec<_>>();
    let restored = cx
        .global::<WindowState>()
        .saved
        .and_then(|state| state.on_displays(&displays));
    let state = restored.unwrap_or_else(|| {
        let bounds = WindowBounds::centered(size(px(1280.), px(820.)), cx).get_bounds();
        Placement {
            bounds: rectangle(bounds),
            maximized: false,
            view: cx
                .global::<WindowState>()
                .saved
                .map(|state| state.view)
                .unwrap_or_default(),
        }
    });
    let model = cx.global_mut::<WindowState>();
    model.current = Some(state);
    #[cfg(target_os = "linux")]
    {
        model.requested = Some(state);
    }
    let bounds = Bounds {
        origin: point(px(state.bounds.x), px(state.bounds.y)),
        size: size(px(state.bounds.width), px(state.bounds.height)),
    };
    if state.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

pub fn view(cx: &App) -> t3_host::window_state::View {
    cx.global::<WindowState>()
        .current
        .map(|state| state.view)
        .unwrap_or_default()
}

pub fn remember_view(view: t3_host::window_state::View, cx: &mut App) {
    let state = cx.global_mut::<WindowState>();
    if let Some(current) = &mut state.current
        && current.view != view
    {
        current.view = view;
        state.dirty = true;
    }
}

pub fn remember(window: &Window, cx: &mut App) {
    let state = cx.global_mut::<WindowState>();
    let Some(previous) = state.current else {
        return;
    };
    // X11 reports maximized geometry rather than the restore rectangle.
    // Keep the last normal bounds across maximization and fullscreen transitions.
    let next = if window.is_fullscreen() {
        previous
    } else if window.is_maximized() {
        Placement {
            maximized: true,
            ..previous
        }
    } else {
        Placement {
            bounds: rectangle(window.window_bounds().get_bounds()),
            maximized: false,
            ..previous
        }
    };
    if previous != next {
        state.current = Some(next);
        state.dirty = true;
    }
}

pub fn save(cx: &mut App) {
    let state = cx.global_mut::<WindowState>();
    if !state.dirty {
        return;
    }
    if let Some(placement) = state.current {
        match state.store.save(placement) {
            Ok(()) => {
                state.saved = Some(placement);
                state.dirty = false;
            }
            Err(error) => tracing::warn!(%error, "unable to save native window placement"),
        }
    }
}

#[cfg(target_os = "linux")]
pub fn restore_position(window: &Window, cx: &App) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt as _, EventMask};
    let Some(placement) = cx.global::<WindowState>().requested else {
        return;
    };
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let id = match handle.as_raw() {
        RawWindowHandle::Xcb(handle) => handle.window.get(),
        RawWindowHandle::Xlib(handle) => handle.window as u32,
        _ => return,
    };
    let result = (|| -> anyhow::Result<()> {
        let (connection, screen) = x11rb::connect(None)?;
        let root = connection.setup().roots[screen].root;
        let mut hints = x11rb::properties::WmSizeHints::get_normal_hints(&connection, id)?
            .reply()?
            .unwrap_or_default();
        hints.win_gravity = Some(x11rb::protocol::xproto::Gravity::STATIC);
        hints.set_normal_hints(&connection, id)?.check()?;
        let atom = connection
            .intern_atom(false, b"_NET_MOVERESIZE_WINDOW")?
            .reply()?
            .atom;
        let scale = window.scale_factor();
        // GPUI's X11 initial bounds omit position hints, so WMs choose a new
        // position. EWMH restores the client origin without changing its size.
        let event = ClientMessageEvent::new(
            32,
            id,
            atom,
            [
                10 | (1 << 8) | (1 << 9) | (1 << 12),
                (placement.bounds.x * scale) as i32 as u32,
                (placement.bounds.y * scale) as i32 as u32,
                0,
                0,
            ],
        );
        connection
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )?
            .check()?;
        if placement.maximized {
            // GPUI zooms before mapping on X11, where the WM ignores the request.
            // ADD is idempotent if a WM already applied the startup maximization.
            let state = connection
                .intern_atom(false, b"_NET_WM_STATE")?
                .reply()?
                .atom;
            let horizontal = connection
                .intern_atom(false, b"_NET_WM_STATE_MAXIMIZED_HORZ")?
                .reply()?
                .atom;
            let vertical = connection
                .intern_atom(false, b"_NET_WM_STATE_MAXIMIZED_VERT")?
                .reply()?
                .atom;
            connection
                .send_event(
                    false,
                    root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    ClientMessageEvent::new(32, id, state, [1, horizontal, vertical, 1, 0]),
                )?
                .check()?;
        }
        connection.flush()?;
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(%error, "unable to restore X11 window position");
    }
}
