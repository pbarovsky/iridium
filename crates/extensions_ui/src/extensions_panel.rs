use anyhow::Result;
use extension_host::ExtensionStore;
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Pixels, Render, Styled, Subscription, WeakEntity, Window, actions,
    px,
};
use settings::{RegisterSetting, Settings};
use ui::{IconName, prelude::*};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::ExtensionsPage;

actions!(
    extensions_panel,
    [
        /// Toggles focus on the extensions panel.
        ToggleFocus,
    ]
);

const EXTENSIONS_PANEL_KEY: &str = "ExtensionsPanel";

#[derive(Debug, Clone, Copy, PartialEq, RegisterSetting)]
pub struct ExtensionsPanelSettings {
    pub button: bool,
    pub show_count_badge: bool,
}

impl Settings for ExtensionsPanelSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let panel = content.extensions_panel.as_ref().unwrap();
        Self {
            button: panel.button.unwrap(),
            show_count_badge: panel.show_count_badge.unwrap(),
        }
    }
}

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<ExtensionsPanel>(window, cx);
        });
    })
    .detach();
}

/// Shows the extensions page in a dock, like the extensions view in VS Code,
/// instead of in a tab.
pub struct ExtensionsPanel {
    page: Entity<ExtensionsPage>,
    position: DockPosition,
    _store_subscription: Subscription,
}

impl ExtensionsPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let page = ExtensionsPage::new(workspace, None, None, window, cx);
            page.update(cx, |page, cx| {
                page.set_compact(true, cx);
                page.defer_catalog_fetch();
            });
            cx.new(|cx| Self {
                page,
                position: DockPosition::Left,
                // Keeps the update count on the panel's button current.
                _store_subscription: cx
                    .observe(&ExtensionStore::global(cx), |_, _, cx| cx.notify()),
            })
        })
    }

    pub fn page(&self) -> &Entity<ExtensionsPage> {
        &self.page
    }
}

impl EventEmitter<PanelEvent> for ExtensionsPanel {}

impl Focusable for ExtensionsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.page.focus_handle(cx)
    }
}

impl Render for ExtensionsPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex().size_full().child(self.page.clone())
    }
}

impl Panel for ExtensionsPanel {
    fn persistent_name() -> &'static str {
        "Extensions Panel"
    }

    fn panel_key() -> &'static str {
        EXTENSIONS_PANEL_KEY
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(320.)
    }

    fn icon(&self, _: &Window, cx: &App) -> Option<IconName> {
        ExtensionsPanelSettings::get_global(cx)
            .button
            .then_some(IconName::ActivityExtensions)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Extensions")
    }

    fn icon_label(&self, _: &Window, cx: &App) -> Option<String> {
        if !ExtensionsPanelSettings::get_global(cx).show_count_badge {
            return None;
        }
        let count = ExtensionStore::global(cx).read(cx).available_update_count();
        (count > 0).then(|| count.to_string())
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        8
    }

    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.page
                .update(cx, |page, cx| page.fetch_deferred_catalog(cx));
            ExtensionStore::global(cx).update(cx, |store, cx| store.refresh_available_updates(cx));
        }
    }
}
