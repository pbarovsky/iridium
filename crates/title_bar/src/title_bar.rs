mod application_menu;
pub mod collab;
mod onboarding_banner;
mod plan_chip;
mod project_status_item;
mod title_bar_settings;
mod update_version;

use crate::application_menu::ApplicationMenu;
use crate::plan_chip::PlanChip;
use agent_settings::{AgentSettings, WindowLayout};
use arrayvec::ArrayVec;
pub use platform_title_bar::{
    self, DraggedWindowTab, MergeAllWindows, MoveTabToNewWindow, PlatformTitleBar,
    ShowNextWindowTab, ShowPreviousWindowTab,
};

#[cfg(not(target_os = "macos"))]
use crate::application_menu::{
    ActivateDirection, ActivateMenuLeft, ActivateMenuRight, OpenApplicationMenu,
};

use auto_update::AutoUpdateStatus;
use call::ActiveCall;
use client::{Client, UserStore, zed_urls};
use command_palette_hooks::CommandPaletteFilter;

use gpui::{
    Action, Anchor, AnyElement, App, Context, Element, Entity, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, TaskExt,
    WeakEntity, Window, actions, div,
};
use onboarding_banner::OnboardingBanner;
use project::{Project, git_store::GitStoreEvent, trusted_worktrees::TrustedWorktrees};
use settings::{Settings as _, SettingsStore};

use std::any::TypeId;
use std::sync::Arc;
use title_bar_settings::TitleBarSettings;
use ui::{
    Avatar, ButtonLike, ContextMenu, ContextMenuEntry, Indicator, PopoverMenu, PopoverMenuHandle,
    TintColor, Tooltip, prelude::*,
};
use update_version::UpdateVersion;
use util::ResultExt;
use workspace::{
    MultiWorkspace, SidebarSide, ToggleBottomDock, ToggleLeftDock, ToggleWorkspaceSidebar,
    ToggleWorktreeSecurity, Workspace, notifications::NotifyTaskExt as _,
    sidebar_side_context_menu,
};

pub use onboarding_banner::restore_banner;
pub use project_status_item::ProjectStatusItem;

actions!(
    collab,
    [
        /// Toggles the user menu dropdown.
        ToggleUserMenu,
        /// Toggles the project menu dropdown.
        ToggleProjectMenu,
        /// Switches to a different git branch.
        SwitchBranch,
        /// A debug action to simulate an update being available to test the update banner UI.
        SimulateUpdateAvailable
    ]
);

actions!(
    workspace,
    [
        /// Switches to the classic, editor-focused panel layout.
        UseClassicLayout,
        /// Switches to the agentic panel layout.
        UseAgenticLayout,
    ]
);

pub fn init(cx: &mut App) {
    platform_title_bar::PlatformTitleBar::init(cx);

    update_layout_action_filter(cx);

    cx.observe_global::<SettingsStore>(update_layout_action_filter)
        .detach();

    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        let multi_workspace = workspace.multi_workspace().cloned();
        let item = cx.new(|cx| TitleBar::new("title-bar", workspace, multi_workspace, window, cx));
        workspace.set_titlebar_item(item.into(), window, cx);

        workspace.register_action(|_workspace, _: &UseClassicLayout, _window, cx| {
            set_window_layout(WindowLayout::Editor(None), cx);
        });

        workspace.register_action(|_workspace, _: &UseAgenticLayout, _window, cx| {
            set_window_layout(WindowLayout::Agent(None), cx);
        });

        workspace.register_action(|workspace, _: &SimulateUpdateAvailable, _window, cx| {
            if let Some(titlebar) = workspace
                .titlebar_item()
                .and_then(|item| item.downcast::<TitleBar>().ok())
            {
                titlebar.update(cx, |titlebar, cx| {
                    titlebar.toggle_update_simulation(cx);
                });
            }
        });

        #[cfg(not(target_os = "macos"))]
        workspace.register_action(|workspace, action: &OpenApplicationMenu, window, cx| {
            if let Some(titlebar) = workspace
                .titlebar_item()
                .and_then(|item| item.downcast::<TitleBar>().ok())
            {
                titlebar.update(cx, |titlebar, cx| {
                    if let Some(ref menu) = titlebar.application_menu {
                        menu.update(cx, |menu, cx| menu.open_menu(action, window, cx));
                    }
                });
            }
        });

        #[cfg(not(target_os = "macos"))]
        workspace.register_action(|workspace, _: &ActivateMenuRight, window, cx| {
            if let Some(titlebar) = workspace
                .titlebar_item()
                .and_then(|item| item.downcast::<TitleBar>().ok())
            {
                titlebar.update(cx, |titlebar, cx| {
                    if let Some(ref menu) = titlebar.application_menu {
                        menu.update(cx, |menu, cx| {
                            menu.navigate_menus_in_direction(ActivateDirection::Right, window, cx)
                        });
                    }
                });
            }
        });

        #[cfg(not(target_os = "macos"))]
        workspace.register_action(|workspace, _: &ActivateMenuLeft, window, cx| {
            if let Some(titlebar) = workspace
                .titlebar_item()
                .and_then(|item| item.downcast::<TitleBar>().ok())
            {
                titlebar.update(cx, |titlebar, cx| {
                    if let Some(ref menu) = titlebar.application_menu {
                        menu.update(cx, |menu, cx| {
                            menu.navigate_menus_in_direction(ActivateDirection::Left, window, cx)
                        });
                    }
                });
            }
        });
    })
    .detach();
}

