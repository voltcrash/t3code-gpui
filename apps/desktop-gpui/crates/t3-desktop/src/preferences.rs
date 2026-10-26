//! Native preference state. Commit the file before announcing a change, so a
//! failed save never leaves controls claiming a setting that was not saved.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use std::io;
use std::sync::Arc;
use t3_host::preferences::{Appearance, Preferences, Store};

pub struct Model {
    pub value: Preferences,
    pub loaded: bool,
    pub saving: bool,
    pub error: Option<SharedString>,
    store: Arc<Store>,
}

struct ClientPreferences(Entity<Model>);
impl Global for ClientPreferences {}

pub fn global(cx: &App) -> Entity<Model> {
    cx.global::<ClientPreferences>().0.clone()
}

pub fn init(store: Store, loaded: io::Result<Preferences>, cx: &mut App) {
    let (value, ready, error) = match loaded {
        Ok(value) => (value, true, None),
        Err(error) => (
            Preferences::default(),
            false,
            Some(format!("Unable to load preferences: {error}").into()),
        ),
    };
    apply_appearance(value.appearance, None, cx);
    let model = cx.new(|_| Model {
        value,
        loaded: ready,
        saving: false,
        error,
        store: Arc::new(store),
    });
    cx.set_global(ClientPreferences(model));
}

pub fn apply_appearance(appearance: Appearance, window: Option<&mut Window>, cx: &mut App) {
    match appearance {
        Appearance::System => Theme::sync_system_appearance(window, cx),
        Appearance::Light => Theme::change(ThemeMode::Light, window, cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, window, cx),
    }
    crate::appearance::apply(cx);
}

impl Model {
    pub fn change(&mut self, value: Preferences, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !self.loaded || value == self.value {
            return;
        }
        self.saving = true;
        self.error = None;
        let store = self.store.clone();
        let save = cx
            .background_executor()
            .spawn(async move { store.save(&value).map(|()| value) });
        let target_window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = save.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(value) => {
                        let appearance = value.appearance;
                        if target_window
                            .update(cx, |_, window, cx| {
                                apply_appearance(appearance, Some(window), cx)
                            })
                            .is_err()
                        {
                            apply_appearance(appearance, None, cx);
                        }
                        this.value = value;
                    }
                    Err(error) => {
                        this.error = Some(format!("Unable to save preferences: {error}").into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn retry_load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.saving = true;
        self.error = None;
        let store = self.store.clone();
        let load = cx.background_executor().spawn(async move { store.load() });
        let target_window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = load.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(value) => {
                        let appearance = value.appearance;
                        if target_window
                            .update(cx, |_, window, cx| {
                                apply_appearance(appearance, Some(window), cx)
                            })
                            .is_err()
                        {
                            apply_appearance(appearance, None, cx);
                        }
                        this.value = value;
                        this.loaded = true;
                    }
                    Err(error) => {
                        this.error = Some(format!("Unable to load preferences: {error}").into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
