//! The notification center: a bell in the status bar that opens a list of
//! notifications closed without being answered, so they can still be answered
//! or discarded.

use gpui::{
    Anchor, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, Global, IntoElement, ParentElement, Render, Styled, Subscription, WeakEntity,
    Window,
};
use ui::{PopoverMenu, Tooltip, prelude::*};
use util::ResultExt as _;

use crate::{
    ItemHandle, StatusItemView, Workspace,
    notifications::{NotificationId, dismiss_app_notification},
    status_bar::HideStatusItem,
};

const DO_NOT_DISTURB_KEY: &str = "notification_center_do_not_disturb";

/// "Do Not Disturb": notifications go straight to the notification center
/// instead of showing over the workspace. Shared by all windows and kept
/// across restarts.
#[derive(Default)]
struct DoNotDisturb(bool);

impl Global for DoNotDisturb {}

pub fn init(cx: &mut App) {
    let enabled = db::kvp::KeyValueStore::global(cx)
        .read_kvp(DO_NOT_DISTURB_KEY)
        .log_err()
        .flatten()
        .is_some_and(|value| value == "true");
    cx.set_global(DoNotDisturb(enabled));
}

pub fn do_not_disturb(cx: &App) -> bool {
    cx.try_global::<DoNotDisturb>()
        .is_some_and(|do_not_disturb| do_not_disturb.0)
}

pub fn set_do_not_disturb(enabled: bool, cx: &mut App) {
    cx.set_global(DoNotDisturb(enabled));
    let kvp = db::kvp::KeyValueStore::global(cx);
    cx.background_spawn(async move {
        kvp.write_kvp(DO_NOT_DISTURB_KEY.to_string(), enabled.to_string())
            .await
            .log_err();
    })
    .detach();
}

pub struct NotificationCenterButton {
    workspace: WeakEntity<Workspace>,
    _subscriptions: [Subscription; 2],
}

impl NotificationCenterButton {
    pub fn new(workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace: workspace.downgrade(),
            _subscriptions: [
                cx.observe(workspace, |_, _, cx| cx.notify()),
                // Each window's button clears its own workspace, so turning
                // the mode on in one window clears the others too.
                cx.observe_global::<DoNotDisturb>(|this, cx| {
                    if do_not_disturb(cx) {
                        this.workspace
                            .update(cx, |workspace, cx| workspace.close_all_notifications(cx))
                            .ok();
                    }
                    cx.notify();
                }),
            ],
        }
    }
}

impl Render for NotificationCenterButton {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self
            .workspace
            .read_with(cx, |workspace, _| workspace.closed_notifications().len())
            .unwrap_or(0);
        let do_not_disturb = do_not_disturb(cx);
        let tooltip = match count {
            0 => "No Notifications".to_string(),
            1 => "1 Notification".to_string(),
            count => format!("{count} Notifications"),
        };
        let tooltip = if do_not_disturb {
            format!("{tooltip} (Do Not Disturb)")
        } else {
            tooltip
        };
        let workspace = self.workspace.clone();
        PopoverMenu::new("notification-center")
            .menu(move |window, cx| {
                let workspace = workspace.clone();
                Some(cx.new(|cx| NotificationCenter::new(workspace, window, cx)))
            })
            .anchor(Anchor::BottomRight)
            .trigger_with_tooltip(
                IconButton::new(
                    "notification-center-button",
                    match (do_not_disturb, count > 0) {
                        (true, true) => IconName::NotificationBellOffDot,
                        (true, false) => IconName::NotificationBellOff,
                        (false, true) => IconName::NotificationBellDot,
                        (false, false) => IconName::NotificationBell,
                    },
                )
                .icon_size(IconSize::Small),
                Tooltip::text(tooltip),
            )
    }
}

impl StatusItemView for NotificationCenterButton {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
        None
    }
}

struct NotificationCenter {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl NotificationCenter {
    fn new(workspace: WeakEntity<Workspace>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![cx.observe_global::<DoNotDisturb>(|_, cx| cx.notify())];
        subscriptions.extend(
            workspace
                .upgrade()
                .map(|workspace| cx.observe(&workspace, |_, _, cx| cx.notify())),
        );
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn closed_ids(&self, cx: &App) -> Vec<NotificationId> {
        self.workspace
            .read_with(cx, |workspace, _| {
                workspace
                    .closed_notifications()
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl EventEmitter<DismissEvent> for NotificationCenter {}

impl Focusable for NotificationCenter {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NotificationCenter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notifications = self
            .workspace
            .read_with(cx, |workspace, _| {
                workspace
                    .closed_notifications()
                    .iter()
                    .rev()
                    .map(|(_, view)| view.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let is_empty = notifications.is_empty();
        let do_not_disturb = do_not_disturb(cx);

        v_flex()
            .key_context("NotificationCenter")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .w_112()
            .max_h(vh(0.7, window))
            .elevation_2(cx)
            .child(
                h_flex()
                    .pl_3()
                    .pr_2()
                    .py_1()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Label::new("Notifications")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                IconButton::new(
                                    "toggle-do-not-disturb",
                                    if do_not_disturb {
                                        IconName::NotificationsMuted
                                    } else {
                                        IconName::Notifications
                                    },
                                )
                                .icon_size(IconSize::Small)
                                .toggle_state(do_not_disturb)
                                .tooltip(Tooltip::text(if do_not_disturb {
                                    "Turn Off Do Not Disturb"
                                } else {
                                    "Do Not Disturb"
                                }))
                                .on_click(move |_, _, cx| {
                                    set_do_not_disturb(!do_not_disturb, cx)
                                }),
                            ),
                    )
                    .child(
                        IconButton::new("clear-all-notifications", IconName::ListX)
                            .icon_size(IconSize::Small)
                            .disabled(is_empty)
                            .tooltip(Tooltip::text("Clear All Notifications"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                for id in this.closed_ids(cx) {
                                    dismiss_app_notification(&id, cx);
                                }
                                cx.emit(DismissEvent);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("notification-center-list")
                    // Lets the list shrink to the panel's height and scroll
                    // instead of growing past it.
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(is_empty, |list| {
                        list.child(
                            div().px_3().py_2().child(
                                Label::new("No new notifications")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                        )
                    })
                    .children(notifications.into_iter().enumerate().map(
                        |(index, notification)| {
                            div()
                                .when(index > 0, |row| {
                                    row.border_t_1()
                                        .border_color(cx.theme().colors().border_variant)
                                })
                                .child(notification)
                        },
                    )),
            )
    }
}
