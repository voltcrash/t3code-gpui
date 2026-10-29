//! Native application menus and the lifetime of the single desktop window.

use gpui_kit::base::GlobalState;
use gpui_kit::component::input::{Copy, Cut, Delete, Paste, Redo, SelectAll, Undo};
use gpui_kit::*;
use t3_host::BackendLaunch;

use crate::environment::LocalEnvironment;
use crate::workspace::{
    FindInThread, NewThread, ToggleChanges, ToggleCommandPalette, ToggleSidebar, Workspace,
};

actions!(
    desktop,
    [
        Quit,
        CloseWindow,
        Minimize,
        Maximize,
        ToggleFullscreen,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        PasteAsText,
        About,
        Hide,
        HideOthers,
        ShowAll,
        OpenSettings
    ]
);

struct Desktop {
    // On macOS the app and its backend remain alive with no windows. A dock
    // activation reopens the UI on the same connection rather than starting
    // another server.
    env: Entity<LocalEnvironment>,
    window: Option<AnyWindowHandle>,
    workspace: Option<Entity<Workspace>>,
}

impl Global for Desktop {}

pub fn init(launch: BackendLaunch, cx: &mut App) {
    let env = cx.new(|cx| LocalEnvironment::start(launch, cx));
    cx.set_global(Desktop {
        env,
        window: None,
        workspace: None,
    });
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-w", CloseWindow, None),
        KeyBinding::new("secondary-=", ZoomIn, None),
        KeyBinding::new("secondary-+", ZoomIn, None),
        KeyBinding::new("secondary--", ZoomOut, None),
        KeyBinding::new("secondary-0", ResetZoom, None),
        KeyBinding::new("secondary-shift-v", PasteAsText, None),
    ]);
    if cfg!(target_os = "macos") {
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.bind_keys([
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("cmd-alt-h", HideOthers, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullscreen, None),
        ]);
    } else {
        cx.bind_keys([KeyBinding::new("f11", ToggleFullscreen, None)]);
    }

    cx.set_menus(menus());
    // GPUI draws the same menu definitions inside Windows/Linux windows;
    // macOS uses the system menu bar installed above.
    GlobalState::global_mut(cx).set_app_menus(menus().into_iter().map(Menu::owned).collect());
    cx.on_window_closed(|cx, id| {
        crate::window_state::save(cx);
        let desktop = cx.global_mut::<Desktop>();
        if desktop
            .window
            .is_some_and(|window| window.window_id() == id)
        {
            desktop.window = None;
            desktop.workspace = None;
        }
        if !cfg!(target_os = "macos") && cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    activate(cx);
}

pub fn activate(cx: &mut App) {
    if !cx.has_global::<Desktop>() {
        return;
    }
    if let Some(window) = cx.global::<Desktop>().window
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return;
    }
    let env = cx.global::<Desktop>().env.clone();
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("T3 Code".into()),
            ..Default::default()
        }),
        window_bounds: Some(crate::window_state::initial(cx)),
        window_min_size: Some(size(px(640.), px(420.))),
        app_id: Some("t3code".into()),
        #[cfg(target_os = "linux")]
        icon: Some(crate::assets::window_icon()),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        #[cfg(target_os = "linux")]
        window.on_next_frame(|window, cx| crate::window_state::restore_position(window, cx));
        let appearance = crate::preferences::global(cx).read(cx).value.appearance;
        crate::preferences::apply_appearance(appearance, Some(window), cx);
        let workspace = cx.new(|cx| Workspace::new(env, window, cx));
        workspace.read(cx).focus_handle(cx).focus(window, cx);
        workspace
    }) {
        Ok((window, workspace)) => {
            let desktop = cx.global_mut::<Desktop>();
            desktop.window = Some(window);
            desktop.workspace = Some(workspace);
        }
        Err(error) => {
            tracing::error!("opening the desktop window failed: {error:#}");
            cx.quit();
        }
    }
    cx.activate(true);
}

pub fn about(window: &mut Window, cx: &mut App) {
    let prompt = window.prompt(
        PromptLevel::Info,
        "T3 Code",
        Some(concat!("Version ", env!("CARGO_PKG_VERSION"))),
        &["OK"],
        cx,
    );
    cx.spawn(async move |_| {
        let _ = prompt.await;
    })
    .detach();
}

fn menus() -> Vec<Menu> {
    let mut menus = Vec::new();
    if cfg!(target_os = "macos") {
        menus.push(Menu::new("T3 Code").items([
            MenuItem::action("About T3 Code", About),
            MenuItem::action("Settings...", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide T3 Code", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit T3 Code", Quit),
        ]));
    }
    let mut file_items = vec![MenuItem::action("New Thread", NewThread)];
    if !cfg!(target_os = "macos") {
        file_items.extend([
            MenuItem::action("Settings...", OpenSettings),
            MenuItem::separator(),
        ]);
    }
    file_items.push(if cfg!(target_os = "macos") {
        MenuItem::action("Close Window", CloseWindow)
    } else {
        MenuItem::action("Quit", Quit)
    });
    menus.extend([
        Menu::new("File").items(file_items),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::action("Paste as Text", PasteAsText),
            MenuItem::action("Delete", Delete),
            MenuItem::separator(),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find in Thread", FindInThread),
        ]),
        Menu::new("View").items([
            MenuItem::action("Command Palette", ToggleCommandPalette),
            MenuItem::separator(),
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Changes", ToggleChanges),
            MenuItem::separator(),
            MenuItem::action("Actual Size", ResetZoom),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::separator(),
            MenuItem::action("Toggle Full Screen", ToggleFullscreen),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Maximize),
        ]),
    ]);
    if !cfg!(target_os = "macos") {
        menus.push(Menu::new("Help").items([MenuItem::action("About T3 Code", About)]));
    }
    menus
}

pub fn is_active(cx: &mut App) -> bool {
    let window = cx.global::<Desktop>().window;
    window
        .and_then(|handle| {
            handle
                .update(cx, |_, window, _| window.is_window_active())
                .ok()
        })
        .unwrap_or(false)
}

pub fn open_notification_thread(id: String, cx: &mut App) {
    activate(cx);
    let desktop = cx.global::<Desktop>();
    let (Some(window), Some(workspace)) = (desktop.window, desktop.workspace.clone()) else {
        return;
    };
    if window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_notification_thread(id, window, cx)
            });
        })
        .is_err()
    {
        tracing::warn!("could not open the notification thread: window closed");
    }
}
