use crate::dock::{Dock, PanelHandle, activate_panel_button, panel_button_context_menu};
use crate::{ActivityBarSettings, DeploySearch, UseAgenticLayout, UseClassicLayout, Workspace};
use agent_settings::{AgentSettings, WindowLayout};
use gpui::{
    Action, Anchor, AnyElement, App, Context, Entity, FocusHandle, IntoElement, ParentElement,
    Pixels, Render, SharedString, Styled, Subscription, Task, WeakEntity, Window, px,
};
use settings::{ActivityBarPosition, Settings as _, SettingsStore};
use std::sync::Arc;
use ui::{
    ButtonSize, ContextMenu, ContextMenuEntry, CountBadge, Icon, IconButton, IconName, IconSize,
    PopoverMenu, Tab, Tooltip, prelude::*, right_click_menu,
};
use util::ResultExt as _;

const DEFAULT_VERTICAL_ICON_SIZE: Pixels = px(20.);
const HORIZONTAL_ICON_SIZE: Pixels = px(16.);

/// Entries are shown in this order by `Panel::panel_key()` until the user drags
/// them around. Panels not listed come after, in dock order (left dock first).
const DEFAULT_ORDER: [&str; 8] = [
    "ProjectPanel",
    "SearchPanel",
    "GitPanel",
    "DebugPanel",
    "ExtensionsPanel",
    "OutlinePanel",
    "agent_panel",
    "TerminalPanel",
];

const ORDER_KEY: &str = "activity_bar_order";

/// The VS Code (Codicon) style icon for a panel. They are all drawn on the same
/// 16px pixel grid, so their strokes look equally thick. Other panels keep
/// their own icon.
fn activity_icon(panel_key: &str) -> Option<IconName> {
    match panel_key {
        "ProjectPanel" => Some(IconName::ActivityExplorer),
        "SearchPanel" => Some(IconName::ActivitySearch),
        "GitPanel" => Some(IconName::ActivitySourceControl),
        "DebugPanel" => Some(IconName::ActivityDebug),
        "ExtensionsPanel" => Some(IconName::ActivityExtensions),
        "OutlinePanel" => Some(IconName::ActivityOutline),
        "agent_panel" => Some(IconName::ActivityChat),
        "TerminalPanel" => Some(IconName::ActivityTerminal),
        _ => None,
    }
}

/// The action whose key binding a panel's tooltip shows, when the panel is
/// usually opened with a different action than the one its button dispatches.
fn shortcut_action(panel_key: &str) -> Option<Box<dyn Action>> {
    match panel_key {
        "SearchPanel" => Some(DeploySearch::default().boxed_clone()),
        "ExtensionsPanel" => Some(zed_actions::Extensions::default().boxed_clone()),
        _ => None,
    }
}

/// Where the bar is drawn, if it is shown. The top bar lives in the left dock,
/// so it is hidden together with that dock.
pub(crate) fn effective_position(cx: &App) -> Option<ActivityBarPosition> {
    let settings = ActivityBarSettings::get_global(cx);
    settings.show.then_some(settings.position)
}

/// Where context menus open relative to the button they belong to, so they
/// stay inside the window whichever edge the bar is on.
fn menu_anchors(at_start: bool, position: ActivityBarPosition) -> (Anchor, Anchor) {
    match position {
        ActivityBarPosition::Top => {
            if at_start {
                (Anchor::TopLeft, Anchor::BottomLeft)
            } else {
                (Anchor::TopRight, Anchor::BottomRight)
            }
        }
        ActivityBarPosition::Left => {
            if at_start {
                (Anchor::TopLeft, Anchor::TopRight)
            } else {
                (Anchor::BottomLeft, Anchor::BottomRight)
            }
        }
        ActivityBarPosition::Right => {
            if at_start {
                (Anchor::TopRight, Anchor::TopLeft)
            } else {
                (Anchor::BottomRight, Anchor::BottomLeft)
            }
        }
    }
}

