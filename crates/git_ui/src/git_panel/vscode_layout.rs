//! The VS Code style layout of the git panel: the commit editor and a full width commit button on top, the
//! changes below them, and collapsible sections for the last commit, the graph,
//! the history and the repositories at the bottom.
//!
//! It lives apart from the classic layout and reuses the panel's own pieces,
//! so upstream changes to the git panel merge without touching it.
//!
//! The graph and the history read the same git log, which the repository loads
//! once in the background and caches. The graph view only exists while its
//! section is open, and the history only keeps the commits scrolled through so
//! far: it takes the next page of the log as the list nears its end instead of
//! copying the whole log on every update.
//!
//! Which sections are open and how tall they are is remembered across restarts.

use super::*;
use crate::git_graph::GitGraph;
use gpui::{Bounds, DragMoveEvent, EntityId, FontWeight};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How many commits of the log the history section takes at a time.
const HISTORY_PAGE_SIZE: usize = 100;
/// The next page is taken once fewer than this many rows are left to scroll,
/// so scrolling doesn't run into the end.
const HISTORY_PRELOAD_ROWS: usize = 30;
const SECTION_HEADER_HEIGHT: Pixels = px(22.);
const MIN_SECTION_HEIGHT: f32 = 60.;
/// Room left for the commit editor and the changes when a section is resized.
const MIN_REST_OF_PANEL_HEIGHT: f32 = 200.;
const SASH_HEIGHT: Pixels = px(6.);
const LAYOUT_KEY: &str = "git_panel_vscode_layout";
const COMMIT_BUTTON_HEIGHT: Pixels = px(28.);

#[derive(Clone, Copy, PartialEq, Eq)]
enum VsCodeSection {
    LastCommit,
    Graph,
    History,
    Repositories,
}

/// The repository and log a section shows; the section reloads when it changes.
#[derive(Clone, PartialEq)]
struct SectionSource {
    repository: EntityId,
    log_source: LogSource,
}

struct EmbeddedGraph {
    source: SectionSource,
    view: Entity<GitGraph>,
}

struct HistoryPage {
    source: SectionSource,
    len: usize,
    _repository_subscription: Subscription,
}

/// Dragged to resize the section whose header it sits on.
struct DraggedSectionSash(VsCodeSection);

/// The part of the layout remembered across restarts.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct StoredLayout {
    last_commit_expanded: bool,
    graph_expanded: bool,
    history_expanded: bool,
    repositories_expanded: bool,
    graph_height: f32,
    history_height: f32,
    repositories_height: f32,
}

impl Default for StoredLayout {
    fn default() -> Self {
        Self {
            last_commit_expanded: false,
            graph_expanded: false,
            history_expanded: false,
            repositories_expanded: false,
            graph_height: 220.,
            history_height: 220.,
            repositories_height: 120.,
        }
    }
}

pub(super) struct VsCodeLayout {
    stored: StoredLayout,
    restore_started: bool,
    graph: Option<EmbeddedGraph>,
    history: Option<HistoryPage>,
    _serialization: Task<()>,
}

impl Default for VsCodeLayout {
    fn default() -> Self {
        Self {
            stored: StoredLayout::default(),
            restore_started: false,
            graph: None,
            history: None,
            _serialization: Task::ready(()),
        }
    }
}

impl GitPanel {
    /// This build always uses this layout. Zed's tests cover the classic one,
    /// so they still get it.
    pub(super) fn uses_vscode_layout(_cx: &App) -> bool {
        !cfg!(test)
    }

    fn section_source(&self, cx: &App) -> Option<SectionSource> {
        let repository = self.active_repository.as_ref()?;
        Some(SectionSource {
            repository: repository.entity_id(),
            log_source: Self::commit_history_log_source(repository, cx)?,
        })
    }

    /// Creates or drops the graph view and the history subscription so they
    /// match the open sections and the active repository and branch.
    pub(super) fn sync_vscode_sections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.vscode_layout.restore_started {
            self.vscode_layout.restore_started = true;
            self.restore_vscode_layout(cx);
        }
        let source = self.section_source(cx);

