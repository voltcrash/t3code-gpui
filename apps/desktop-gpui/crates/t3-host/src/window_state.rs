//! Native window placement is local desktop state, never a server setting.

use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rectangle {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rectangle {
    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f32::is_finite)
            && self.width >= 640.
            && self.height >= 420.
    }

    fn fits(self, display: Self) -> bool {
        self.x >= display.x
            && self.y >= display.y
            && self.x + self.width <= display.x + display.width
            && self.y + self.height <= display.y + display.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    pub zoom: f32,
    pub sidebar_visible: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: 1.,
            sidebar_visible: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub bounds: Rectangle,
    pub maximized: bool,
    #[serde(default)]
    pub view: View,
}

impl Placement {
    pub fn on_displays(self, displays: &[Rectangle]) -> Option<Self> {
        (self.bounds.valid() && displays.iter().any(|display| self.bounds.fits(*display)))
            .then_some(self)
    }
}

pub struct Store(PathBuf);

impl Store {
    pub fn new(t3_home: &Path) -> Self {
        Self(t3_home.join("userdata/gpui-window-state.json"))
    }

    pub fn load(&self) -> io::Result<Option<Placement>> {
        match fs::read(&self.0) {
            Ok(bytes) => {
                let mut placement: Option<Placement> =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if let Some(placement) = &mut placement
                    && (!placement.view.zoom.is_finite()
                        || !(0.5..=3.).contains(&placement.view.zoom))
                {
                    placement.view.zoom = 1.;
                }
                Ok(placement)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, placement: Placement) -> io::Result<()> {
        if !placement.bounds.valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid window bounds",
            ));
        }
        crate::preferences::save_json(&self.0, &placement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn restores_normal_bounds_and_maximization_on_a_connected_display() {
        let placement = Placement {
            bounds: rect(-1800., 30., 1280., 820.),
            maximized: true,
            view: View::default(),
        };
        let displays = [rect(0., 0., 1920., 1080.), rect(-1920., 0., 1920., 1080.)];
        assert_eq!(placement.on_displays(&displays), Some(placement));
        assert_eq!(placement.on_displays(&displays[..1]), None);
        assert_eq!(placement.on_displays(&[]), None);
    }

    #[test]
    fn rejects_invalid_or_partly_offscreen_placement() {
        let displays = [rect(0., 0., 1920., 1080.)];
        for bounds in [
            rect(1800., 0., 1280., 820.),
            rect(0., -1., 1280., 820.),
            rect(0., 0., 0., 820.),
            rect(f32::NAN, 0., 1280., 820.),
            rect(0., 0., f32::INFINITY, 820.),
        ] {
            assert_eq!(
                Placement {
                    bounds,
                    maximized: false,
                    view: View::default(),
                }
                .on_displays(&displays),
                None
            );
        }
    }

    #[test]
    fn workspace_view_survives_restarts_and_legacy_files_keep_defaults() {
        let root = std::env::temp_dir().join(format!(
            "t3-view-state-{}",
            super::super::preferences::nonce().unwrap()
        ));
        let store = Store::new(&root);
        let mut placement: Placement = serde_json::from_value(serde_json::json!({
            "bounds": {"x":20,"y":30,"width":1280,"height":820}, "maximized":false
        }))
        .unwrap();
        assert_eq!(placement.view.zoom, 1.);
        assert!(placement.view.sidebar_visible);
        placement.view = View {
            zoom: 1.44,
            sidebar_visible: false,
        };
        store.save(placement).unwrap();
        assert_eq!(
            Store::new(&root).load().unwrap().unwrap().view,
            placement.view
        );
        for zoom in [0., -1., 100.] {
            let mut value = serde_json::to_value(placement).unwrap();
            value["view"]["zoom"] = serde_json::json!(zoom);
            fs::write(&store.0, serde_json::to_vec(&value).unwrap()).unwrap();
            let restored = store.load().unwrap().unwrap();
            assert_eq!(restored.view.zoom, 1.);
            assert!(!restored.view.sidebar_visible);
            assert_eq!(restored.bounds, placement.bounds);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persists_across_restart_without_touching_other_desktop_or_server_settings() {
        let root = std::env::temp_dir().join(format!(
            "t3-window-state-{}",
            super::super::preferences::nonce().unwrap()
        ));
        let store = Store::new(&root);
        assert_eq!(store.load().unwrap(), None);
        assert!(!root.exists());
        let placement = Placement {
            bounds: rect(20., 30., 1280., 820.),
            maximized: true,
            view: View::default(),
        };
        store.save(placement).unwrap();
        assert_eq!(Store::new(&root).load().unwrap(), Some(placement));
        assert_eq!(fs::read_dir(root.join("userdata")).unwrap().count(), 1);
        fs::write(&store.0, "broken").unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read_to_string(&store.0).unwrap(), "broken");
        fs::remove_dir_all(root).unwrap();
    }
}