/// The top bar matches the tab bar, so `icon_size` only applies to the
/// vertical bar.
fn icon_pixels(position: ActivityBarPosition, cx: &App) -> Pixels {
    if position == ActivityBarPosition::Top {
        return HORIZONTAL_ICON_SIZE;
    }
    ActivityBarSettings::get_global(cx)
        .icon_size
        .map_or(DEFAULT_VERTICAL_ICON_SIZE, px)
}

fn icon_size(position: ActivityBarPosition, window: &Window, cx: &App) -> IconSize {
    IconSize::Custom(rems(
        f32::from(icon_pixels(position, cx)) / f32::from(window.rem_size()),
    ))
}

fn button_extent(position: ActivityBarPosition, cx: &App) -> Pixels {
    if position == ActivityBarPosition::Top {
        // With the frame's 1px border the button is exactly as tall as a tab.
        return Tab::chip_height() - px(2.);
    }
    icon_pixels(position, cx) + px(8.)
}

/// Width of the vertical bar, so dock resizing can account for it.
pub(crate) fn bar_width(cx: &App) -> Pixels {
    button_extent(ActivityBarPosition::Left, cx) + px(10.)
}

/// A bar with one button per left or right dock panel, like the activity bar in
/// VS Code. It runs down the left or right edge of the window, or across the top
/// of the left dock.
pub struct ActivityBar {
    workspace: WeakEntity<Workspace>,
    left_dock: Entity<Dock>,
    right_dock: Entity<Dock>,
    order: Vec<String>,
    _serialization: Task<()>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
pub struct DraggedActivityBarEntry {
    key: &'static str,
    icon: IconName,
    position: ActivityBarPosition,
}

impl Render for DraggedActivityBarEntry {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_1()
            .rounded_md()
            .bg(cx.theme().colors().elevated_surface_background)
            .child(Icon::new(self.icon).size(icon_size(self.position, window, cx)))
    }
}

pub struct ActivityBarEntry {
    dock: Entity<Dock>,
    panel_index: usize,
    panel: Arc<dyn PanelHandle>,
    icon: IconName,
    icon_tooltip: &'static str,
}

impl ActivityBarEntry {
    pub fn key(&self) -> &'static str {
        self.panel.panel_key()
    }

    /// The action the entry's button dispatches, its tooltip, whether the button is
    /// shown as active, and the focus handle to focus before dispatching.
    fn button_state(
        &self,
        window: &Window,
        cx: &App,
    ) -> (Box<dyn Action>, SharedString, bool, FocusHandle) {
        let dock = self.dock.read(cx);
        let (action, tooltip, is_active) =
            dock.panel_button_action(self.panel_index, self.icon_tooltip, window, cx);
        (
            action,
            tooltip,
            is_active,
            dock.panel_button_focus_handle(window, cx),
        )
    }
}

