//! The web client draws Lucide icons too; this adds the ones the default
//! component bundle leaves out.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Pin,
        AlarmClock,
        GitBranch,
        ArrowUp,
        Square,
        SquarePen,
        FolderPlus,
        Trash,
        X,
        Ellipsis,
        Settings,
        Paperclip,
        FolderCode,
        ListTodo,
        CircleDot,
        Circle,
        PanelLeft,
        FileDiff,
        ZoomIn,
        ZoomOut,
        Maximize,
        ArrowDown,
        MessageSquare
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "t3-wordmark.svg" {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../../../assets/t3-wordmark.svg"
            ))));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.push("t3-wordmark.svg".into());
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(target_os = "linux")]
pub fn window_icon() -> std::sync::Arc<image::RgbaImage> {
    let bytes: &[u8] = if cfg!(debug_assertions) {
        include_bytes!("../../../../../assets/dev/blueprint-universal-1024.png")
    } else {
        include_bytes!("../../../../../assets/prod/black-universal-1024.png")
    };
    // _NET_WM_ICON is sent over X11; a full 1024px source would add 4 MiB
    // to window creation for an icon that taskbars display at a fraction of that.
    std::sync::Arc::new(
        image::load_from_memory(bytes)
            .expect("bundled app icon is a valid PNG")
            .thumbnail(256, 256)
            .into_rgba8(),
    )
}