/// Hides or shows the panel layout actions in the command palette based on
/// whether AI is currently disabled.
fn update_layout_action_filter(cx: &mut App) {
    let disable_ai = project::DisableAiSettings::get_global(cx).disable_ai;
    let layout_actions = [
        TypeId::of::<UseClassicLayout>(),
        TypeId::of::<UseAgenticLayout>(),
    ];
    CommandPaletteFilter::update_global(cx, |filter, _| {
        if disable_ai {
            filter.hide_action_types(&layout_actions);
        } else {
            filter.show_action_types(layout_actions.iter());
        }
    });
}

fn set_window_layout(layout: WindowLayout, cx: &App) {
    let fs = <dyn fs::Fs>::global(cx);
    drop(AgentSettings::set_layout(layout, fs, cx));
}

pub struct TitleBar {
    platform_titlebar: Entity<PlatformTitleBar>,
    project: Entity<Project>,
    user_store: Entity<UserStore>,
    client: Arc<Client>,
    workspace: WeakEntity<Workspace>,
    multi_workspace: Option<WeakEntity<MultiWorkspace>>,
    application_menu: Option<Entity<ApplicationMenu>>,
    _subscriptions: Vec<Subscription>,
    banner: Option<Entity<OnboardingBanner>>,
    update_version: Entity<UpdateVersion>,
    screen_share_popover_handle: PopoverMenuHandle<ContextMenu>,
    _diagnostics_subscription: Option<gpui::Subscription>,
}

impl Render for TitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.multi_workspace.is_none() {
            if let Some(mw) = self
                .workspace
                .upgrade()
                .and_then(|ws| ws.read(cx).multi_workspace().cloned())
            {
                self.multi_workspace = Some(mw.clone());
                if let Some(multi_workspace) = mw.upgrade() {
                    self._subscriptions
                        .push(cx.observe(&multi_workspace, |_, _, cx| cx.notify()));
                }
                self.platform_titlebar.update(cx, |titlebar, _cx| {
                    titlebar.set_multi_workspace(mw);
                });
            }
        }

        let title_bar_settings = *TitleBarSettings::get_global(cx);
        let button_layout = title_bar_settings.button_layout;

        let mut children = <ArrayVec<_, 5>>::new();

        // The project name, worktree and branch live in the status bar
        // (see `ProjectStatusItem`), as in VS Code.
        children.push(
            h_flex()
                .h_full()
                .gap_0p5()
                .when_some(self.application_menu.clone(), |title_bar, menu| {
                    title_bar.child(menu)
                })
                .children(self.render_restricted_mode(cx))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .into_any_element(),
        );

        children.push(self.render_collaborator_list(window, cx).into_any_element());

        if title_bar_settings.show_onboarding_banner {
            if let Some(banner) = &self.banner {
                children.push(banner.clone().into_any_element())
            }
        }

        let status = self.client.status();
        let status = &*status.borrow();

        children.push(
            h_flex()
                .pr_1()
                .gap_1()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(self.render_call_controls(window, cx))
                .children(self.render_connection_status(status, cx))
                .child(self.update_version.clone())
                .when(TitleBarSettings::get_global(cx).show_user_menu, |this| {
                    this.child(self.render_user_menu_button(cx))
                })
                .child(self.render_layout_toggles(cx))
                .into_any_element(),
        );

        // The menus share the row with the window controls instead of getting
        // a row of their own, so the title bar stays a single line.
        self.platform_titlebar.update(cx, |this, _| {
            this.set_button_layout(button_layout);
            this.set_children(children);
        });
        self.platform_titlebar.clone().into_any_element()
    }
}

