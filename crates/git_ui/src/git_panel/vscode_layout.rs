//! The VS Code style layout of the git panel, used when `git_panel.layout` is
//! `"custom"`: the commit editor and a full width commit button on top, the
//! changes below them, and collapsible sections for the last commit, the graph,
//! the history and the repositories at the bottom.
//!
//! It lives apart from the classic layout and reuses the panel's own pieces,
//! so upstream changes to the git panel merge without touching it.
//!
//! The graph and the history read the same git log, which the repository loads
//! once in the background and caches. The graph view only exists while its
//! section is open, and the history only keeps the commits it shows: it pages
//! through the log instead of copying all of it on every update.

use super::*;
use crate::git_graph::GitGraph;
use gpui::{EntityId, FontWeight};
use settings::GitPanelLayout;

/// How many commits the history section shows at first, and how many more each
/// "Load More" adds.
const HISTORY_PAGE_SIZE: usize = 50;
const GRAPH_SECTION_HEIGHT: Pixels = px(220.);
const HISTORY_SECTION_HEIGHT: Pixels = px(220.);
const REPOSITORIES_SECTION_MAX_HEIGHT: Pixels = px(160.);
const SECTION_HEADER_HEIGHT: Pixels = px(22.);
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

pub(super) struct VsCodeLayout {
    last_commit_expanded: bool,
    graph_expanded: bool,
    history_expanded: bool,
    repositories_expanded: bool,
    graph: Option<EmbeddedGraph>,
    history: Option<HistoryPage>,
}

impl Default for VsCodeLayout {
    fn default() -> Self {
        Self {
            last_commit_expanded: false,
            graph_expanded: false,
            history_expanded: false,
            repositories_expanded: false,
            graph: None,
            history: None,
        }
    }
}

impl GitPanel {
    pub(super) fn uses_vscode_layout(cx: &App) -> bool {
        GitPanelSettings::get_global(cx).layout == GitPanelLayout::Custom
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
        let source = self.section_source(cx);

        if self.vscode_layout.graph_expanded {
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

        if self.vscode_layout.history_expanded {
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
            }
        }
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

    fn load_more_history(&mut self, cx: &mut Context<Self>) {
        if let Some(history) = self.vscode_layout.history.as_mut() {
            history.len += HISTORY_PAGE_SIZE;
            self.fetch_history_page(cx);
        }
    }

    fn toggle_vscode_section(&mut self, section: VsCodeSection, cx: &mut Context<Self>) {
        let layout = &mut self.vscode_layout;
        match section {
            VsCodeSection::LastCommit => layout.last_commit_expanded = !layout.last_commit_expanded,
            VsCodeSection::Graph => {
                layout.graph_expanded = !layout.graph_expanded;
                if !layout.graph_expanded {
                    layout.graph = None;
                }
            }
            VsCodeSection::History => {
                layout.history_expanded = !layout.history_expanded;
                if !layout.history_expanded {
                    layout.history = None;
                    self.set_commit_history(CommitHistory::Loading, cx);
                }
            }
            VsCodeSection::Repositories => {
                layout.repositories_expanded = !layout.repositories_expanded
            }
        }
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

    /// A full width commit button with the commit options in a menu next to it,
    /// like VS Code's.
    fn render_vscode_commit_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
        v_flex().w_full().child(SplitButton::new(
            commit_button,
            self.render_vscode_commit_menu(cx).into_any_element(),
        ))
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
        h_flex()
            .id(id)
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
    }

    fn render_last_commit_section(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.vscode_layout.last_commit_expanded;
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
        let expanded = self.vscode_layout.graph_expanded;
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
                        .h(GRAPH_SECTION_HEIGHT)
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
        let expanded = self.vscode_layout.history_expanded;
        let can_load_more = self.vscode_layout.history.as_ref().is_some_and(|history| {
            matches!(&self.commit_history, CommitHistory::Loaded(entries) if entries.len() >= history.len)
        });

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
                        .h(HISTORY_SECTION_HEIGHT)
                        .w_full()
                        .overflow_hidden()
                        .child(self.render_history_tab(window, cx)),
                )
                .when(can_load_more, |this| {
                    this.child(
                        div().px_2().pb_1().child(
                            Button::new("vscode-history-load-more", "Load More")
                                .full_width()
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener(|this, _, _, cx| this.load_more_history(cx))),
                        ),
                    )
                })
            })
    }

    fn render_repositories_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = self.vscode_layout.repositories_expanded;
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
                    .max_h(REPOSITORIES_SECTION_MAX_HEIGHT)
                    .overflow_y_scroll()
                    .children(rows)
            }))
    }
}
