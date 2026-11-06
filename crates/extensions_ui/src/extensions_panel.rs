use anyhow::Result;
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Pixels, Render, Styled, WeakEntity, Window, actions, px,
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
}

impl Settings for ExtensionsPanelSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let panel = content.extensions_panel.as_ref().unwrap();
        Self {
            button: panel.button.unwrap(),
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
}

impl ExtensionsPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let page = ExtensionsPage::new(workspace, None, None, window, cx);
            page.update(cx, |page, cx| page.set_compact(true, cx));
            cx.new(|_| Self {
                page,
                position: DockPosition::Left,
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

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        8
    }
}
