use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Disableable, Icon, Sizable, h_flex};
use gpui_kit::*;
use t3_client::editors::Catalog;

use crate::preferences;
use crate::workspace::OpenInEditor;

pub fn ready(catalog: &Catalog, cx: &App) -> bool {
    let model = preferences::global(cx);
    let model = model.read(cx);
    model.loaded
        && !model.saving
        && catalog
            .preferred(model.value.last_editor.as_deref())
            .is_some()
}

pub fn picker(catalog: &Catalog, connected: bool, busy: bool, cx: &App) -> AnyElement {
    let model = preferences::global(cx);
    let preferred = catalog.preferred(model.read(cx).value.last_editor.as_deref());
    let disabled = !connected || busy || !ready(catalog, cx);
    let tooltip = preferred.map_or_else(
        || "No installed editors found".to_owned(),
        |editor| format!("Open in {} (Cmd/Ctrl+O)", editor.label),
    );
    let options = catalog.available.clone();
    h_flex()
        .child(
            Button::new("open-in-editor")
                .label("Open")
                .icon(Icon::new(IconName::SquareArrowOutUpRight))
                .ghost()
                .small()
                .disabled(disabled)
                .tooltip(tooltip)
                .accessibility_label("Open in editor")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenInEditor), cx)),
        )
        .child(
            Button::new("choose-editor")
                .icon(Icon::new(IconName::ChevronDown))
                .ghost()
                .small()
                .disabled(disabled)
                .tooltip("Choose editor")
                .accessibility_label("Choose editor")
                .dropdown_menu(move |mut menu, _, _| {
                    for editor in &options {
                        let editor = *editor;
                        let model = model.clone();
                        menu = menu.item(
                            PopupMenuItem::new(editor.label)
                                .checked(preferred == Some(editor))
                                .on_click(move |_, window, cx| {
                                    model.update(cx, |model, cx| {
                                        let mut value = model.value.clone();
                                        value.last_editor = Some(editor.id.into());
                                        model.change(value, window, cx);
                                    });
                                }),
                        );
                    }
                    menu
                }),
        )
        .into_any_element()
}
