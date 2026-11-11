//! Where the title bar's command center is, so Quick Open can open over it,
//! like VS Code's.

use collections::{HashMap, HashSet};
use gpui::{
    AnyElement, App, Bounds, Global, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, Pixels, Styled as _, Window, WindowId, div, px,
};

#[derive(Default)]
struct CommandCenterBounds(HashMap<WindowId, Bounds<Pixels>>);

impl Global for CommandCenterBounds {}

/// Records where the command center was drawn in this window, or that it
/// isn't shown.
pub fn set_command_center_bounds(bounds: Option<Bounds<Pixels>>, window: &Window, cx: &mut App) {
    let window_id = window.window_handle().window_id();
    let all = &mut cx.default_global::<CommandCenterBounds>().0;
    match bounds {
        Some(bounds) => all.insert(window_id, bounds),
        None => all.remove(&window_id),
    };
}

pub fn command_center_bounds(window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
    cx.try_global::<CommandCenterBounds>()?
        .0
        .get(&window.window_handle().window_id())
        .copied()
}

/// Windows where Quick Open is open: their command center hides, as if it had
/// become Quick Open, like VS Code's.
#[derive(Default)]
struct QuickOpenShown(HashSet<WindowId>);

impl Global for QuickOpenShown {}

pub fn set_quick_open_shown(shown: bool, window: &Window, cx: &mut App) {
    let window_id = window.window_handle().window_id();
    let windows = &mut cx.default_global::<QuickOpenShown>().0;
    if shown {
        windows.insert(window_id);
    } else {
        windows.remove(&window_id);
    }
}

pub fn is_quick_open_shown(window: &Window, cx: &App) -> bool {
    cx.try_global::<QuickOpenShown>()
        .is_some_and(|shown| shown.0.contains(&window.window_handle().window_id()))
}

/// The width of a modal opened over the command center: the command center's,
/// but never too narrow to read.
pub fn overlay_width(window: &Window, cx: &App) -> Pixels {
    const DEFAULT_WIDTH: f32 = 600.;
    const MIN_WIDTH: f32 = 360.;
    let width = command_center_bounds(window, cx)
        .map_or(px(DEFAULT_WIDTH), |bounds| bounds.size.width)
        .max(px(MIN_WIDTH));
    width.min(window.viewport_size().width - px(16.))
}

/// Draws a modal (one whose `ModalView::render_bare` is true) over the command
/// center and as wide as it, like VS Code's quick input, and calls `dismiss`
/// on a click elsewhere.
pub fn render_over_command_center(
    content: AnyElement,
    dismiss: impl Fn(&mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let viewport = window.viewport_size();
    let width = overlay_width(window, cx);
    let (left, top) = match command_center_bounds(window, cx) {
        Some(bounds) => {
            let left = (bounds.center().x - width / 2.)
                .max(px(8.))
                .min(viewport.width - width - px(8.));
            (left, bounds.top())
        }
        None => ((viewport.width - width) / 2., px(8.)),
    };
    div()
        .absolute()
        .inset_0()
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| dismiss(window, cx))
        .child(
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(width)
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(content),
        )
        .into_any_element()
}
