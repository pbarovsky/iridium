use client::UserStore;
use git_ui_core::worktree_picker::WorktreePicker;
use gpui::{
    AnyElement, App, Context, Entity, Focusable, IntoElement, ParentElement, Render, Styled,
    Subscription, WeakEntity, Window,
};
use path::PathStyle;
use platform_title_bar::PlatformTitleBar;
use project::{
    Project, git_store::GitStoreEvent, linked_worktree_short_name,
    project_settings::ProjectSettings, repo_identity_path,
};
use remote::RemoteConnectionOptions;
use settings::{Settings as _, SettingsStore};
use theme::ActiveTheme;
use ui::{ButtonLike, IconWithIndicator, Indicator, PopoverMenu, TintColor, Tooltip, prelude::*};
use util::ResultExt;
use workspace::{HideStatusItem, ItemHandle, MultiWorkspace, StatusItemView, Workspace};
use zed_actions::OpenRemote;

use crate::title_bar_settings::TitleBarSettings;

const MAX_PROJECT_NAME_LENGTH: usize = 40;
const MAX_BRANCH_NAME_LENGTH: usize = 40;
const MAX_SHORT_SHA_LENGTH: usize = 8;

/// Shows the project name, worktree and branch at the left edge of the status bar,
/// where VS Code shows its remote and branch indicators.
pub struct ProjectStatusItem {
    project: Entity<Project>,
    user_store: Entity<UserStore>,
    workspace: WeakEntity<Workspace>,
    _subscriptions: Vec<Subscription>,
}

impl ProjectStatusItem {
    pub fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let project = workspace.project().clone();
        let git_store = project.read(cx).git_store().clone();
        let user_store = workspace.app_state().user_store.clone();

        let mut subscriptions = vec![
            cx.observe(&project, |_, _, cx| cx.notify()),
            cx.observe(&user_store, |_, _, cx| cx.notify()),
            cx.observe_global::<SettingsStore>(|_, cx| cx.notify()),
            cx.subscribe(&git_store, |_, _, event, cx| match event {
                GitStoreEvent::ActiveRepositoryChanged(_)
                | GitStoreEvent::RepositoryUpdated(_, _, true) => cx.notify(),
                _ => {}
            }),
        ];
        if let Some(workspace_entity) = workspace.weak_handle().upgrade() {
            subscriptions.push(cx.observe(&workspace_entity, |_, _, cx| cx.notify()));
            subscriptions.push(cx.subscribe(
                &workspace_entity,
                |_, _, event: &workspace::Event, cx| {
                    if matches!(event, workspace::Event::WorktreeCreationChanged) {
                        cx.notify();
                    }
                },
            ));
        }

