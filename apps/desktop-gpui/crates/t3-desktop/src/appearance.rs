//! The original desktop's T3 Code palette and geometry. Color values mirror
//! `packages/shared/src/themePalettes.ts`; layout values come from web/index.css
//! and components/threadSidebarWidth.ts.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme, Colorize, Theme};
use gpui_kit::*;

pub const SIDEBAR_WIDTH: Pixels = px(256.);
pub const TOPBAR_HEIGHT: Pixels = px(52.);
pub const CONTENT_WIDTH: Pixels = px(736.);

pub struct Palette(BTreeMap<String, Hsla>);
impl Palette {
    pub fn color(&self, role: &str) -> Hsla {
        self.0[role]
    }
}
struct Palettes {
    light: Rc<Palette>,
    dark: Rc<Palette>,
}
impl Global for Palettes {}

pub fn init(cx: &mut App) {
    let mut palettes: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_str(include_str!("../../../assets/t3-code-colors.json"))
            .expect("bundled T3 Code palettes are valid");
    let mut load = |mode: &str| {
        Rc::new(Palette(
            palettes
                .remove(mode)
                .expect("bundled appearance exists")
                .into_iter()
                .map(|(role, value)| {
                    (
                        role,
                        Hsla::parse_hex(&value).expect("bundled color is valid"),
                    )
                })
                .collect(),
        ))
    };
    cx.set_global(Palettes {
        light: load("light"),
        dark: load("dark"),
    });
}

pub fn palette(cx: &App) -> &Palette {
    let palettes = cx.global::<Palettes>();
    if cx.theme().is_dark() {
        &palettes.dark
    } else {
        &palettes.light
    }
}

pub fn apply(cx: &mut App) {
    let palettes = cx.global::<Palettes>();
    let palette = if cx.theme().is_dark() {
        palettes.dark.clone()
    } else {
        palettes.light.clone()
    };
    Theme::update(cx, |theme| {
        let colors = &mut theme.colors;
        let c = |role| palette.color(role);
        colors.background = c("canvas");
        colors.foreground = c("text");
        colors.border = c("border");
        colors.input = c("input");
        colors.caret = c("text");
        colors.accent = c("accentSurface");
        colors.accent_foreground = c("accentSurfaceForeground");
        colors.primary = c("messageAction");
        colors.primary_hover = c("messageActionHover");
        colors.primary_active = c("messageActionHover");
        colors.primary_foreground = c("messageActionForeground");
        colors.secondary = c("secondary");
        colors.secondary_foreground = c("secondaryForeground");
        colors.secondary_hover = c("accentSurface");
        colors.secondary_active = c("accentSurface");
        colors.muted = c("muted");
        colors.muted_foreground = c("mutedForeground");
        colors.popover = c("surfaceOverlay");
        colors.popover_foreground = c("text");
        colors.button = c("toolbarControl");
        colors.button_foreground = c("toolbarControlForeground");
        colors.button_hover = c("toolbarControlHover");
        colors.button_active = c("accentSurface");
        colors.button_primary = colors.primary;
        colors.button_primary_foreground = colors.primary_foreground;
        colors.button_primary_hover = colors.primary_hover;
        colors.button_primary_active = colors.primary_active;
        colors.button_secondary = colors.secondary;
        colors.button_secondary_foreground = colors.secondary_foreground;
        colors.button_secondary_hover = colors.secondary_hover;
        colors.button_secondary_active = colors.secondary_active;
        colors.ring = c("focus");
        colors.link = c("accent");
        colors.link_hover = c("accent");
        colors.link_active = c("accent");
        colors.danger = c("error");
        colors.danger_foreground = c("errorForeground");
        colors.warning = c("warning");
        colors.warning_foreground = c("warningForeground");
        colors.sidebar = c("sidebar");
        colors.sidebar_foreground = c("sidebarForeground");
        colors.sidebar_border = c("sidebarBorder");
        colors.sidebar_accent = c("sidebarRowActive");
        colors.sidebar_accent_foreground = c("sidebarForeground");
        colors.sidebar_primary = c("accent");
        colors.sidebar_primary_foreground = c("accentForeground");
        colors.list = c("surfaceOverlay");
        colors.list_hover = c("accentSurface");
        colors.list_active = c("accentSurface");
        colors.list_active_border = c("focus");
        colors.tab = c("canvas");
        colors.tab_active = c("surface");
        colors.tab_active_foreground = c("text");
        colors.tab_foreground = c("textMuted");
        colors.tab_bar = c("chrome");
        colors.title_bar = c("chrome");
        colors.title_bar_border = c("toolbarBorder");
        colors.status_bar = c("chrome");
        colors.status_bar_border = c("border");
        colors.scrollbar_thumb = c("terminalScrollbar");
        colors.scrollbar_thumb_hover = c("terminalScrollbarHover");
        colors.switch = c("input");
        theme.radius = px(8.);
        theme.radius_lg = px(10.);
    });
}
