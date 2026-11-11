//! Where the title bar's command center is, so Quick Open can open over it,
//! like VS Code's.

use collections::{HashMap, HashSet};
use gpui::{App, Bounds, Global, Pixels, Window, WindowId};

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
