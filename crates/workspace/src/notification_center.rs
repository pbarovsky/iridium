//! The notification center: a bell in the status bar that opens a list of
//! notifications closed without being answered, so they can still be answered
//! or discarded.

use gpui::{
    Anchor, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Styled, Subscription, WeakEntity, Window,
};
use ui::{PopoverMenu, Tooltip, prelude::*};

use crate::{
    ItemHandle, StatusItemView, Workspace,
    notifications::{NotificationId, dismiss_app_notification},
    status_bar::HideStatusItem,
};

pub struct NotificationCenterButton {
    workspace: WeakEntity<Workspace>,
    _workspace_subscription: Subscription,
}

impl NotificationCenterButton {
    pub fn new(workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace: workspace.downgrade(),
            _workspace_subscription: cx.observe(workspace, |_, _, cx| cx.notify()),
        }
    }
}

impl Render for NotificationCenterButton {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self
            .workspace
            .read_with(cx, |workspace, _| workspace.closed_notifications().len())
            .unwrap_or(0);
        let tooltip = match count {
            0 => "No Notifications".to_string(),
            1 => "1 Notification".to_string(),
            count => format!("{count} Notifications"),
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
                    if count > 0 {
                        IconName::NotificationBellDot
                    } else {
                        IconName::NotificationBell
                    },
                )
                .icon_size(IconSize::Small)
                .when(count > 0, |button| button.icon_color(Color::Accent)),
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
    _workspace_subscription: Option<Subscription>,
}

impl NotificationCenter {
    fn new(workspace: WeakEntity<Workspace>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = workspace
            .upgrade()
            .map(|workspace| cx.observe(&workspace, |_, _, cx| cx.notify()));
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
            _workspace_subscription: subscription,
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
                        Label::new("Notifications")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
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
