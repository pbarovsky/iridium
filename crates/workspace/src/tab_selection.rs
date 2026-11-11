//! Selecting several tabs in a pane, like VS Code: shift-click selects the tabs
//! from the last plainly clicked one to the clicked one, the secondary modifier
//! (ctrl, or cmd on macOS) adds or removes a tab, and closing any selected tab
//! closes all of them.

use collections::HashSet;
use gpui::{App, ClickEvent, Context, EntityId, Task, Window};

use crate::{Pane, SaveIntent};

#[derive(Default)]
pub struct TabSelection {
    /// The tab a shift-click selects from: the last one clicked without a
    /// modifier.
    anchor: Option<EntityId>,
    selected: HashSet<EntityId>,
}

impl Pane {
    /// Updates the tab selection for a click on the tab at `ix`, before the
    /// tab is activated.
    pub(crate) fn select_tab_for_click(&mut self, ix: usize, event: &ClickEvent, cx: &App) {
        let Some(item_id) = self.item_for_index(ix).map(|item| item.item_id()) else {
            return;
        };
        let modifiers = event.modifiers();
        if modifiers.shift {
            let anchor_ix = self
                .tab_selection
                .anchor
                .and_then(|anchor| self.items().position(|item| item.item_id() == anchor))
                .unwrap_or(self.active_item_index());
            let (start, end) = if anchor_ix <= ix {
                (anchor_ix, ix)
            } else {
                (ix, anchor_ix)
            };
            self.tab_selection.selected = self
                .items()
                .skip(start)
                .take(end - start + 1)
                .map(|item| item.item_id())
                .collect();
        } else if modifiers.secondary(cx) {
            // The tab that's already open is part of the selection, as in VS Code.
            if self.tab_selection.selected.is_empty()
                && let Some(active) = self.active_item()
            {
                self.tab_selection.selected.insert(active.item_id());
            }
            if !self.tab_selection.selected.remove(&item_id) {
                self.tab_selection.selected.insert(item_id);
            }
        } else {
            self.tab_selection.selected.clear();
            self.tab_selection.anchor = Some(item_id);
        }
    }

    /// Whether the tab for `item_id` is one of several selected tabs.
    pub(crate) fn is_tab_multi_selected(&self, item_id: EntityId) -> bool {
        self.selected_tab_count() > 1 && self.tab_selection.selected.contains(&item_id)
    }

    fn selected_tab_count(&self) -> usize {
        self.items()
            .filter(|item| self.tab_selection.selected.contains(&item.item_id()))
            .count()
    }

    /// Closes the tab for `item_id`, or every selected tab if it's one of them.
    pub(crate) fn close_tab_or_selection(
        &mut self,
        item_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        if !self.is_tab_multi_selected(item_id) {
            return self.close_item_by_id(item_id, SaveIntent::Close, window, cx);
        }
        let selected = std::mem::take(&mut self.tab_selection.selected);
        self.close_items(window, cx, SaveIntent::Close, &move |item_id| {
            selected.contains(&item_id)
        })
    }
}