        if self.vscode_layout.stored.graph_expanded {
            let up_to_date = self
                .vscode_layout
                .graph
                .as_ref()
                .is_some_and(|graph| Some(&graph.source) == source.as_ref());
            if !up_to_date {
                self.vscode_layout.graph = source.clone().and_then(|source| {
                    let repository = self.active_repository.as_ref()?;
                    let repository_id = repository.read(cx).id;
                    let git_store = self.project.read(cx).git_store().clone();
                    let workspace = self.workspace.clone();
                    let log_source = source.log_source.clone();
                    let view = cx.new(|cx| {
                        GitGraph::new_embedded(
                            repository_id,
                            git_store,
                            workspace,
                            log_source,
                            window,
                            cx,
                        )
                    });
                    Some(EmbeddedGraph { source, view })
                });
            }
        }

        if self.vscode_layout.stored.history_expanded {
            let up_to_date = self
                .vscode_layout
                .history
                .as_ref()
                .is_some_and(|history| Some(&history.source) == source.as_ref());
            if !up_to_date {
                self.vscode_layout.history = None;
                self.set_commit_history(CommitHistory::Loading, cx);
                if let (Some(source), Some(repository)) = (source, self.active_repository.clone()) {
                    let subscription = cx.subscribe(&repository, |this, _, event, cx| {
                        if matches!(
                            event,
                            RepositoryEvent::GraphEvent(_, _) | RepositoryEvent::TagsChanged
                        ) {
                            this.fetch_history_page(cx);
                        }
                    });
                    self.vscode_layout.history = Some(HistoryPage {
                        source,
                        len: HISTORY_PAGE_SIZE,
                        _repository_subscription: subscription,
                    });
                }
                self.fetch_history_page(cx);
            } else {
                // Also catches scrolling by dragging the scrollbar.
                self.load_more_history_if_near_end(cx);
            }
        }
    }

    fn restore_vscode_layout(&mut self, cx: &mut Context<Self>) {
        let kvp = KeyValueStore::global(cx);
        cx.spawn(async move |this, cx| {
            let stored = cx
                .background_spawn(async move { kvp.read_kvp(LAYOUT_KEY) })
                .await
                .log_err()
                .flatten()
                .and_then(|json| serde_json::from_str::<StoredLayout>(&json).log_err());
            if let Some(stored) = stored {
                this.update(cx, |this, cx| {
                    this.vscode_layout.stored = stored;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn save_vscode_layout(&mut self, cx: &mut Context<Self>) {
        let Some(json) = serde_json::to_string(&self.vscode_layout.stored).log_err() else {
            return;
        };
        let kvp = KeyValueStore::global(cx);
        self.vscode_layout._serialization = cx.background_spawn(async move {
            kvp.write_kvp(LAYOUT_KEY.to_string(), json).await.log_err();
        });
    }

    /// Frees the graph view and the history subscription while the classic
    /// layout is in use.
    pub(super) fn release_vscode_sections(&mut self) {
        self.vscode_layout.graph = None;
        self.vscode_layout.history = None;
    }

    /// Loads only the commits the history section shows, not the whole log.
    fn fetch_history_page(&mut self, cx: &mut Context<Self>) {
        let Some(len) = self
            .vscode_layout
            .history
            .as_ref()
            .map(|history| history.len)
        else {
            return;
        };
        let Some(repository) = self.active_repository.clone() else {
            return;
        };
        let Some(log_source) = Self::commit_history_log_source(&repository, cx) else {
            self.set_commit_history(CommitHistory::Loaded(Rc::from([])), cx);
            return;
        };
        let (entries, is_loading, error) = repository.update(cx, |repository, cx| {
            let response = repository.graph_data(log_source, LogOrder::DateOrder, 0..len, cx);
            let entries: Rc<[CommitHistoryEntry]> = response
                .commits
                .iter()
                .map(CommitHistoryEntry::from)
                .collect();
            (entries, response.is_loading, response.error)
        });
        self.set_commit_history(commit_history_from_response(entries, is_loading, error), cx);
    }

    /// Takes the next page of the log once the list is scrolled close to the
    /// end of the commits taken so far.
    fn load_more_history_if_near_end(&mut self, cx: &mut Context<Self>) {
        let Some(history) = self.vscode_layout.history.as_mut() else {
            return;
        };
        let CommitHistory::Loaded(entries) = &self.commit_history else {
            return;
        };
        // Fewer commits than asked for: the log has no more, or is still being
        // read and the rest comes with its next update.
        if entries.len() < history.len {
            return;
        }
        let near_end = {
            let state = self.commit_history_scroll_handle.0.borrow();
            let Some(size) = state.last_item_size else {
                return;
            };
            let row_height = size.contents.height / entries.len().max(1) as f32;
            let left_to_scroll = state.base_handle.max_offset().y + state.base_handle.offset().y;
            left_to_scroll <= row_height * HISTORY_PRELOAD_ROWS as f32
        };
        if near_end {
            history.len += HISTORY_PAGE_SIZE;
            self.fetch_history_page(cx);
        }
    }

    fn resize_vscode_section(
        &mut self,
        section: VsCodeSection,
        bounds: Bounds<Pixels>,
        mouse_y: Pixels,
        cx: &mut Context<Self>,
    ) {
        let stored = &mut self.vscode_layout.stored;
        let header = f32::from(SECTION_HEADER_HEIGHT);
        let section_height =
            |expanded: bool, height: f32| header + if expanded { height } else { 0. };
        // The sections are stacked at the bottom of the panel, so everything
        // below the resized one keeps its place.
        let below = match section {
            VsCodeSection::LastCommit => return,
            VsCodeSection::Graph => {
                section_height(stored.history_expanded, stored.history_height)
                    + section_height(stored.repositories_expanded, stored.repositories_height)
            }
            VsCodeSection::History => {
                section_height(stored.repositories_expanded, stored.repositories_height)
            }
            VsCodeSection::Repositories => 0.,
        };
        let max_height =
            (f32::from(bounds.size.height) - MIN_REST_OF_PANEL_HEIGHT).max(MIN_SECTION_HEIGHT);
        let height = (f32::from(bounds.bottom() - mouse_y) - header - below)
            .clamp(MIN_SECTION_HEIGHT, max_height);
        match section {
            VsCodeSection::LastCommit => {}
            VsCodeSection::Graph => stored.graph_height = height,
            VsCodeSection::History => stored.history_height = height,
            VsCodeSection::Repositories => stored.repositories_height = height,
        }
        cx.notify();
    }

    fn toggle_vscode_section(&mut self, section: VsCodeSection, cx: &mut Context<Self>) {
        let layout = &mut self.vscode_layout;
        let stored = &mut layout.stored;
        match section {
            VsCodeSection::LastCommit => stored.last_commit_expanded = !stored.last_commit_expanded,
            VsCodeSection::Graph => {
                stored.graph_expanded = !stored.graph_expanded;
                if !stored.graph_expanded {
                    layout.graph = None;
                }
            }
            VsCodeSection::History => {
                stored.history_expanded = !stored.history_expanded;
                if !stored.history_expanded {
                    layout.history = None;
                    self.set_commit_history(CommitHistory::Loading, cx);
                }
            }
            VsCodeSection::Repositories => {
                stored.repositories_expanded = !stored.repositories_expanded
            }
        }
        self.save_vscode_layout(cx);
        cx.notify();
    }

    pub(super) fn render_vscode_layout(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let has_write_access = self.has_write_access(cx);
        let commit_area = self.render_vscode_commit_area(window, cx);

        if self.commit_editor_expanded {
            return v_flex()
                .size_full()
                .children(commit_area)
                .into_any_element();
        }

        v_flex()
            .size_full()
            .on_drag_move::<DraggedSectionSash>(cx.listener(
                |this, event: &DragMoveEvent<DraggedSectionSash>, _, cx| {
                    let section = event.drag(cx).0;
                    this.resize_vscode_section(section, event.bounds, event.event.position.y, cx);
                },
            ))
            .on_drop::<DraggedSectionSash>(cx.listener(|this, _, _, cx| {
                this.save_vscode_layout(cx);
            }))
            .children(commit_area)
            .when(self.amend_pending, |this| {
                this.child(self.render_pending_amend(cx))
            })
            .children(self.render_changes_header(window, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .map(|this| match self.active_repository.clone() {
                        Some(repository) if !self.entries.is_empty() => this
                            .child(self.render_entries(has_write_access, repository, window, cx)),
                        _ => this.child(self.render_empty_state(cx).into_any_element()),
                    }),
            )
            .when(self.active_repository.is_some(), |this| {
                this.child(self.render_last_commit_section(window, cx))
                    .child(self.render_graph_section(cx))
                    .child(self.render_history_section(window, cx))
            })
            .child(self.render_repositories_section(cx))
            .into_any_element()
    }

    fn render_vscode_commit_area(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.active_repository.as_ref()?;
        let colors = cx.theme().colors();
        let editor_style = git_commit_editor_style(
            ThemeSettings::get_global(cx).git_commit_buffer_font_size(cx),
            cx,
        );
        let editor_focus_handle = self.commit_editor.focus_handle(cx);
        let editor_focused = editor_focus_handle.is_focused(window);

        let max_title_length = GitPanelSettings::get_global(cx).commit_title_max_length;
        let title_exceeds_limit = max_title_length > 0
            && self
                .commit_editor
                .read(cx)
                .text(cx)
                .lines()
                .next()
                .is_some_and(|title| commit_title_exceeds_limit(title, max_title_length));

        let (fill_icon, fill_label) = if self.commit_editor_expanded {
            (IconName::Minimize, "Collapse Commit Editor")
        } else {
            (IconName::Maximize, "Expand Commit Editor")
        };
        let panel_focus_handle = self.focus_handle.clone();
        let editor_buttons = v_flex()
            .p_1()
            .gap_px()
            .child(
                IconButton::new("vscode-open-commit-modal", IconName::MaximizeAlt)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip({
                        let editor_focus_handle = editor_focus_handle.clone();
                        move |_window, cx| {
                            Tooltip::for_action_in(
                                "Open Commit Modal",
                                &git::ExpandCommitEditor,
                                &editor_focus_handle,
                                cx,
                            )
                        }
                    })
                    .on_click(|_, window, cx| {
                        window.dispatch_action(git::ExpandCommitEditor.boxed_clone(), cx)
                    }),
            )
            .child(
                IconButton::new("vscode-fill-commit-editor", fill_icon)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(move |_window, cx| {
                        Tooltip::for_action_in(
                            fill_label,
                            &git::ToggleFillCommitEditor,
                            &panel_focus_handle,
                            cx,
                        )
                    })
                    .on_click(|_, window, cx| {
                        window.dispatch_action(git::ToggleFillCommitEditor.boxed_clone(), cx)
                    }),
            )
            .children(self.render_generate_commit_message_button(cx))
            .children(self.render_co_authors(cx));

        let input_border = if title_exceeds_limit {
            cx.theme().status().warning_border
        } else if editor_focused {
            colors.border_focused
        } else {
            colors.border
        };

        Some(
            v_flex()
                .p_2()
                .gap_1p5()
                .border_b_1()
                .border_color(colors.border)
                .when(self.commit_editor_expanded, |this| this.flex_1().min_h_0())
                .child(
                    h_flex()
                        .id("vscode-commit-editor")
                        .w_full()
                        .items_start()
                        .rounded_md()
                        .border_1()
                        .border_color(input_border)
                        .bg(colors.editor_background)
                        .when(self.commit_editor_expanded, |this| this.flex_1().min_h_0())
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            window.focus(&this.commit_editor.focus_handle(cx), cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .px_2()
                                .py_1p5()
                                .cursor_text()
                                .on_action(|&zed_actions::editor::MoveUp, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_action(|&zed_actions::editor::MoveDown, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(EditorElement::new(&self.commit_editor, editor_style)),
                        )
                        .child(editor_buttons),
                )
                .when(title_exceeds_limit, |this| {
                    this.child(
                        h_flex()
                            .gap_1()
                            .child(
                                Icon::new(IconName::Warning)
                                    .size(IconSize::XSmall)
                                    .color(Color::Warning),
                            )
                            .child(
                                Label::new(format!(
                                    "Commit message title exceeds {max_title_length}-character limit."
                                ))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            ),
                    )
                })
                .child(self.render_vscode_commit_button(cx))
                .into_any_element(),
        )
    }

    /// What the commit button does instead of committing when there is nothing
    /// to commit, like VS Code's "Sync Changes" and "Publish Branch".
    fn sync_action(&self, cx: &App) -> Option<SyncAction> {
        if self.entry_count > 0 || self.is_branch_mode() || !self.can_push_and_pull(cx) {
            return None;
        }
        let repository = self.active_repository.as_ref()?.read(cx);
        let branch = repository.branch.as_ref()?;
        match &branch.upstream {
            None
            | Some(Upstream {
                tracking: UpstreamTracking::Gone,
                ..
            }) => Some(SyncAction::Publish),
            Some(Upstream {
                tracking: UpstreamTracking::Tracked(UpstreamTrackingStatus { ahead, behind }),
                ..
            }) => match (*ahead, *behind) {
                (0, 0) => None,
                (ahead, 0) => Some(SyncAction::Push { ahead }),
                (ahead, behind) => Some(SyncAction::Sync { ahead, behind }),
            },
        }
    }

    /// Pulls, then pushes what's left to push once the pull has finished and the
    /// branch's tracking status has caught up.
    fn sync_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const POLL_INTERVAL: Duration = Duration::from_millis(250);
        const MAX_POLLS: usize = 40;

        self.pull(false, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            while this.read_with(cx, |this, _| this.pending_remote_operation.is_some())? {
                cx.background_executor().timer(POLL_INTERVAL).await;
            }
            // A failed pull leaves the branch behind, so nothing is pushed.
            for _ in 0..MAX_POLLS {
                let action = this.read_with(cx, |this, cx| this.sync_action(cx))?;
                match action {
                    Some(SyncAction::Push { .. }) => {
                        this.update_in(cx, |this, window, cx| this.push(false, false, window, cx))?;
                        break;
                    }
                    Some(SyncAction::Sync { .. }) => {
                        cx.background_executor().timer(POLL_INTERVAL).await;
                    }
                    Some(SyncAction::Publish) | None => break,
                }
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    /// The commit button's place when there's nothing to commit but the branch
    /// differs from its remote: one button that publishes, pushes, or pulls and
    /// then pushes, with the number of commits each way.
    fn render_vscode_sync_button(&self, action: SyncAction, cx: &mut Context<Self>) -> AnyElement {
        let in_progress = self.pending_remote_operation.is_some();
        let (icon, label, tooltip): (IconName, SharedString, &'static str) = match action {
            SyncAction::Publish => (
                IconName::ArrowUp,
                "Publish Branch".into(),
                "Publish this branch to the remote",
            ),
            SyncAction::Push { ahead } => (
                IconName::ArrowCircle,
                format!("Sync Changes  {ahead}↑").into(),
                "Push the commits that aren't on the remote yet",
            ),
            SyncAction::Sync { ahead, behind } => (
                IconName::ArrowCircle,
                if ahead > 0 {
                    format!("Sync Changes  {behind}↓ {ahead}↑").into()
                } else {
                    format!("Sync Changes  {behind}↓").into()
                },
                "Pull the remote's new commits, then push yours",
            ),
        };
        let label = if in_progress {
            "Syncing…".into()
        } else {
            label
        };

        v_flex()
            .w_full()
            .child(
                ButtonLike::new("vscode-sync-button")
                    .style(ButtonStyle::Outlined)
                    .layer(ElevationIndex::ModalSurface)
                    .full_width()
                    .height(COMMIT_BUTTON_HEIGHT.into())
                    .disabled(in_progress)
                    .child(
                        h_flex()
                            .w_full()
                            .justify_center()
                            .gap_1p5()
                            .child(Icon::new(icon).size(IconSize::Small).color(if in_progress {
                                Color::Disabled
                            } else {
                                Color::Muted
                            }))
                            .child(Label::new(label).color(if in_progress {
                                Color::Disabled
                            } else {
                                Color::Default
                            })),
                    )
                    .tooltip(Tooltip::text(tooltip))
                    .on_click(cx.listener(move |this, _, window, cx| match action {
                        SyncAction::Publish | SyncAction::Push { .. } => {
                            this.push(false, false, window, cx)
                        }
                        SyncAction::Sync { .. } => this.sync_changes(window, cx),
                    })),
            )
            .into_any_element()
    }

    /// A full width commit button with the commit options in a menu next to it,
    /// like VS Code's.
    fn render_vscode_commit_button(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(action) = self.sync_action(cx) {
            return self.render_vscode_sync_button(action, cx);
        }
        let (can_commit, tooltip) = self.configure_commit_button(cx);
        let title = self.commit_button_title();
        let editor_focus_handle = self.commit_editor.focus_handle(cx);
        let options = self.commit_options();
        let command = format!(
            "git commit{}{}{}",
            if options.amend { " --amend" } else { "" },
            if options.signoff { " --signoff" } else { "" },
            if options.no_verify {
                " --no-verify"
            } else {
                ""
            }
        );
        let disabled = !can_commit || self.modal_open;

        // The same dark, outlined split button as "Stage All", only full width
        // and taller.
        let commit_button = ButtonLike::new_rounded_left("vscode-commit-button")
            .layer(ElevationIndex::ModalSurface)
            .full_width()
            .height(COMMIT_BUTTON_HEIGHT.into())
            .disabled(disabled)
            .child(
                h_flex()
                    .id("vscode-commit-button-label")
                    .w_full()
                    .justify_center()
                    // Hovering the button itself shows which files a tracked
                    // commit takes; the options menu next to it doesn't.
                    .on_hover(cx.listener(|this, hovered, _, cx| {
                        this.show_placeholders = *hovered
                            && !this.has_staged_changes()
                            && !this.has_unstaged_conflicts();
                        cx.notify()
                    }))
                    .child(Label::new(title).color(if disabled {
                        Color::Disabled
                    } else {
                        Color::Default
                    })),
            )
            .on_click({
                let git_panel = cx.weak_entity();
                move |_, window, cx| {
                    git_panel
                        .update(cx, |git_panel, cx| {
                            let options = git_panel.commit_options();
                            git_panel.commit_changes(options, window, cx);
                        })
                        .ok();
                }
            })
            .tooltip(move |_window, cx| {
                if can_commit {
                    Tooltip::with_meta_in(
                        tooltip,
                        Some(&git::Commit),
                        command.clone(),
                        &editor_focus_handle,
                        cx,
                    )
                } else {
                    Tooltip::simple(tooltip, cx)
                }
            });

        // A column stretches the split button, which has no width of its own,
        // across the panel.
        v_flex()
            .w_full()
            .child(SplitButton::new(
                commit_button,
                self.render_vscode_commit_menu(cx).into_any_element(),
            ))
            .into_any_element()
    }

    /// The commit options (amend, signoff, skip hooks), as in the classic
    /// layout's commit split button, behind a trigger as tall as the button.
    fn render_vscode_commit_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let menu_open = self.commit_menu_handle.is_deployed();
        let keybinding_target = self.commit_editor.focus_handle(cx);
        let git_panel = cx.entity();
        let has_previous_commit = self.head_commit(cx).is_some();
        let amend = self.amend_pending();
        let signoff = self.signoff_enabled;
        let skip_hooks = self.skip_hooks_enabled;

        PopoverMenu::new("vscode-commit-menu")
            .trigger(
                ButtonLike::new_rounded_right("vscode-commit-menu-trigger")
                    .layer(ElevationIndex::ModalSurface)
                    .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                    .width(COMMIT_BUTTON_HEIGHT)
                    .height(COMMIT_BUTTON_HEIGHT.into())
                    .disabled(self.generate_commit_message_task.is_some())
                    .child(
                        Icon::new(if menu_open {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .size(IconSize::Small),
                    ),
            )
            .with_handle(self.commit_menu_handle.clone())
            .menu(move |window, cx| {
                let keybinding_target = keybinding_target.clone();
                let git_panel = git_panel.downgrade();
                Some(ContextMenu::build(window, cx, move |menu, _, _| {
                    menu.context(keybinding_target)
                        .when(has_previous_commit, |this| {
                            this.toggleable_entry(
                                "Amend",
                                amend,
                                IconPosition::Start,
                                Some(Box::new(Amend)),
                                move |_, cx| {
                                    git_panel
                                        .update(cx, |git_panel, cx| {
                                            git_panel.toggle_amend_pending(cx);
                                        })
                                        .ok();
                                },
                            )
                        })
                        .toggleable_entry(
                            "Signoff",
                            signoff,
                            IconPosition::Start,
                            Some(Box::new(Signoff)),
                            move |window, cx| window.dispatch_action(Box::new(Signoff), cx),
                        )
                        .toggleable_entry(
                            "Skip Hooks",
                            skip_hooks,
                            IconPosition::Start,
                            Some(Box::new(SkipHooks)),
                            move |window, cx| window.dispatch_action(Box::new(SkipHooks), cx),
                        )
                }))
            })
            .anchor(Anchor::TopRight)
            .offset(gpui::Point {
                x: px(0.),
                y: px(2.),
            })
    }

    fn render_section_header(
        &self,
        section: VsCodeSection,
        id: &'static str,
        title: &'static str,
        expanded: bool,
        end_slot: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let resizable = expanded && section != VsCodeSection::LastCommit;
        h_flex()
            .id(id)
            .relative()
            .flex_none()
            .h(SECTION_HEADER_HEIGHT)
            .w_full()
            .pl_1()
            .pr_1p5()
            .gap_1()
            .border_t_1()
            .border_color(colors.border)
            .cursor_pointer()
            .hover(|style| style.bg(colors.ghost_element_hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_vscode_section(section, cx)
            }))
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .child(
                Label::new(title)
                    .size(LabelSize::XSmall)
                    .weight(FontWeight::SEMIBOLD)
                    .color(Color::Muted),
            )
            .child(div().flex_1())
            .children(end_slot)
            // Straddles the header's top border, like the sashes between the
            // sections of VS Code's panels.
            .when(resizable, |this| {
                this.child(
                    div()
                        .id(SharedString::from(format!("{id}-sash")))
                        .absolute()
                        .top(-SASH_HEIGHT / 2.)
                        .left_0()
                        .right_0()
                        .h(SASH_HEIGHT)
                        .cursor_row_resize()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .on_drag(DraggedSectionSash(section), |_, _, _, cx| {
                            cx.new(|_| gpui::Empty)
                        }),
                )
            })
    }

    fn render_last_commit_section(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.vscode_layout.stored.last_commit_expanded;
        v_flex()
            .flex_none()
            .child(self.render_section_header(
                VsCodeSection::LastCommit,
                "vscode-last-commit-header",
                "LAST COMMIT",
                expanded,
                None,
                cx,
            ))
            .when(expanded, |this| {
                this.children(self.render_previous_commit(window, cx))
            })
    }

    fn render_graph_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = self.vscode_layout.stored.graph_expanded;
        let open_graph = IconButton::new("vscode-open-git-graph", IconName::GitGraph)
            .icon_size(IconSize::Small)
            .tooltip(|_window, cx| {
                Tooltip::for_action("Open Git Graph", &crate::git_graph::Open, cx)
            })
            .on_click(|_, window, cx| {
                cx.stop_propagation();
                window.dispatch_action(crate::git_graph::Open.boxed_clone(), cx)
            })
            .into_any_element();

        v_flex()
            .flex_none()
            .child(self.render_section_header(
                VsCodeSection::Graph,
                "vscode-graph-header",
                "GRAPH",
                expanded,
                Some(open_graph),
                cx,
            ))
            .when(expanded, |this| {
                this.children(self.vscode_layout.graph.as_ref().map(|graph| {
                    div()
                        .h(px(self.vscode_layout.stored.graph_height))
                        .w_full()
                        .overflow_hidden()
                        .child(graph.view.clone())
                }))
            })
    }

    fn render_history_section(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.vscode_layout.stored.history_expanded;
        let height = px(self.vscode_layout.stored.history_height);

        v_flex()
            .flex_none()
            .child(self.render_section_header(
                VsCodeSection::History,
                "vscode-history-header",
                "HISTORY",
                expanded,
                None,
                cx,
            ))
            .when(expanded, |this| {
                this.child(
                    v_flex()
                        .id("vscode-history")
                        .h(height)
                        .w_full()
                        .overflow_hidden()
                        .on_scroll(
                            cx.listener(|this, _, _, cx| this.load_more_history_if_near_end(cx)),
                        )
                        .child(self.render_history_tab(window, cx)),
                )
            })
    }

    fn render_repositories_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = self.vscode_layout.stored.repositories_expanded;
        let colors = cx.theme().colors();
        let active_repository = self
            .active_repository
            .as_ref()
            .map(|repository| repository.entity_id());

        let rows = expanded.then(|| {
            let mut repositories: Vec<Entity<Repository>> = self
                .project
                .read(cx)
                .git_store()
                .read(cx)
                .repositories()
                .values()
                .cloned()
                .collect();
            repositories.sort_by_cached_key(|repository| repository.read(cx).display_name());

            repositories
                .into_iter()
                .enumerate()
                .map(|(index, repository)| {
                    let is_active = active_repository == Some(repository.entity_id());
                    let snapshot = repository.read(cx);
                    let name = snapshot.display_name();
                    let branch = snapshot.branch.as_ref();
                    let branch_name: Option<SharedString> =
                        branch.map(|branch| branch.name().to_string().into());
                    let tracking = branch
                        .and_then(|branch| branch.upstream.as_ref())
                        .and_then(|upstream| upstream.tracking.status());

                    h_flex()
                        .id(("vscode-repository", index))
                        .h(SECTION_HEADER_HEIGHT)
                        .w_full()
                        .px_2()
                        .gap_1p5()
                        .cursor_pointer()
                        .when(is_active, |this| this.bg(colors.ghost_element_selected))
                        .hover(|style| style.bg(colors.ghost_element_hover))
                        .on_click(move |_, _, cx| {
                            repository.update(cx, |repository, cx| {
                                repository.set_as_active_repository(cx)
                            });
                        })
                        .child(
                            Icon::new(IconName::Folder)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .child(Label::new(name).size(LabelSize::Small).truncate())
                        .children(branch_name.map(|branch_name| {
                            h_flex()
                                .gap_0p5()
                                .min_w_0()
                                .child(
                                    Icon::new(IconName::GitBranch)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .child(
                                    Label::new(branch_name)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .truncate(),
                                )
                        }))
                        .child(div().flex_1())
                        .children(
                            tracking
                                .filter(|status| status.ahead > 0 || status.behind > 0)
                                .map(|status| {
                                    Label::new(format!("{}↓ {}↑", status.behind, status.ahead))
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted)
                                }),
                        )
                })
                .collect::<Vec<_>>()
        });

        v_flex()
            .flex_none()
            .child(self.render_section_header(
                VsCodeSection::Repositories,
                "vscode-repositories-header",
                "REPOSITORIES",
                expanded,
                None,
                cx,
            ))
            .children(rows.map(|rows| {
                v_flex()
                    .id("vscode-repositories")
                    .h(px(self.vscode_layout.stored.repositories_height))
                    .overflow_y_scroll()
                    .children(rows)
            }))
    }
}

#[derive(Clone, Copy)]
enum SyncAction {
    Publish,
    Push { ahead: u32 },
    Sync { ahead: u32, behind: u32 },
}