impl TitleBar {
    /// Buttons that open and close the side bar (left dock, with the activity
    /// bar), the panel (bottom dock) and the threads sidebar, like the layout
    /// controls in VS Code's title bar.
    fn render_layout_toggles(&self, cx: &App) -> impl IntoElement {
        let (left_open, bottom_open) = self
            .workspace
            .upgrade()
            .map(|workspace| {
                let workspace = workspace.read(cx);
                (
                    workspace.left_dock().read(cx).is_open(),
                    workspace.bottom_dock().read(cx).is_open(),
                )
            })
            .unwrap_or_default();
        let threads_sidebar = self
            .multi_workspace
            .as_ref()
            .and_then(|multi_workspace| multi_workspace.upgrade())
            .and_then(|multi_workspace| {
                let multi_workspace = multi_workspace.read(cx);
                multi_workspace.multi_workspace_enabled(cx).then(|| {
                    (
                        multi_workspace.sidebar_open(),
                        multi_workspace.sidebar_side(cx),
                        multi_workspace.sidebar_has_notifications(cx),
                    )
                })
            });

        let toggle = |id: &'static str,
                      icon: IconName,
                      tooltip: &'static str,
                      action: &'static dyn gpui::Action| {
            IconButton::new(id, icon)
                .icon_size(IconSize::Small)
                .aria_label(tooltip)
                .tooltip(move |_, cx| Tooltip::for_action(tooltip, action, cx))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(action.boxed_clone(), cx);
                })
        };

        h_flex()
            .gap_0p5()
            .child(toggle(
                "toggle-primary-side-bar",
                if left_open {
                    IconName::LayoutSidebarLeft
                } else {
                    IconName::LayoutSidebarLeftOff
                },
                "Toggle Primary Side Bar",
                &ToggleLeftDock,
            ))
            .child(toggle(
                "toggle-panel",
                if bottom_open {
                    IconName::LayoutPanel
                } else {
                    IconName::LayoutPanelOff
                },
                "Toggle Panel",
                &ToggleBottomDock,
            ))
            .children(threads_sidebar.map(|(open, side, has_notifications)| {
                let icon = match (side, open) {
                    (SidebarSide::Right, true) => IconName::LayoutSidebarRight,
                    (SidebarSide::Right, false) => IconName::LayoutSidebarRightOff,
                    (SidebarSide::Left, true) => IconName::LayoutSidebarLeft,
                    (SidebarSide::Left, false) => IconName::LayoutSidebarLeftOff,
                };
                let indicator_border = cx.theme().colors().title_bar_background;
                sidebar_side_context_menu("title-bar-threads-sidebar-menu", cx)
                    .anchor(Anchor::TopRight)
                    .attach(Anchor::BottomRight)
                    .trigger(move |_is_active, _window, _cx| {
                        toggle(
                            "toggle-threads-sidebar",
                            icon,
                            "Toggle Threads Sidebar",
                            &ToggleWorkspaceSidebar,
                        )
                        .when(has_notifications && !open, |this| {
                            this.indicator(Indicator::dot().color(Color::Accent))
                                .indicator_border_color(Some(indicator_border))
                        })
                    })
            }))
    }

    pub fn new(
        id: impl Into<ElementId>,
        workspace: &Workspace,
        multi_workspace: Option<WeakEntity<MultiWorkspace>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = workspace.project().clone();
        let git_store = project.read(cx).git_store().clone();
        let user_store = workspace.app_state().user_store.clone();
        let client = workspace.app_state().client.clone();
        let active_call = ActiveCall::global(cx);

        let platform_style = PlatformStyle::platform();
        let application_menu = match platform_style {
            PlatformStyle::Mac => {
                if option_env!("ZED_USE_CROSS_PLATFORM_MENU").is_some() {
                    Some(cx.new(|cx| ApplicationMenu::new(window, cx)))
                } else {
                    None
                }
            }
            PlatformStyle::Linux | PlatformStyle::Windows => {
                Some(cx.new(|cx| ApplicationMenu::new(window, cx)))
            }
        };

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.observe(&workspace.weak_handle().upgrade().unwrap(), |_, _, cx| {
                cx.notify()
            }),
        );

        subscriptions.push(cx.observe(&active_call, |this, _, cx| this.active_call_changed(cx)));
        subscriptions.push(
            cx.subscribe(&git_store, move |_, _, event, cx| match event {
                GitStoreEvent::ActiveRepositoryChanged(_)
                | GitStoreEvent::RepositoryUpdated(_, _, true) => {
                    cx.notify();
                }
                _ => {}
            }),
        );
        subscriptions.push(cx.observe(&user_store, |_a, _, cx| cx.notify()));
        subscriptions.push(cx.observe(workspace.left_dock(), |_, _, cx| cx.notify()));
        subscriptions.push(cx.observe(workspace.bottom_dock(), |_, _, cx| cx.notify()));
        if let Some(multi_workspace) = multi_workspace.as_ref().and_then(|mw| mw.upgrade()) {
            subscriptions.push(cx.observe(&multi_workspace, |_, _, cx| cx.notify()));
        }
        if let Some(workspace_entity) = workspace.weak_handle().upgrade() {
            subscriptions.push(cx.subscribe(
                &workspace_entity,
                |_, _, event: &workspace::Event, cx| {
                    if matches!(event, workspace::Event::WorktreeCreationChanged) {
                        cx.notify();
                    }
                },
            ));
        }
        subscriptions.push(cx.observe_button_layout_changed(window, |_, _, cx| cx.notify()));
        if let Some(trusted_worktrees) = TrustedWorktrees::try_get_global(cx) {
            subscriptions.push(cx.subscribe(&trusted_worktrees, |_, _, _, cx| {
                cx.notify();
            }));
        }

        let update_version = cx.new(|cx| UpdateVersion::new(cx));
        let platform_titlebar = cx.new(|cx| {
            let mut titlebar = PlatformTitleBar::new(id, cx);
            if let Some(mw) = multi_workspace.clone() {
                titlebar = titlebar.with_multi_workspace(mw);
            }
            titlebar
        });

        let banner = None;

        let mut this = Self {
            platform_titlebar,
            application_menu,
            workspace: workspace.weak_handle(),
            multi_workspace,
            project,
            user_store,
            client,
            _subscriptions: subscriptions,
            banner,
            update_version,
            screen_share_popover_handle: PopoverMenuHandle::default(),
            _diagnostics_subscription: None,
        };

        this.observe_diagnostics(cx);

        this
    }

    fn toggle_update_simulation(&mut self, cx: &mut Context<Self>) {
        self.update_version
            .update(cx, |banner, cx| banner.update_simulation(cx));
        cx.notify();
    }

    pub fn render_restricted_mode(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let has_restricted_worktrees =
            TrustedWorktrees::has_restricted_worktrees(&self.project.read(cx).worktree_store(), cx);
        if !has_restricted_worktrees {
            return None;
        }

        let button = Button::new("restricted_mode_trigger", "Restricted Mode")
            .style(ButtonStyle::Tinted(TintColor::Warning))
            .label_size(LabelSize::Small)
            .color(Color::Warning)
            .start_icon(
                Icon::new(IconName::Warning)
                    .size(IconSize::Small)
                    .color(Color::Warning),
            )
            .tooltip(|_, cx| {
                Tooltip::with_meta(
                    "You're in Restricted Mode",
                    Some(&ToggleWorktreeSecurity),
                    "Mark this project as trusted and unlock all features",
                    cx,
                )
            })
            .on_click({
                cx.listener(move |this, _, window, cx| {
                    this.workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_worktree_trust_security_modal(true, window, cx)
                        })
                        .log_err();
                })
            });

        if ui::utils::MACOS_SDK_26_OR_LATER {
            // Make up for Tahoe's traffic light buttons having less spacing around them
            Some(div().child(button).ml_0p5().into_any_element())
        } else {
            Some(button.into_any_element())
        }
    }

    fn active_call_changed(&mut self, cx: &mut Context<Self>) {
        self.observe_diagnostics(cx);
        cx.notify();
    }

    fn observe_diagnostics(&mut self, cx: &mut Context<Self>) {
        let diagnostics = ActiveCall::global(cx)
            .read(cx)
            .room()
            .and_then(|room| room.read(cx).diagnostics().cloned());

        if let Some(diagnostics) = diagnostics {
            self._diagnostics_subscription = Some(cx.observe(&diagnostics, |_, _, cx| cx.notify()));
        } else {
            self._diagnostics_subscription = None;
        }
    }

    fn share_project(&mut self, cx: &mut Context<Self>) {
        let active_call = ActiveCall::global(cx);
        let project = self.project.clone();
        active_call
            .update(cx, |call, cx| call.share_project(project, cx))
            .detach_and_log_err(cx);
    }

    fn unshare_project(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let active_call = ActiveCall::global(cx);
        let project = self.project.clone();
        active_call
            .update(cx, |call, cx| call.unshare_project(project, cx))
            .log_err();
    }

    fn render_connection_status(
        &self,
        status: &client::Status,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match status {
            client::Status::ConnectionError
            | client::Status::ConnectionLost
            | client::Status::Reauthenticating
            | client::Status::Reconnecting
            | client::Status::ReconnectionError { .. } => Some(
                div()
                    .id("disconnected")
                    .child(Icon::new(IconName::Disconnected).size(IconSize::Small))
                    .tooltip(Tooltip::text("Disconnected"))
                    .into_any_element(),
            ),
            client::Status::UpgradeRequired => {
                let auto_updater = auto_update::AutoUpdater::get(cx);
                let label = match auto_updater.map(|auto_update| auto_update.read(cx).status()) {
                    Some(AutoUpdateStatus::Updated { .. }) => "Please restart Zed to Collaborate",
                    Some(AutoUpdateStatus::Installing { .. })
                    | Some(AutoUpdateStatus::Downloading { .. })
                    | Some(AutoUpdateStatus::Checking) => "Updating...",
                    Some(AutoUpdateStatus::Idle)
                    | Some(AutoUpdateStatus::Errored { .. })
                    | None => "Please update Zed to Collaborate",
                };

                Some(
                    Button::new("connection-status", label)
                        .label_size(LabelSize::Small)
                        .on_click(|_, window, cx| {
                            if let Some(auto_updater) = auto_update::AutoUpdater::get(cx)
                                && auto_updater.read(cx).status().is_updated()
                            {
                                workspace::reload(cx);
                                return;
                            }
                            auto_update::check(&Default::default(), window, cx);
                        })
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }

    pub fn render_user_menu_button(&mut self, cx: &mut Context<Self>) -> impl Element {
        let show_update_button = self.update_version.read(cx).show_update_in_menu_bar();

        let user_store = self.user_store.clone();
        let workspace = self.workspace.clone();
        let user = user_store.read(cx).current_user();

        let user_avatar = user.as_ref().map(|u| u.avatar_uri.clone());
        let username = user.as_ref().map(|u| u.username.clone());

        let is_signed_in = user.is_some();

        let current_organization = user_store.read(cx).current_organization();
        let business_organization = current_organization
            .as_ref()
            .filter(|organization| !organization.is_personal);
        let organizations: Vec<_> = user_store
            .read(cx)
            .organizations()
            .iter()
            .map(|organization| {
                let plan = user_store.read(cx).plan_for_organization(&organization.id);
                (organization.clone(), plan)
            })
            .collect();

        let show_user_picture = TitleBarSettings::get_global(cx).show_user_picture;

        let trigger = if is_signed_in && show_user_picture {
            let avatar = user_avatar.map(|avatar| Avatar::new(avatar)).map(|avatar| {
                if show_update_button {
                    avatar.indicator(
                        div()
                            .absolute()
                            .bottom_0()
                            .right_0()
                            .child(Indicator::dot().color(Color::Accent)),
                    )
                } else {
                    avatar
                }
            });

            ButtonLike::new("user-menu")
                .aria_label("User menu")
                .tab_index(0isize)
                .child(
                    h_flex()
                        .when_some(business_organization, |this, organization| {
                            this.gap_2()
                                .child(Label::new(&organization.name).size(LabelSize::Small))
                        })
                        .children(avatar),
                )
        } else {
            ButtonLike::new("user-menu")
                .aria_label("User menu")
                .tab_index(0isize)
                .child(Icon::new(IconName::ChevronDown).size(IconSize::Small))
        };

        PopoverMenu::new("user-menu")
            .trigger(trigger)
            .menu(move |window, cx| {
                let username = username.clone();
                let current_organization = current_organization.clone();
                let organizations = organizations.clone();
                let user_store = user_store.clone();
                let workspace = workspace.clone();

                let ai_enabled = !project::DisableAiSettings::get_global(cx).disable_ai;
                let current_layout = AgentSettings::get_layout(cx);
                let is_editor = matches!(current_layout, WindowLayout::Editor(_));
                let is_agent = matches!(current_layout, WindowLayout::Agent(_));
                let is_custom = matches!(current_layout, WindowLayout::Custom(_));

                ContextMenu::build(window, cx, |menu, _, _cx| {
                    menu.when(is_signed_in, |this| {
                        let username = username.clone();
                        this.custom_entry(
                            move |_window, _cx| {
                                let username = username.clone().unwrap_or_default();

                                h_flex()
                                    .w_full()
                                    .justify_between()
                                    .child(Label::new(username))
                                    .into_any_element()
                            },
                            move |_, cx| {
                                cx.open_url(&zed_urls::account_url(cx));
                            },
                        )
                        .separator()
                    })
                    .when(show_update_button, |this| {
                        this.custom_entry(
                            move |_window, _cx| {
                                h_flex()
                                    .w_full()
                                    .gap_1()
                                    .justify_between()
                                    .child(Label::new("Restart to update Zed").color(Color::Accent))
                                    .child(
                                        Icon::new(IconName::Download)
                                            .size(IconSize::Small)
                                            .color(Color::Accent),
                                    )
                                    .into_any_element()
                            },
                            move |_, cx| {
                                workspace::reload(cx);
                            },
                        )
                        .separator()
                    })
                    .when(is_signed_in, |this| {
                        let mut this = this.header("Organization");

                        for (organization, plan) in &organizations {
                            let organization = organization.clone();
                            let plan = *plan;

                            let is_current =
                                current_organization
                                    .as_ref()
                                    .is_some_and(|current_organization| {
                                        current_organization.id == organization.id
                                    });

                            this = this.custom_entry(
                                {
                                    let organization = organization.clone();
                                    move |_window, _cx| {
                                        h_flex()
                                            .w_full()
                                            .gap_4()
                                            .justify_between()
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .child(Label::new(&organization.name))
                                                    .when(is_current, |this| {
                                                        this.child(
                                                            Icon::new(IconName::Check)
                                                                .color(Color::Accent),
                                                        )
                                                    }),
                                            )
                                            .children(plan.map(|plan| PlanChip::new(plan)))
                                            .into_any_element()
                                    }
                                },
                                {
                                    let user_store = user_store.clone();
                                    let organization = organization.clone();
                                    let workspace = workspace.clone();
                                    move |window, cx| {
                                        let task = user_store.update(cx, |user_store, cx| {
                                            user_store
                                                .set_current_organization(organization.clone(), cx)
                                        });
                                        task.detach_and_notify_err(workspace.clone(), window, cx);
                                    }
                                },
                            );
                        }

                        this.separator()
                    })
                    .action("Settings", zed_actions::OpenSettings.boxed_clone())
                    .action("Keymap", Box::new(zed_actions::OpenKeymap))
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
                                    move |window, cx| {
                                        window.dispatch_action(UseClassicLayout.boxed_clone(), cx);
                                    },
                                )
                                .toggleable_entry(
                                    "Agentic",
                                    is_agent,
                                    IconPosition::Start,
                                    Some(UseAgenticLayout.boxed_clone()),
                                    move |window, cx| {
                                        window.dispatch_action(UseAgenticLayout.boxed_clone(), cx);
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
                    .when(is_signed_in, |this| {
                        this.separator()
                            .action("Sign Out", client::SignOut.boxed_clone())
                    })
                })
                .into()
            })
            .anchor(Anchor::TopRight)
    }
}
