mod assets;
mod composer;
mod desktop;
mod draft_view;
mod environment;
mod format;
mod launch;
mod project_picker;
mod sidebar;
mod thread_view;
mod tokio_bridge;
mod workspace;

use gpui_kit::component::Theme;
use tracing_subscriber::EnvFilter;

use crate::tokio_bridge::Tokio;

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

    let app = gpui_kit::application().with_assets(assets::AppAssets);
    app.on_reopen(desktop::activate);
    app.run(move |cx| {
        gpui_kit::init(cx);
        cx.set_global(Tokio(runtime));
        workspace::bind_keys(cx);
        Theme::sync_system_appearance(None, cx);

        desktop::init(launch, cx);
    });
    Ok(())
}