        Self {
            project,
            user_store,
            workspace: workspace.weak_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn multi_workspace(&self, cx: &App) -> Option<Entity<MultiWorkspace>> {
        self.workspace
            .upgrade()?
            .read(cx)
            .multi_workspace()
            .and_then(|multi_workspace| multi_workspace.upgrade())
    }

    fn worktree_count(&self, cx: &App) -> usize {
        self.project.read(cx).visible_worktrees(cx).count()
    }

    /// Returns the worktree to display.
    /// - Prefer the worktree owning the project's active repository
    /// - Fall back to the first visible worktree
    fn effective_active_worktree(&self, cx: &App) -> Option<Entity<project::Worktree>> {
        let project = self.project.read(cx);

        if let Some(repo) = project.active_repository(cx) {
            let repo = repo.read(cx);
            let repo_path = &repo.work_directory_abs_path;

            for worktree in project.visible_worktrees(cx) {
                let worktree_path = worktree.read(cx).abs_path();
                if worktree_path == *repo_path || worktree_path.starts_with(repo_path.as_ref()) {
                    return Some(worktree);
                }
            }
        }

        project.visible_worktrees(cx).next()
    }

    fn get_repository_for_worktree(
        &self,
        worktree: &Entity<project::Worktree>,
        path_style: PathStyle,
        cx: &App,
    ) -> Option<Entity<project::git_store::Repository>> {
        let project = self.project.read(cx);
        let git_store = project.git_store().read(cx);
        let worktree_path = worktree.read(cx).abs_path();

        git_store
            .repositories()
            .values()
            .filter(|repo| {
                let repo_path = &repo.read(cx).work_directory_abs_path;
                worktree_path == *repo_path
                    || path_style.starts_with(&worktree_path, repo_path.as_ref())
            })
            .max_by_key(|repo| repo.read(cx).work_directory_abs_path.as_os_str().len())
            .cloned()
    }

    fn visible_worktrees_in_repository(
        &self,
        repository: &project::git_store::Repository,
        path_style: PathStyle,
        cx: &App,
    ) -> usize {
        let repo_path = &repository.work_directory_abs_path;
        self.project
            .read(cx)
            .visible_worktrees(cx)
            .filter(|worktree| {
                let worktree_path = worktree.read(cx).abs_path();
                worktree_path == *repo_path
                    || path_style.starts_with(&worktree_path, repo_path.as_ref())
            })
            .count()
    }

    fn render_remote_project_connection(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let workspace = self.workspace.clone();

        let options = self.project.read(cx).remote_connection_options(cx)?;
        let host: SharedString = options.display_name().into();

        let (tooltip_title, icon) = match options {
            RemoteConnectionOptions::Ssh(_) => ("Remote Project", IconName::Server),
            RemoteConnectionOptions::Wsl(_) => ("Remote Project", IconName::Linux),
            RemoteConnectionOptions::Docker(_dev_container_connection) => {
                ("Dev Container", IconName::Box)
            }
            #[cfg(any(test, feature = "test-support"))]
            RemoteConnectionOptions::Mock(_) => ("Mock Remote Project", IconName::Server),
        };

        let connection_state = self.project.read(cx).remote_connection_state(cx)?;
        let (indicator_color, icon_color, meta) = match connection_state {
            remote::ConnectionState::Connecting => {
                (Color::Info, Color::Info, format!("Connecting to: {host}"))
            }
            remote::ConnectionState::Connected => (
                Color::Success,
                Color::Default,
                format!("Connected to: {host}"),
            ),
            remote::ConnectionState::HeartbeatMissed => (
                Color::Warning,
                Color::Warning,
                format!("Connection attempt to {host} missed. Retrying..."),
            ),
            remote::ConnectionState::Reconnecting => (
                Color::Warning,
                Color::Warning,
                format!("Lost connection to {host}. Reconnecting..."),
            ),
            remote::ConnectionState::Disconnected => (
                Color::Error,
                Color::Error,
                format!("Disconnected from {host}"),
            ),
        };

        let meta = SharedString::from(meta);

        Some(
            PopoverMenu::new("remote-project-menu")
                .menu(move |window, cx| {
                    let workspace_entity = workspace.upgrade()?;
                    let fs = workspace_entity.read(cx).project().read(cx).fs().clone();
                    Some(recent_projects::RemoteServerProjects::popover(
                        fs,
                        workspace.clone(),
                        None,
                        window,
                        cx,
                    ))
                })
                .trigger_with_tooltip(
                    ButtonLike::new("remote_project")
                        .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                        .child(
                            h_flex()
                                .gap_2()
                                .max_w_32()
                                .child(
                                    IconWithIndicator::new(
                                        Icon::new(icon).size(IconSize::Small).color(icon_color),
                                        Some(Indicator::dot().color(indicator_color)),
                                    )
                                    .indicator_border_color(Some(
                                        cx.theme().colors().status_bar_background,
                                    ))
                                    .into_any_element(),
                                )
                                .child(Label::new(host).size(LabelSize::Small).truncate()),
                        ),
                    move |_window, cx| {
                        Tooltip::with_meta(
                            tooltip_title,
                            Some(&OpenRemote::default()),
                            meta.clone(),
                            cx,
                        )
                    },
                )
                .anchor(gpui::Anchor::BottomLeft)
                .attach(gpui::Anchor::TopLeft)
                .into_any_element(),
        )
    }

    fn render_project_host(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.project.read(cx).is_via_remote_server() {
            return self.render_remote_project_connection(cx);
        }

        if self.project.read(cx).is_disconnected(cx) {
            return Some(
                Button::new("disconnected", "Disconnected")
                    .disabled(true)
                    .color(Color::Disabled)
                    .label_size(LabelSize::Small)
                    .into_any_element(),
            );
        }

        let host = self.project.read(cx).host()?;
        let host_user = self.user_store.read(cx).get_cached_user(host.user_id)?;
        let participant_index = self
            .user_store
            .read(cx)
            .participant_indices()
            .get(&host_user.legacy_id)?;

        Some(
            Button::new("project_owner_trigger", host_user.username.clone())
                .color(Color::Player(participant_index.0))
                .label_size(LabelSize::Small)
                .tab_index(0isize)
                .tooltip(move |_, cx| {
                    let tooltip_title = format!(
                        "{} is sharing this project. Click to follow.",
                        host_user.username
                    );

                    Tooltip::with_meta(tooltip_title, None, "Click to Follow", cx)
                })
                .on_click({
                    let host_peer_id = host.peer_id;
                    cx.listener(move |this, _, window, cx| {
                        this.workspace
                            .update(cx, |workspace, cx| {
                                workspace.follow(host_peer_id, window, cx);
                            })
                            .log_err();
                    })
                })
                .into_any_element(),
        )
    }

    fn render_project_name(
        &self,
        name: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let workspace = self.workspace.clone();
        let is_project_selected = name.is_some();

        let display_name = if let Some(ref name) = name {
            util::truncate_and_trailoff(name, MAX_PROJECT_NAME_LENGTH)
        } else {
            "Open Recent Project".to_string()
        };

        let multi_workspace = self.multi_workspace(cx);
        let is_sidebar_open = multi_workspace
            .as_ref()
            .is_some_and(|multi_workspace| multi_workspace.read(cx).sidebar_open())
            && PlatformTitleBar::is_multi_workspace_enabled(cx);
        let is_threads_list_view_active = multi_workspace.as_ref().is_some_and(|multi_workspace| {
            multi_workspace.read(cx).is_threads_list_view_active(cx)
        });
        let menu_id = if is_sidebar_open && is_threads_list_view_active {
            "sidebar-status-recent-projects-menu"
        } else {
            "recent-projects-menu"
        };

        let focus_handle = workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).focus_handle(cx))
            .unwrap_or_else(|| cx.focus_handle());

