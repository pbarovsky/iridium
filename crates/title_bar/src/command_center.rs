//! The search box in the middle of the title bar, like VS Code's command
//! center: it opens Quick Open, which opens right over it, as wide as it.
//! Optionally with back and forward buttons
//! (`title_bar.show_command_center_navigation`).
//!
//! The box is centered on the window, not on the space between the title
//! bar's parts. It shrinks to keep a gap from the menus on the left and the
//! buttons on the right, moves into the space between them when centering
//! leaves too little room, and hides when even that's too narrow.

use std::{cell::Cell, rc::Rc};

use gpui::{
    App, Bounds, IntoElement, MouseButton, ParentElement, Pixels, Styled, WeakEntity, Window,
    canvas,
};
use settings::Settings as _;
use ui::{Tooltip, prelude::*};
use workspace::Workspace;
use zed_actions::quick_open::Toggle;

use crate::title_bar_settings::TitleBarSettings;

const MAX_WIDTH: f32 = 600.;
/// Below this, the box leaves the center for the space between the parts.
const MIN_CENTERED_WIDTH: f32 = 200.;
/// Below this, the box hides.
const MIN_WIDTH: f32 = 120.;
const GAP: f32 = 16.;
const BOX_HEIGHT: f32 = 22.;
const NAVIGATION_WIDTH: f32 = 48.;

/// Where the title bar's left part ends and its right part begins, measured
/// while painting, for laying out the command center between them.
#[derive(Clone, Copy, Default)]
pub(crate) struct Measurements {
    /// Where the title bar's content starts, which the command center is
    /// positioned relative to.
    origin: Pixels,
    left_end: Pixels,
    right_start: Pixels,
}

#[derive(Clone, Default)]
pub(crate) struct CommandCenterLayout(Rc<Cell<Option<Measurements>>>);

impl CommandCenterLayout {
    /// An invisible element that measures the title bar part it's in.
    pub(crate) fn measure(&self, left: bool) -> impl IntoElement {
        let layout = self.0.clone();
        canvas(
            move |bounds: Bounds<Pixels>, window, _| {
                let mut measurements = layout.get().unwrap_or_default();
                if left {
                    measurements.origin = bounds.left();
                    measurements.left_end = bounds.right();
                } else {
                    measurements.right_start = bounds.left();
                }
                let changed = layout.get().is_none_or(|old| {
                    old.origin != measurements.origin
                        || old.left_end != measurements.left_end
                        || old.right_start != measurements.right_start
                });
                layout.set(Some(measurements));
                if changed {
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    /// Where the command center goes, relative to the title bar's content,
    /// and how wide it is.
    fn place(&self, window: &Window) -> Option<(Pixels, Pixels)> {
        let measurements = self.0.get()?;
        let gap = px(GAP);
        let center = window.viewport_size().width / 2.;
        let half = (center - measurements.left_end).min(measurements.right_start - center) - gap;
        let (left, width) = if half * 2. >= px(MIN_CENTERED_WIDTH) {
            let width = (half * 2.).min(px(MAX_WIDTH));
            (center - width / 2., width)
        } else {
            let space = measurements.right_start - measurements.left_end - gap * 2.;
            if space < px(MIN_WIDTH) {
                return None;
            }
            let width = space.min(px(MAX_WIDTH));
            (measurements.left_end + gap + (space - width) / 2., width)
        };
        Some((left - measurements.origin, width))
    }
}

pub(crate) fn render_command_center(
    workspace: &WeakEntity<Workspace>,
    layout: &CommandCenterLayout,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let settings = TitleBarSettings::get_global(cx);
    let show_navigation = settings.show_command_center_navigation;
    let placement = layout.place(window);
    let (workspace_entity, (left, width)) = match (
        workspace.upgrade().filter(|_| settings.show_command_center),
        placement,
    ) {
        (Some(workspace), Some(placement)) => (workspace, placement),
        _ => {
            workspace::quick_open::set_command_center_bounds(None, window, cx);
            return None;
        }
    };
    let project_name: SharedString = workspace_entity
        .read(cx)
        .project()
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|worktree| worktree.read(cx).root_name_str().to_string().into())
        .unwrap_or_else(|| "Search".into());
    let colors = cx.theme().colors();

    let navigate = |id: &'static str, icon: IconName, tooltip: &'static str, back: bool| {
        let workspace = workspace.clone();
        IconButton::new(id, icon)
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .tooltip(Tooltip::text(tooltip))
            .on_click(move |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        let pane = workspace.active_pane().downgrade();
                        let task = if back {
                            workspace.go_back(pane, window, cx)
                        } else {
                            workspace.go_forward(pane, window, cx)
                        };
                        task.detach_and_log_err(cx);
                    })
                    .ok();
            })
    };

    // Quick Open opens over the box, which keeps its place but isn't drawn.
    let quick_open_shown = workspace::quick_open::is_quick_open_shown(window, cx);
    let search_box = h_flex()
        .id("command-center")
        .when(quick_open_shown, |this| this.invisible())
        .relative()
        .flex_1()
        .min_w_0()
        .h(px(BOX_HEIGHT))
        .px_2()
        .gap_1p5()
        .justify_center()
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(colors.border_variant)
        .bg(colors.element_background)
        .hover(|style| style.bg(colors.element_hover))
        .cursor_pointer()
        // Remembers where the box is, so Quick Open can open over it.
        .child(
            canvas(
                |bounds, window, cx| {
                    workspace::quick_open::set_command_center_bounds(Some(bounds), window, cx)
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(
            Icon::new(IconName::MagnifyingGlass)
                .size(IconSize::XSmall)
                .color(Color::Muted),
        )
        .child(
            Label::new(project_name)
                .size(LabelSize::Small)
                .color(Color::Muted)
                .truncate(),
        )
        .tooltip(|_, cx| {
            Tooltip::for_action("Search Files, Commands and Symbols", &Toggle::default(), cx)
        })
        // Clicking the box shouldn't start moving the window.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, window, cx| window.dispatch_action(Box::new(Toggle::default()), cx));

    let navigation_width = if show_navigation {
        px(NAVIGATION_WIDTH)
    } else {
        px(0.)
    };
    Some(
        h_flex()
            .absolute()
            .top_0()
            .bottom_0()
            .items_center()
            .left(left - navigation_width)
            .w(width + navigation_width)
            .gap_0p5()
            .when(show_navigation, |this| {
                this.child(
                    h_flex()
                        .flex_none()
                        .w(px(NAVIGATION_WIDTH))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(navigate(
                            "command-center-back",
                            IconName::ArrowLeft,
                            "Go Back",
                            true,
                        ))
                        .child(navigate(
                            "command-center-forward",
                            IconName::ArrowRight,
                            "Go Forward",
                            false,
                        )),
                )
            })
            .child(search_box)
            .into_any_element(),
    )
}
