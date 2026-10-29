//! Shared thread menus and dialogs for the sidebar and open conversation.

use std::collections::HashSet;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use t3_client::thread_actions::{Action, menu_actions};

use crate::environment::LocalEnvironment;
use crate::tokio_bridge;

struct Dialog {
    thread_id: String,
    title: String,
    action: Action,
}

pub struct ThreadActions {
    env: Entity<LocalEnvironment>,
    busy: HashSet<String>,
    dialog: Option<Dialog>,
    input: Entity<InputState>,
    focus: FocusHandle,
    return_focus: FocusHandle,
    focus_pending: bool,
    pub error: Option<SharedString>,
    _input: Subscription,
}

impl ThreadActions {
    pub fn new(
        env: Entity<LocalEnvironment>,
        return_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_placeholder("Thread title", window, cx);
            input
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. })
                    && this
                        .dialog
                        .as_ref()
                        .is_some_and(|dialog| matches!(dialog.action, Action::Rename(_)))
                {
                    this.submit(window, cx);
                }
            });
        Self {
            env,
            busy: HashSet::new(),
            dialog: None,
            input,
            focus: cx.focus_handle(),
            return_focus,
            focus_pending: false,
            error: None,
            _input: subscription,
        }
    }

    pub fn has_dialog(&self) -> bool {
        self.dialog.is_some()
    }

    pub fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .dialog
            .as_ref()
            .is_some_and(|dialog| self.busy.contains(&dialog.thread_id))
        {
            return;
        }
        self.dialog = None;
        self.error = None;
        self.return_focus.focus(window, cx);
        cx.notify();
    }

    fn request(
        &mut self,
        thread_id: String,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.contains(&thread_id) || self.dialog.is_some() {
            return;
        }
        let env = self.env.read(cx);
        let Some(thread) = env.thread(&thread_id) else {
            return;
        };
        let preferences = crate::preferences::global(cx);
        let preferences = &preferences.read(cx).value;
        let confirm = match action {
            Action::Rename(_) => true,
            Action::Archive => preferences.confirm_thread_archive,
            Action::Delete => preferences.confirm_thread_delete,
            _ => false,
        };
        if confirm {
            let title = thread.title.clone();
            if matches!(action, Action::Rename(_)) {
                self.input
                    .update(cx, |input, cx| input.set_value(title.clone(), window, cx));
            }
            self.dialog = Some(Dialog {
                thread_id,
                title,
                action,
            });
            self.error = None;
            self.focus_pending = true;
            cx.notify();
        } else {
            self.perform(thread_id, action, window, cx);
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.dialog else {
            return;
        };
        let action = match &dialog.action {
            Action::Rename(_) => {
                let title = self.input.read(cx).value().trim().to_owned();
                if title.is_empty() {
                    self.error = Some("Enter a thread title.".into());
                    cx.notify();
                    return;
                }
                Action::Rename(title)
            }
            action => action.clone(),
        };
        self.perform(dialog.thread_id.clone(), action, window, cx);
    }

    fn perform(
        &mut self,
        thread_id: String,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.contains(&thread_id) {
            return;
        }
        let env = self.env.read(cx);
        let Some(rpc) = env.rpc.clone() else {
            self.error = Some("Environment unavailable".into());
            cx.notify();
            return;
        };
        let Some(thread) = env.thread(&thread_id) else {
            self.close(window, cx);
            return;
        };
        let allowed =
            menu_actions(thread, env.thread_actions)
                .iter()
                .any(|(candidate, enabled)| {
                    *enabled && std::mem::discriminant(candidate) == std::mem::discriminant(&action)
                });
        if !allowed {
            self.error = Some("This action is no longer available for this thread.".into());
            cx.notify();
            return;
        }
        self.busy.insert(thread_id.clone());
        self.error = None;
        let command = action.command(&thread_id);
        let operation = tokio_bridge::spawn(cx, async move {
            t3_client::commands::dispatch(&rpc, &command).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match operation.await {
                Ok(result) => result.map_err(|error| error.detail()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy.remove(&thread_id);
                match result {
                    Ok(_) => {
                        if this
                            .dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.thread_id == thread_id)
                        {
                            this.close(window, cx);
                        }
                    }
                    Err(error) => {
                        this.error = Some(format!("{} failed: {error}", action.label()).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

pub fn menu(
    mut menu: PopupMenu,
    actions: &Entity<ThreadActions>,
    thread_id: &str,
    cx: &mut App,
) -> PopupMenu {
    let state = actions.read(cx);
    let env = state.env.read(cx);
    let Some(thread) = env.thread(thread_id) else {
        return menu;
    };
    let disabled = env.rpc.is_none() || state.busy.contains(thread_id) || state.dialog.is_some();
    let copy_id = thread.id.clone();
    menu = menu.item(
        PopupMenuItem::new("Copy thread ID").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_id.clone()));
        }),
    );
    for (action, enabled) in menu_actions(thread, env.thread_actions) {
        let target = actions.downgrade();
        let id = thread_id.to_owned();
        menu = menu.item(
            PopupMenuItem::new(action.label())
                .disabled(disabled || !enabled)
                .on_click(move |_, window, cx| {
                    let _ = target.update(cx, |state, cx| {
                        state.request(id.clone(), action.clone(), window, cx)
                    });
                }),
        );
    }
    menu
}

pub fn button(actions: &Entity<ThreadActions>, thread_id: &str) -> AnyElement {
    let actions = actions.clone();
    let id = thread_id.to_owned();
    Button::new("thread-actions")
        .accessibility_label("Thread actions")
        .tooltip("Thread actions")
        .icon(Icon::new(Lucide::Ellipsis))
        .ghost()
        .xsmall()
        .dropdown_menu(move |popup, _, cx| menu(popup, &actions, &id, cx))
        .into_any_element()
}

impl Render for ThreadActions {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(dialog) = &self.dialog else {
            return div().into_any_element();
        };
        let renaming = matches!(dialog.action, Action::Rename(_));
        let deleting = dialog.action == Action::Delete;
        let busy = self.busy.contains(&dialog.thread_id);
        if self.focus_pending {
            self.focus_pending = false;
            let view = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                let _ = view.update(cx, |this, cx| {
                    if let Some(dialog) = &this.dialog {
                        if matches!(dialog.action, Action::Rename(_)) {
                            this.input.read(cx).focus_handle(cx).focus(window, cx);
                        } else {
                            this.focus.focus(window, cx);
                        }
                    }
                });
            });
        }
        let warning = if deleting {
            "This permanently clears conversation history for this thread. Files in the project are kept."
        } else {
            "This thread will move to the archive. You can restore it later."
        };
        let confirm_label = if renaming {
            "Save"
        } else if deleting {
            "Delete thread"
        } else {
            "Archive thread"
        };
        v_flex()
            .id("thread-action-dialog")
            .track_focus(&self.focus)
            .role(if renaming {
                Role::Dialog
            } else {
                Role::AlertDialog
            })
            .aria_label(dialog.action.label())
            .w_full()
            .max_w(px(560.))
            .p_4()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(div().text_sm().font_medium().child(dialog.action.label()))
            .child(div().text_sm().child(dialog.title.clone()))
            .when(renaming, |column| {
                column.child(Input::new(&self.input).disabled(busy))
            })
            .when(!renaming, |column| {
                column.child(div().text_sm().child(warning))
            })
            .when_some(self.error.clone(), |column, error| {
                column.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel-thread-action")
                            .label("Cancel")
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        Button::new("confirm-thread-action")
                            .label(confirm_label)
                            .small()
                            .disabled(busy)
                            .when(deleting, |button| button.danger())
                            .when(!deleting, |button| button.primary())
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .focus_trap("thread-action", &self.focus)
            .into_any_element()
    }
}
