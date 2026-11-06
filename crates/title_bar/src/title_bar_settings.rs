use gpui::WindowButtonLayout;
use settings::{RegisterSetting, Settings, SettingsContent};

#[derive(Copy, Clone, Debug, RegisterSetting)]
pub struct TitleBarSettings {
    pub show_onboarding_banner: bool,
    pub show_menus: bool,
    pub open_menus_on_hover: bool,
    pub button_layout: Option<WindowButtonLayout>,
}

impl Settings for TitleBarSettings {
    fn from_settings(s: &SettingsContent) -> Self {
        let content = s.title_bar.clone().unwrap();
        TitleBarSettings {
            show_onboarding_banner: content.show_onboarding_banner.unwrap(),
            show_menus: content.show_menus.unwrap(),
            open_menus_on_hover: content.open_menus_on_hover.unwrap(),
            button_layout: content.button_layout.unwrap_or_default().into_layout(),
        }
    }
}
