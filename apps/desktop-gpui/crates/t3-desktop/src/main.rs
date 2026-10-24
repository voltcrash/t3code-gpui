mod assets;
mod composer;
mod environment;
mod format;
mod launch;
mod sidebar;
mod thread_view;
mod tokio_bridge;
mod workspace;

use gpui_kit::component::Theme;
use gpui_kit::*;
use tracing_subscriber::EnvFilter;

use crate::environment::LocalEnvironment;
use crate::tokio_bridge::Tokio;
use crate::workspace::Workspace;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let launch = launch::development()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("t3-io")
        .enable_all()
        .build()?;

    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.set_global(Tokio(runtime));
            Theme::sync_system_appearance(None, cx);

            let env = cx.new(|cx| LocalEnvironment::start(launch, cx));
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("T3 Code".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
                window_min_size: Some(size(px(640.), px(420.))),
                app_id: Some("t3code".into()),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Workspace::new(env, window, cx))
            })
            .expect("failed to open the main window");
            cx.activate(true);
        });
    Ok(())
}