impl ActivityBar {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        left_dock: Entity<Dock>,
        right_dock: Entity<Dock>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&left_dock, |_, _, cx| cx.notify()),
            cx.observe(&right_dock, |_, _, cx| cx.notify()),
            cx.observe_global::<SettingsStore>(|_, cx| cx.notify()),
        ];
        let kvp = db::kvp::KeyValueStore::global(cx);
        cx.spawn(async move |this, cx| {
            let stored_order = cx
                .background_spawn(async move { kvp.read_kvp(ORDER_KEY) })
                .await
                .log_err()
                .flatten()
                .and_then(|json| serde_json::from_str::<Vec<String>>(&json).log_err());
            if let Some(order) = stored_order {
                this.update(cx, |this, cx| {
                    this.order = order;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        Self {
            workspace,
            left_dock,
            right_dock,
            order: DEFAULT_ORDER.iter().map(|key| key.to_string()).collect(),
            _serialization: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    /// Moves the dragged entry to the slot of the entry it was dropped on and
    /// remembers the resulting order across restarts.
    pub fn move_entry(
        &mut self,
        dragged_key: &str,
        target_key: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if dragged_key == target_key {
            return;
        }
        let mut keys: Vec<String> = self
            .entry_keys(window, cx)
            .into_iter()
            .map(str::to_string)
            .collect();
        let (Some(from), Some(to)) = (
            keys.iter().position(|key| key == dragged_key),
            keys.iter().position(|key| key == target_key),
        ) else {
            return;
        };
        let dragged = keys.remove(from);
        keys.insert(to, dragged);
        self.order = keys;
        let json = serde_json::to_string(&self.order).log_err();
        let kvp = db::kvp::KeyValueStore::global(cx);
        self._serialization = cx.background_spawn(async move {
            if let Some(json) = json {
                kvp.write_kvp(ORDER_KEY.to_string(), json).await.log_err();
            }
        });
        cx.notify();
    }

    fn position(&self, cx: &App) -> ActivityBarPosition {
        effective_position(cx).unwrap_or(ActivityBarPosition::Left)
    }

    pub fn entries(&self, window: &Window, cx: &App) -> Vec<ActivityBarEntry> {
        let mut entries = Vec::new();
        for dock in [&self.left_dock, &self.right_dock] {
            for (panel_index, panel) in dock.read(cx).panels().enumerate() {
                let Some(panel_icon) = panel.icon(window, cx) else {
                    continue;
                };
                let Some(icon_tooltip) = panel
                    .icon_tooltip(window, cx)
                    .ok_or_else(|| {
                        anyhow::anyhow!("can't render an activity bar button without a tooltip")
                    })
                    .log_err()
                else {
                    continue;
                };
                entries.push(ActivityBarEntry {
                    dock: dock.clone(),
                    panel_index,
                    panel: panel.clone(),
                    icon: activity_icon(panel.panel_key()).unwrap_or(panel_icon),
                    icon_tooltip,
                });
            }
        }
        entries.sort_by_key(|entry| {
            self.order
                .iter()
                .position(|key| key == entry.key())
                .unwrap_or(self.order.len())
        });
        entries
    }

    pub fn entry_keys(&self, window: &Window, cx: &App) -> Vec<&'static str> {
        self.entries(window, cx)
            .iter()
            .map(ActivityBarEntry::key)
            .collect()
    }

    /// Does what clicking the entry's button does. Returns false when there is no
    /// entry with the given key.
    pub fn activate_entry(&self, key: &str, window: &mut Window, cx: &mut App) -> bool {
        let entries = self.entries(window, cx);
        let Some(entry) = entries.iter().find(|entry| entry.key() == key) else {
            return false;
        };
        let (action, _, _, focus_handle) = entry.button_state(window, cx);
        activate_panel_button(&focus_handle, &*action, window, cx);
        true
    }

    fn render_entry(
        &self,
        entry: ActivityBarEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = entry.key();
        let (action, tooltip, is_active, focus_handle) = entry.button_state(window, cx);
        let icon = entry.icon;
        let icon_tooltip = entry.icon_tooltip;
        let badge_count = entry
            .panel
            .icon_label(window, cx)
            .and_then(|label| label.parse::<usize>().ok());
        let bar_position = self.position(cx);
        let horizontal = bar_position == ActivityBarPosition::Top;

        let icon_size = icon_size(bar_position, window, cx);
        let extent = button_extent(bar_position, cx);
        let colors = cx.theme().colors();
        // Like VS Code's modern activity bar: the active entry gets a bordered,
        // filled frame and an accent-colored icon instead of an edge marker.
        // Inactive entries keep a transparent border so nothing shifts.
        let (frame_background, frame_border) = if is_active {
            (colors.ghost_element_hover, colors.border_variant)
        } else {
            (gpui::transparent_black(), gpui::transparent_black())
        };
        let button = move |is_menu_open: bool| {
            let action = action.boxed_clone();
            let focus_handle = focus_handle.clone();
            let tooltip = tooltip.clone();
            // Include active state in element ID to invalidate the cached
            // tooltip when panel state changes (e.g., via keyboard shortcut)
            let button = IconButton::new((key, is_active as u64), icon)
                .size(ButtonSize::Large)
                .width(extent)
                .height(extent.into())
                .icon_size(icon_size)
                .icon_color(if is_active {
                    Color::Accent
                } else {
                    Color::Muted
                })
                .tab_index(0isize)
                .aria_label(icon_tooltip)
                .on_click({
                    let action = action.boxed_clone();
                    let focus_handle = focus_handle.clone();
                    move |_, window, cx| activate_panel_button(&focus_handle, &*action, window, cx)
                })
                .when(!is_menu_open, |this| {
                    let shortcut = (!is_active)
                        .then(|| shortcut_action(key))
                        .flatten()
                        .unwrap_or_else(|| action.boxed_clone());
                    // Look the binding up from the dock rather than the focused
                    // element, where e.g. the editor rebinds the project panel's key.
                    this.tooltip(move |_window, cx| {
                        Tooltip::for_action_in(tooltip.clone(), &*shortcut, &focus_handle, cx)
                    })
                });
            div()
                .rounded(px(6.))
                .border_1()
                .border_color(frame_border)
                .bg(frame_background)
                .child(button)
        };

        let dock = entry.dock;
        let panel = entry.panel;
        let workspace = self.workspace.clone();
        let position = self
            .order
            .iter()
            .position(|ordered| ordered == key)
            .unwrap_or(self.order.len());
        let drop_indicator_before = move |dragged: &DraggedActivityBarEntry, order: &[String]| {
            order
                .iter()
                .position(|ordered| ordered == dragged.key)
                .is_none_or(|dragged_position| position < dragged_position)
        };
        let order = self.order.clone();
        let (menu_anchor, menu_attach) = menu_anchors(true, bar_position);
        let on_drop = cx.listener(move |this, dragged: &DraggedActivityBarEntry, window, cx| {
            this.move_entry(dragged.key, key, window, cx);
        });
        right_click_menu(format!("activity-bar-{key}"))
            .menu(move |window, cx| {
                panel_button_context_menu(&panel, &dock, &workspace, window, cx)
            })
            .anchor(menu_anchor)
            .attach(menu_attach)
            .trigger(move |is_menu_open, _window, _cx| {
                div()
                    .id(SharedString::from(format!("activity-bar-entry-{key}")))
                    .relative()
                    .on_drag(
                        DraggedActivityBarEntry {
                            key,
                            icon,
                            position: bar_position,
                        },
                        |entry, _, _, cx| cx.new(|_| entry.clone()),
                    )
                    .drag_over::<DraggedActivityBarEntry>(move |style, dragged, _, cx| {
                        let colors = cx.theme().colors();
                        let style = style
                            .bg(colors.drop_target_background)
                            .border_color(colors.drop_target_border);
                        match (horizontal, drop_indicator_before(dragged, &order)) {
                            (true, true) => style.border_l_2(),
                            (true, false) => style.border_r_2(),
                            (false, true) => style.border_t_2(),
                            (false, false) => style.border_b_2(),
                        }
                    })
                    .on_drop(on_drop)
                    .child(button(is_menu_open))
                    .when_some(badge_count, |this, count| {
                        this.child(CountBadge::new(count))
                    })
            })
            .into_any_element()
    }

    fn render_settings_menu(&self, window: &Window, cx: &App) -> impl IntoElement {
        let position = self.position(cx);
        let (menu_anchor, menu_attach) = menu_anchors(false, position);
        PopoverMenu::new("activity-bar-settings-menu")
            .trigger_with_tooltip(
                IconButton::new("activity-bar-settings", IconName::ActivitySettings)
                    .size(ButtonSize::Large)
                    .width(button_extent(position, cx))
                    .height(button_extent(position, cx).into())
                    .icon_size(icon_size(position, window, cx))
                    .icon_color(Color::Muted)
                    .tab_index(0isize)
                    .aria_label("Manage"),
                Tooltip::text("Manage"),
            )
            .menu(|window, cx| {
                let ai_enabled = !project::DisableAiSettings::get_global(cx).disable_ai;
                let current_layout = AgentSettings::get_layout(cx);
                let is_editor = matches!(current_layout, WindowLayout::Editor(_));
                let is_agent = matches!(current_layout, WindowLayout::Agent(_));
                let is_custom = matches!(current_layout, WindowLayout::Custom(_));
                ContextMenu::build(window, cx, |menu, _, _| {
                    menu.action("Settings", zed_actions::OpenSettings.boxed_clone())
                        .action("Keymap", zed_actions::OpenKeymap.boxed_clone())
                        .action(
                            "Themes…",
                            zed_actions::theme_selector::Toggle::default().boxed_clone(),
                        )
                        .action(
                            "Icon Themes…",
                            zed_actions::icon_theme_selector::Toggle::default().boxed_clone(),
                        )
                        .action(
                            "Extensions",
                            zed_actions::Extensions::default().boxed_clone(),
                        )
                        .when(ai_enabled, |menu| {
                            menu.separator()
                                .submenu("Panel Layout", move |menu, _window, _cx| {
                                    menu.toggleable_entry(
                                        "Classic",
                                        is_editor,
                                        IconPosition::Start,
                                        Some(UseClassicLayout.boxed_clone()),
                                        |window, cx| {
                                            window.dispatch_action(
                                                UseClassicLayout.boxed_clone(),
                                                cx,
                                            );
                                        },
                                    )
                                    .toggleable_entry(
                                        "Agentic",
                                        is_agent,
                                        IconPosition::Start,
                                        Some(UseAgenticLayout.boxed_clone()),
                                        |window, cx| {
                                            window.dispatch_action(
                                                UseAgenticLayout.boxed_clone(),
                                                cx,
                                            );
                                        },
                                    )
                                    .when(is_custom, |menu| {
                                        menu.item(
                                            ContextMenuEntry::new("Custom")
                                                .toggleable(IconPosition::Start, true)
                                                .disabled(true),
                                        )
                                    })
                                })
                        })
                })
                .into()
            })
            .anchor(menu_anchor)
            .attach(menu_attach)
    }
}

impl Render for ActivityBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let buttons: Vec<_> = self
            .entries(window, cx)
            .into_iter()
            .map(|entry| self.render_entry(entry, window, cx))
            .collect();
        let colors = cx.theme().colors();

        let position = self.position(cx);
        if position == ActivityBarPosition::Top {
            // Same height and padding as the tab bar next to it, so the
            // active frame has the same room around it as the active tab.
            return h_flex()
                .id("activity-bar")
                .flex_none()
                .w_full()
                .h(Tab::container_height(cx))
                .items_center()
                .px(DynamicSpacing::Base06.rems(cx))
                .gap_0p5()
                .border_b_1()
                .border_color(colors.border)
                .bg(colors.panel_background)
                .children(buttons)
                .child(div().flex_1())
                .child(self.render_settings_menu(window, cx))
                .into_any_element();
        }

        let bar = v_flex()
            .id("activity-bar")
            .flex_none()
            .w(bar_width(cx))
            .h_full()
            .items_center()
            .gap_1()
            .py_1()
            .children(buttons)
            .child(div().flex_1())
            .child(self.render_settings_menu(window, cx));

        bar.bg(colors.status_bar_background)
            .border_color(colors.border)
            .map(|bar| {
                if position == ActivityBarPosition::Right {
                    bar.border_l_1()
                } else {
                    bar.border_r_1()
                }
            })
            .into_any_element()
    }
}