        let window_project_groups: Vec<_> = multi_workspace
            .map(|multi_workspace| multi_workspace.read(cx).project_group_keys())
            .unwrap_or_default();

        PopoverMenu::new(menu_id)
            .menu(move |window, cx| {
                Some(recent_projects::RecentProjects::popover(
                    workspace.clone(),
                    window_project_groups.clone(),
                    None,
                    focus_handle.clone(),
                    window,
                    cx,
                ))
            })
            .trigger_with_tooltip(
                Button::new("project_name_trigger", display_name)
                    .label_size(LabelSize::Small)
                    .tab_index(0isize)
                    .start_icon(
                        Icon::new(IconName::Folder)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .when(self.worktree_count(cx) > 1, |this| {
                        this.end_icon(
                            Icon::new(IconName::ChevronUp)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                    })
                    .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                    .when(!is_project_selected, |button| button.color(Color::Muted)),
                move |_window, cx| {
                    Tooltip::for_action("Recent Projects", &zed_actions::OpenRecent::default(), cx)
                },
            )
            .anchor(gpui::Anchor::BottomLeft)
            .attach(gpui::Anchor::TopLeft)
    }

    fn render_worktree_and_branch(
        &self,
        repository: Entity<project::git_store::Repository>,
        linked_worktree_name: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let workspace = self.workspace.upgrade()?;

        let (branch_name, icon_info, is_detached_head) = {
            let repo = repository.read(cx);

            let is_detached_head = repo.branch.is_none();

            let branch_name = repo
                .branch
                .as_ref()
                .map(|branch| branch.name())
                .map(|name| util::truncate_and_trailoff(name, MAX_BRANCH_NAME_LENGTH))
                .or_else(|| {
                    repo.head_commit.as_ref().map(|commit| {
                        commit
                            .sha
                            .chars()
                            .take(MAX_SHORT_SHA_LENGTH)
                            .collect::<String>()
                    })
                });

            let status = repo.status_summary();
            let tracked = status.index + status.worktree;
            let icon_info = if status.conflict > 0 {
                (IconName::Warning, Color::VersionControlConflict)
            } else if tracked.modified > 0 {
                (IconName::SquareDot, Color::VersionControlModified)
            } else if tracked.added > 0 || status.untracked > 0 {
                (IconName::SquarePlus, Color::VersionControlAdded)
            } else if tracked.deleted > 0 {
                (IconName::SquareMinus, Color::VersionControlDeleted)
            } else {
                (IconName::GitBranch, Color::Muted)
            };

            (branch_name, icon_info, is_detached_head)
        };

        let settings = TitleBarSettings::get_global(cx);
        let effective_repository = Some(repository);

        let worktree_label: SharedString = linked_worktree_name.unwrap_or_else(|| "main".into());

        let creation = workspace.read(cx).active_worktree_creation();
        let creation_in_progress = creation.label.clone();
        let is_switch = creation.is_switch;
        let is_creating = creation_in_progress.is_some();

        let display_label: SharedString = if let Some(ref name) = creation_in_progress {
            if is_switch {
                format!("Loading {}…", name).into()
            } else {
                format!("Creating {}…", name).into()
            }
        } else {
            worktree_label.clone()
        };

        let worktree_button = settings.show_worktree_name.then(|| {
            let project = self.project.clone();
            let workspace_handle = workspace.downgrade();
            PopoverMenu::new("worktree-picker-menu")
                .menu(move |window, cx| {
                    // When opened from the status bar, focus is on the trigger
                    // button (not a dock), so `focused_dock` is `None`. That's
                    // fine — there's no prior dock focus to restore.
                    Some(cx.new(|cx| {
                        WorktreePicker::new(project.clone(), workspace_handle.clone(), window, cx)
                    }))
                })
                .trigger_with_tooltip(
                    Button::new("worktree_picker_trigger", display_label)
                        .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                        .label_size(LabelSize::Small)
                        .color(Color::Muted)
                        .tab_index(0isize)
                        .loading(is_creating)
                        .start_icon(
                            Icon::new(IconName::GitWorktree)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        ),
                    move |_window, cx| {
                        Tooltip::with_meta(
                            "Worktree",
                            Some(&zed_actions::git::Worktree),
                            format!("Currently In Use: {}", worktree_label),
                            cx,
                        )
                    },
                )
                .anchor(gpui::Anchor::BottomLeft)
                .attach(gpui::Anchor::TopLeft)
        });

        let branch_picker = branch_name.and_then(|branch_name| {
            settings.show_branch_name.then(|| {
                let branch_tooltip_label = branch_name.clone();
                let (branch_icon, branch_icon_color) = if settings.show_branch_status_icon {
                    icon_info
                } else {
                    (IconName::GitBranch, Color::Muted)
                };

                let trigger = if is_detached_head {
                    Button::new("project_branch_trigger", "Create Branch")
                        .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                        .label_size(LabelSize::Small)
                        .tab_index(0isize)
                        .start_icon(
                            Icon::new(IconName::GitBranchPlus)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                } else {
                    Button::new("project_branch_trigger", branch_name)
                        .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                        .label_size(LabelSize::Small)
                        .color(Color::Muted)
                        .tab_index(0isize)
                        .start_icon(
                            Icon::new(branch_icon)
                                .size(IconSize::XSmall)
                                .color(branch_icon_color),
                        )
                };

                PopoverMenu::new("branch-menu")
                    .menu(move |window, cx| {
                        git_ui_core::build_branch_picker(
                            workspace.downgrade(),
                            effective_repository.clone(),
                            window,
                            cx,
                        )
                    })
                    .trigger_with_tooltip(trigger, move |_window, cx| {
                        let meta = if is_detached_head {
                            format!("Detached HEAD: {}", branch_tooltip_label)
                        } else {
                            format!("Currently Checked Out: {}", branch_tooltip_label)
                        };
                        Tooltip::with_meta(
                            "Branch & Stash",
                            Some(&zed_actions::git::Branch),
                            meta,
                            cx,
                        )
                    })
                    .anchor(gpui::Anchor::BottomLeft)
                    .attach(gpui::Anchor::TopLeft)
            })
        });

        if worktree_button.is_none() && branch_picker.is_none() {
            return None;
        }

        let show_separator = worktree_button.is_some() && branch_picker.is_some();

        Some(
            h_flex()
                .gap_px()
                .children(worktree_button)
                .when(show_separator, |this| {
                    this.child(
                        Label::new("/")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .alpha(0.25),
                    )
                })
                .children(branch_picker)
                .into_any_element(),
        )
    }

    fn project_name_and_repository(
        &self,
        cx: &App,
    ) -> (
        Option<SharedString>,
        Option<Entity<project::git_store::Repository>>,
        Option<SharedString>,
    ) {
        let mut project_name = None;
        let mut repository = None;
        let mut linked_worktree_name = None;
        let Some(worktree) = self.effective_active_worktree(cx) else {
            return (project_name, repository, linked_worktree_name);
        };

        let path_style = worktree.read(cx).path_style();
        repository = self.get_repository_for_worktree(&worktree, path_style, cx);
        let worktree_abs_path = worktree.read(cx).abs_path();
        project_name = worktree
            .read(cx)
            .root_name()
            .file_name()
            .map(|name| SharedString::from(name.to_string()));
        if let Some(repo) = &repository {
            let repo = repo.read(cx);
            let identity = repo_identity_path(&repo.common_dir_abs_path, path_style);
            linked_worktree_name = linked_worktree_short_name(
                repo.linked_worktree_anchor_path(),
                repo.work_directory_abs_path.as_ref(),
                path_style,
            )
            .or_else(|| {
                repo.is_linked_worktree()
                    .then_some(project_name.clone())
                    .flatten()
            });

            let display_name =
                if path_style.extension(identity) == Some(std::ffi::OsStr::new("git")) {
                    path_style.file_stem(identity)
                } else {
                    path_style.file_name(identity)
                };

            if let Some(repo_name) = display_name.and_then(|name| name.to_str()) {
                let visible_worktrees_in_repo =
                    self.visible_worktrees_in_repository(repo, path_style, cx);
                let name = if visible_worktrees_in_repo == 1 {
                    match path_style
                        .strip_prefix(&worktree_abs_path, &*repo.work_directory_abs_path)
                    {
                        Ok(relative) if !relative.as_os_str().is_empty() => format!(
                            "{}{}{}",
                            repo_name,
                            path_style.primary_separator(),
                            relative.display()
                        ),
                        _ => repo_name.to_string(),
                    }
                } else {
                    repo_name.to_string()
                };
                project_name = Some(SharedString::from(name));
            }
        }

        (project_name, repository, linked_worktree_name)
    }
}

impl Render for ProjectStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = *TitleBarSettings::get_global(cx);
        let is_git_enabled = ProjectSettings::get_global(cx).git.enabled.status;
        let (project_name, repository, linked_worktree_name) = self.project_name_and_repository(cx);

        h_flex()
            .gap_0p5()
            .when(settings.show_project_items, |this| {
                this.children(self.render_project_host(cx))
                    .child(self.render_project_name(project_name, cx))
            })
            .when_some(repository.filter(|_| is_git_enabled), |this, repository| {
                this.children(self.render_worktree_and_branch(repository, linked_worktree_name, cx))
            })
    }
}

impl StatusItemView for ProjectStatusItem {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _cx: &App) -> Option<HideStatusItem> {
        Some(HideStatusItem::new(|settings| {
            let title_bar = settings.title_bar.get_or_insert_default();
            title_bar.show_project_items = Some(false);
            title_bar.show_worktree_name = Some(false);
            title_bar.show_branch_name = Some(false);
        }))
    }
}
