//! A commit shown one file at a time, like VS Code: the commit's details and
//! its changed files on the left, styled like the git graph's details panel,
//! and the selected file in full with its changes on the right. Used instead of
//! [`CommitView`](crate::commit_view::CommitView), except for stashes and in
//! Zed's own tests.
//!
//! Only the selected file gets a buffer and a diff; picking another file
//! replaces them, so a commit with many files costs no more than one file.

use std::{rc::Rc, sync::Arc};

use anyhow::Result;
use buffer_diff::BufferDiff;
use collections::HashMap;
use editor::{DiffStyleControls, EditorSettings, SplittableEditor};
use git::{
    BuildCommitPermalinkParams, GitHostingProviderRegistry, GitRemote, ParsedGitRemote,
    parse_git_remote_url,
    repository::{CommitDetails, RepoPath},
};
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, KeyContext, ParentElement, Render, ScrollHandle, ScrollStrategy,
    Styled, Subscription, Task, UniformListScrollHandle, WeakEntity, Window, uniform_list,
};
use language::{Buffer, line_diff};
use markdown::{Markdown, MarkdownElement};
use multi_buffer::PathKey;
use project::{
    Project,
    git_store::{CommitFile, Repository},
};
use settings::{GitPanelClickBehavior, Settings, SettingsStore};
use ui::{
    DiffStat, Disclosure, Divider, IndentGuideColors, ListItem, ListItemSpacing, Tooltip,
    WithScrollbar, prelude::*,
};
use util::{ResultExt as _, paths::PathStyle, truncate_and_trailoff};
use workspace::{
    Item, Workspace,
    item::{ItemEvent, TabContentParams},
};

use crate::{
    commit_tooltip::CommitAvatar,
    commit_view::{GitBlob, build_buffer, build_buffer_diff, worktree_id_for_repo_path},
    git_graph::{
        COPIED_STATE_DURATION, ChangedFileEntry, ChangedFileTreeEntry, ChangedFilesViewMode,
        CopiedState, GitGraphSettings, TREE_INDENT, build_changed_file_tree_entries,
        format_timestamp,
    },
    git_panel_settings::GitPanelSettings,
    git_status_icon,
    solo_diff_view::{SoloDiffView, render_file_diff_mode_controls, save_show_full_file},
};

const DETAILS_PANEL_WIDTH: Pixels = px(320.);
const BINARY_PLACEHOLDER: &str = "(binary file not shown)";

/// A row of the changed files list.
enum FileRow {
    Directory {
        path: RepoPath,
        name: SharedString,
        depth: usize,
        expanded: bool,
    },
    File {
        index: usize,
        entry: ChangedFileEntry,
        depth: usize,
        directory_label: Option<SharedString>,
    },
}

pub struct CommitFilesView {
    commit: CommitDetails,
    files: Vec<CommitFile>,
    file_entries: Vec<ChangedFileEntry>,
    /// Lines added and removed, per file and in total.
    file_stats: Vec<(usize, usize)>,
    diff_stats: (usize, usize),
    is_shallow_boundary: bool,
    message: Entity<Markdown>,
    message_collapsed: bool,
    message_scroll_handle: ScrollHandle,
    remote: Option<GitRemote>,
    view_mode: ChangedFilesViewMode,
    expanded_dirs: HashMap<RepoPath, bool>,
    selected: Option<usize>,
    editor: Option<Entity<SplittableEditor>>,
    /// The selected file's buffer and diff, once loaded.
    loaded_file: Option<(Entity<Buffer>, Entity<BufferDiff>)>,
    showing_full_file: bool,
    repository: Entity<Repository>,
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    list_scroll_handle: UniformListScrollHandle,
    _load_file: Task<()>,
    _settings_subscription: Subscription,
}

impl CommitFilesView {
    pub(crate) fn enabled(cx: &App) -> bool {
        GitPanelSettings::get_global(cx).entry_primary_click_action
            == GitPanelClickBehavior::FileDiff
    }

    pub(crate) fn open(
        commit_sha: String,
        repository: WeakEntity<Repository>,
        workspace: WeakEntity<Workspace>,
        file_filter: Option<RepoPath>,
        window: &mut Window,
        cx: &mut App,
    ) {
        // A commit that is already open just shows the requested file, which
        // keeps clicking through a commit's files in the graph instant.
        if let Some(workspace_entity) = workspace.upgrade() {
            let pane = workspace_entity.read(cx).active_pane().clone();
            let existing = pane.read(cx).items().find_map(|item| {
                let view = item.downcast::<CommitFilesView>()?;
                (view.read(cx).commit.sha.as_ref() == commit_sha).then_some(view)
            });
            if let Some(existing) = existing {
                pane.update(cx, |pane, cx| {
                    if let Some(index) = pane.index_for_item(&existing) {
                        pane.activate_item(index, true, true, window, cx);
                    }
                });
                if let Some(path) = file_filter {
                    existing.update(cx, |view, cx| {
                        if let Some(index) = view.files.iter().position(|file| file.path == path) {
                            view.select_file(index, window, cx);
                        }
                    });
                }
                return;
            }
        }

        let commit_diff = repository
            .update(cx, |repository, cx| {
                repository.load_commit_diff(commit_sha.clone(), false, cx)
            })
            .ok();
        let commit_details = repository
            .update(cx, |repository, cx| {
                repository.show_commit(commit_sha.clone(), cx)
            })
            .ok();

        window
            .spawn(cx, async move |cx| {
                let (commit_diff, commit_details) = futures::join!(commit_diff?, commit_details?);
                let commit_diff = commit_diff.log_err()?;
                let commit_details = commit_details.log_err()?;
                let repository = repository.upgrade()?;

                workspace
                    .update_in(cx, |workspace, window, cx| {
                        let project = workspace.project().clone();
                        let workspace_handle = cx.weak_entity();
                        let view = cx.new(|cx| {
                            Self::new(
                                commit_details,
                                commit_diff,
                                file_filter,
                                repository,
                                project,
                                workspace_handle,
                                window,
                                cx,
                            )
                        });
                        workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
                    })
                    .log_err()
            })
            .detach();
    }

    fn new(
        commit: CommitDetails,
        commit_diff: project::git_store::CommitDiff,
        file_filter: Option<RepoPath>,
        repository: Entity<Repository>,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let files = commit_diff.files;
        let file_stats: Vec<(usize, usize)> = files
            .iter()
            .map(|file| {
                line_diff(
                    file.old_text.as_deref().unwrap_or_default(),
                    file.new_text.as_deref().unwrap_or_default(),
                )
                .iter()
                .fold((0, 0), |(added, removed), (old_range, new_range)| {
                    (added + new_range.len(), removed + old_range.len())
                })
            })
            .collect();
        let diff_stats =
            file_stats
                .iter()
                .fold((0, 0), |(added, removed), (file_added, file_removed)| {
                    (added + file_added, removed + file_removed)
                });
        let file_entries = files
            .iter()
            .map(|file| ChangedFileEntry::from_commit_file(file, cx))
            .collect();
        let languages = project.read(cx).languages().clone();
        let message = cx.new(|cx| Markdown::new(commit.message.clone(), Some(languages), None, cx));
        let remote = repository.read(cx).default_remote_url().and_then(|url| {
            let registry = GitHostingProviderRegistry::default_global(cx);
            let (host, parsed) = parse_git_remote_url(registry, &url)?;
            Some(GitRemote {
                host,
                owner: parsed.owner.into(),
                repo: parsed.repo.into(),
            })
        });

        let mut this = Self {
            commit,
            files,
            file_entries,
            file_stats,
            diff_stats,
            is_shallow_boundary: commit_diff.is_shallow_boundary,
            message,
            message_collapsed: false,
            message_scroll_handle: ScrollHandle::new(),
            remote,
            view_mode: ChangedFilesViewMode::Tree,
            expanded_dirs: HashMap::default(),
            selected: None,
            editor: None,
            loaded_file: None,
            showing_full_file: EditorSettings::get_global(cx).file_diff.show_full_file,
            repository,
            project,
            workspace,
            focus_handle: cx.focus_handle(),
            list_scroll_handle: UniformListScrollHandle::new(),
            _load_file: Task::ready(()),
            // Full file or changes only is one choice for all file diffs.
            _settings_subscription: cx.observe_global::<SettingsStore>(|this, cx| {
                let show_full_file = EditorSettings::get_global(cx).file_diff.show_full_file;
                this.set_showing_full_file(show_full_file, cx);
            }),
        };
        let first_file = this.visible_file_indices().first().copied().unwrap_or(0);
        let selected = file_filter
            .and_then(|path| this.files.iter().position(|file| file.path == path))
            .unwrap_or(first_file);
        this.select_file(selected, window, cx);
        this
    }

    fn rows(&self) -> Vec<FileRow> {
        let index_of = |entry: &ChangedFileEntry| {
            self.files
                .iter()
                .position(|file| file.path == entry.repo_path)
        };
        if self.view_mode.is_tree() {
            build_changed_file_tree_entries(self.file_entries.clone(), &self.expanded_dirs)
                .into_iter()
                .filter_map(|entry| match entry {
                    ChangedFileTreeEntry::Directory(directory) => Some(FileRow::Directory {
                        path: directory.path,
                        name: directory.name,
                        depth: directory.depth,
                        expanded: directory.expanded,
                    }),
                    ChangedFileTreeEntry::File(file) => Some(FileRow::File {
                        index: index_of(&file.entry)?,
                        entry: file.entry,
                        depth: file.depth,
                        directory_label: None,
                    }),
                })
                .collect()
        } else {
            let mut entries: Vec<(usize, &ChangedFileEntry)> =
                self.file_entries.iter().enumerate().collect();
            entries.sort_by_key(|(index, _)| self.files[*index].status());
            entries
                .into_iter()
                .map(|(index, entry)| FileRow::File {
                    index,
                    entry: entry.clone(),
                    depth: 0,
                    directory_label: (!entry.dir_path.is_empty()).then(|| entry.dir_path.clone()),
                })
                .collect()
        }
    }

    /// The files in the order the list shows them, for moving the selection.
    fn visible_file_indices(&self) -> Vec<usize> {
        self.rows()
            .into_iter()
            .filter_map(|row| match row {
                FileRow::File { index, .. } => Some(index),
                FileRow::Directory { .. } => None,
            })
            .collect()
    }

    fn select_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected == Some(index) && self.editor.is_some() {
            return;
        }
        let Some(file) = self.files.get(index) else {
            return;
        };
        self.selected = Some(index);
        if let Some(row) = self.rows().iter().position(
            |row| matches!(row, FileRow::File { index: row_index, .. } if *row_index == index),
        ) {
            self.list_scroll_handle
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();

        let Some(worktree_id) = worktree_id_for_repo_path(
            self.repository.read(cx),
            self.project.read(cx),
            &file.path,
            cx,
        ) else {
            return;
        };
        let path = file.path.clone();
        let is_binary = file.is_binary;
        let is_deleted = file.new_text.is_none();
        let new_text = if is_binary {
            BINARY_PLACEHOLDER.to_string()
        } else {
            file.new_text.clone().unwrap_or_default()
        };
        let old_text = if is_binary {
            None
        } else {
            file.old_text.clone()
        };
        let file_name = path
            .file_name()
            .map(|name| name.to_string())
            .unwrap_or_else(|| path.display(PathStyle::local()).to_string());
        let blob = Arc::new(GitBlob {
            path: path.clone(),
            worktree_id,
            is_deleted,
            is_binary,
            display_name: format!("{} - {file_name}", self.short_sha()),
        }) as Arc<dyn language::File>;
        let language_registry = self.project.read(cx).languages().clone();
        // Keep split or unified as the user left it on the previous file.
        let diff_view_style = self
            .editor
            .as_ref()
            .map(|editor| editor.read(cx).diff_view_style())
            .unwrap_or(EditorSettings::get_global(cx).diff_view_style);

        self._load_file = cx.spawn_in(window, async move |this, cx| {
            let result: Result<()> = async {
                let buffer = build_buffer(new_text, blob, &language_registry, cx).await?;
                let diff = if is_binary {
                    cx.update(|_, cx| {
                        let snapshot = buffer.read(cx).snapshot();
                        cx.new(|cx| {
                            BufferDiff::new_unchanged(
                                &snapshot,
                                snapshot.language().cloned(),
                                Some(language_registry.clone()),
                                cx,
                            )
                        })
                    })?
                } else {
                    build_buffer_diff(old_text, &buffer, &language_registry, cx).await?
                };
                this.update_in(cx, |this, window, cx| {
                    let Some(workspace) = this.workspace.upgrade() else {
                        return;
                    };
                    // Built like a single file diff: no file header above the
                    // file, since the view shows which file it is.
                    let showing_full_file = this.showing_full_file;
                    let multibuffer = cx.new(|cx| {
                        let mut multibuffer = SoloDiffView::build_multibuffer(
                            buffer.clone(),
                            diff.clone(),
                            showing_full_file,
                            cx,
                        );
                        multibuffer.set_all_diff_hunks_expanded(cx);
                        multibuffer
                    });
                    let project = this.project.clone();
                    let editor = cx.new(|cx| {
                        let editor = SplittableEditor::new(
                            diff_view_style,
                            multibuffer,
                            project,
                            workspace,
                            window,
                            cx,
                        );
                        editor.rhs_editor().update(cx, |editor, cx| {
                            editor.set_show_bookmarks(false, cx);
                            editor.set_show_breakpoints(false, cx);
                            editor.set_allow_git_diff_scrollbar_markers(showing_full_file, cx);
                        });
                        editor
                    });
                    this.loaded_file = Some((buffer, diff));
                    this.editor = Some(editor);
                    cx.notify();
                })?;
                Ok(())
            }
            .await;
            result.log_err();
        });
    }

    fn set_showing_full_file(&mut self, showing_full_file: bool, cx: &mut Context<Self>) {
        if self.showing_full_file == showing_full_file {
            return;
        }
        self.showing_full_file = showing_full_file;
        if let (Some(editor), Some((buffer, diff))) = (&self.editor, &self.loaded_file) {
            let (ranges, context_line_count) =
                SoloDiffView::excerpt_ranges(buffer, diff, showing_full_file, cx);
            editor.update(cx, |editor, cx| {
                let path = PathKey::for_buffer(buffer, cx);
                editor.remove_excerpts_for_path(path.clone(), cx);
                editor.update_excerpts_for_path(
                    path,
                    buffer.clone(),
                    ranges,
                    context_line_count,
                    diff.clone(),
                    cx,
                );
                editor.rhs_editor().update(cx, |editor, cx| {
                    editor.set_allow_git_diff_scrollbar_markers(showing_full_file, cx);
                });
            });
        }
        cx.notify();
    }

    fn open_selected_file(&self, window: &mut Window, cx: &mut App) {
        let Some(file) = self.selected.and_then(|index| self.files.get(index)) else {
            return;
        };
        let Some(project_path) = self
            .repository
            .read(cx)
            .repo_path_to_project_path(&file.path, cx)
        else {
            return;
        };
        self.workspace
            .update(cx, |workspace, cx| {
                workspace
                    .open_path(project_path, None, true, window, cx)
                    .detach_and_log_err(cx);
            })
            .log_err();
    }

    fn move_selection(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.visible_file_indices();
        let position = self
            .selected
            .and_then(|selected| order.iter().position(|index| *index == selected));
        let next = match (position, forward) {
            (None, _) => order.first(),
            (Some(position), true) => order.get(position + 1),
            (Some(position), false) => position.checked_sub(1).and_then(|p| order.get(p)),
        };
        if let Some(next) = next.copied() {
            self.select_file(next, window, cx);
        }
    }

    fn select_next(&mut self, _: &menu::SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(true, window, cx);
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(false, window, cx);
    }

    fn short_sha(&self) -> &str {
        self.commit
            .sha
            .get(0..git::SHORT_SHA_LENGTH)
            .unwrap_or(&self.commit.sha)
    }

    fn subject(&self) -> &str {
        self.commit.message.lines().next().unwrap_or_default()
    }

    fn render_copy_button(
        &self,
        id: &'static str,
        text: SharedString,
        icon: IconName,
        tooltip: &'static str,
        copied_tooltip: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let copied_state: Entity<CopiedState> = window.use_keyed_state(id, cx, CopiedState::new);
        let is_copied = copied_state.read(cx).is_copied();
        let (icon, icon_color, tooltip) = if is_copied {
            (IconName::Check, Color::Success, copied_tooltip)
        } else {
            (icon, Color::Muted, tooltip)
        };
        let meta = text.clone();
        Button::new(id, text.clone())
            .start_icon(Icon::new(icon).size(IconSize::Small).color(icon_color))
            .label_size(LabelSize::Small)
            .truncate(true)
            .color(Color::Muted)
            .tooltip(move |_, cx| Tooltip::with_meta(tooltip, None, meta.clone(), cx))
            .on_click(move |_, _, cx| {
                copied_state.update(cx, |state, _| state.mark_copied());
                cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                let state_id = copied_state.entity_id();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(COPIED_STATE_DURATION).await;
                    cx.update(|cx| cx.notify(state_id))
                })
                .detach();
            })
    }

    /// The author, date and links, laid out like the git graph's details panel.
    fn render_metadata(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let full_sha = self.commit.sha.clone();
        let author_email = self.commit.author_email.clone();
        let avatar = CommitAvatar::new(
            &full_sha,
            (!author_email.is_empty()).then(|| author_email.clone()),
            self.remote.as_ref(),
        )
        .size(px(32.))
        .render(window, cx);
        let permalink = self.remote.as_ref().map(|remote| {
            let parsed = ParsedGitRemote {
                owner: remote.owner.as_ref().into(),
                repo: remote.repo.as_ref().into(),
            };
            let url = remote
                .host
                .build_commit_permalink(&parsed, BuildCommitPermalinkParams { sha: &full_sha })
                .to_string();
            (remote.host.name(), url)
        });

        v_flex()
            .p_2()
            .gap_2()
            .child(
                v_flex()
                    .py_1()
                    .w_full()
                    .items_center()
                    .child(avatar)
                    .child(Label::new(self.commit.author_name.clone()).mt_1p5())
                    .child(
                        Label::new(format_timestamp(
                            self.commit.commit_timestamp,
                            GitGraphSettings::get_global(cx).date_style,
                            cx,
                        ))
                            .color(Color::Muted)
                            .size(LabelSize::Small),
                    ),
            )
            .child(
                v_flex()
                    .ml_neg_1()
                    .gap_1p5()
                    .when(!author_email.is_empty(), |this| {
                        this.child(self.render_copy_button(
                            "commit-files-email-copy",
                            author_email.clone(),
                            IconName::Envelope,
                            "Copy Email",
                            "Email Copied!",
                            window,
                            cx,
                        ))
                    })
                    .child(self.render_copy_button(
                        "commit-files-sha-copy",
                        full_sha.clone(),
                        IconName::Hash,
                        "Copy Commit SHA",
                        "Commit SHA Copied!",
                        window,
                        cx,
                    ))
                    .when_some(permalink, |this, (provider_name, url)| {
                        this.child(
                            Button::new("commit-files-view-on-provider", format!("View on {provider_name}"))
                                .start_icon(
                                    Icon::new(ui::git_hosting_provider_icon(provider_name.as_str()))
                                        .size(IconSize::Small)
                                        .color(Color::Muted),
                                )
                                .label_size(LabelSize::Small)
                                .truncate(true)
                                .color(Color::Muted)
                                .tooltip(Tooltip::text("Open In Browser"))
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            )
            .when(self.is_shallow_boundary, |this| {
                this.child(
                    Label::new(
                        "This commit is at the edge of a shallow clone, so its changes may be incomplete.",
                    )
                    .size(LabelSize::Small)
                    .color(Color::Warning),
                )
            })
    }

    fn render_message(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let message_style = editor::hover_markdown_style(window, cx);
        let line_height = message_style
            .base_text_style
            .line_height_in_pixels(window.rem_size());
        let collapsed = self.message_collapsed;

        // A grid rather than a flexbox, so the text reflows to the panel's
        // width, as in the git graph's details panel.
        div()
            .w_full()
            .py_2()
            .pl_2()
            .grid()
            .grid_cols(1)
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Disclosure::new("commit-files-toggle-message", !collapsed).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.message_collapsed = !this.message_collapsed;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Label::new("Message")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .when(!collapsed, |this| {
                this.child(
                    div()
                        .relative()
                        .w_full()
                        .child(
                            div()
                                .id("commit-files-message")
                                .text_sm()
                                .w_full()
                                .max_h(line_height * 12.)
                                .overflow_y_scroll()
                                .track_scroll(&self.message_scroll_handle)
                                .child(MarkdownElement::new(self.message.clone(), message_style)),
                        )
                        .vertical_scrollbar_for(&self.message_scroll_handle, window, cx),
                )
            })
    }

    fn render_files_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let file_count = self.files.len();
        let is_tree = self.view_mode.is_tree();
        h_flex()
            .p_2()
            .pr_3()
            .pb_1()
            .gap_1()
            .w_full()
            .justify_between()
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Label::new(format!(
                            "{file_count} Changed {}",
                            if file_count == 1 { "File" } else { "Files" }
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    )
                    .child(Divider::vertical())
                    .child(
                        IconButton::new("commit-files-toggle-view", IconName::ListTree)
                            .icon_size(IconSize::Small)
                            .toggle_state(is_tree)
                            .tooltip(Tooltip::text(if is_tree {
                                "Show Flat View"
                            } else {
                                "Show Tree View"
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.view_mode = this.view_mode.toggled();
                                this.list_scroll_handle
                                    .scroll_to_item(0, ScrollStrategy::Top);
                                cx.notify();
                            })),
                    ),
            )
            .child(DiffStat::new(
                "commit-files-diff-stat",
                self.diff_stats.0,
                self.diff_stats.1,
            ))
    }

    fn render_file_list(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = Rc::new(self.rows());
        let indent_rows = rows.clone();
        let selected = self.selected;
        let view = cx.weak_entity();
        let mut list_context = KeyContext::default();
        list_context.add("CommitFilesView");
        list_context.add("menu");

        div()
            .id("commit-files-list")
            .flex_1()
            .min_h_0()
            .key_context(list_context)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .child(
                uniform_list(
                    "commit-files-rows",
                    rows.len(),
                    move |range, _window, _cx| {
                        range
                            .map(|ix| match &rows[ix] {
                                FileRow::Directory {
                                    path,
                                    name,
                                    depth,
                                    expanded,
                                } => {
                                    let path = path.clone();
                                    let expanded = *expanded;
                                    let view = view.clone();
                                    ListItem::new(("commit-files-dir", ix))
                                        .spacing(ListItemSpacing::Sparse)
                                        .indent_level(*depth)
                                        .indent_step_size(px(TREE_INDENT))
                                        .start_slot(
                                            Icon::new(if expanded {
                                                IconName::FolderOpen
                                            } else {
                                                IconName::Folder
                                            })
                                            .size(IconSize::Small)
                                            .color(Color::Muted),
                                        )
                                        .child(
                                            Label::new(name.clone())
                                                .size(LabelSize::Small)
                                                .color(Color::Muted)
                                                .truncate(),
                                        )
                                        .on_click(move |_, _, cx| {
                                            view.update(cx, |view, cx| {
                                                view.expanded_dirs.insert(path.clone(), !expanded);
                                                cx.notify();
                                            })
                                            .ok();
                                        })
                                        .into_any_element()
                                }
                                FileRow::File {
                                    index,
                                    entry,
                                    depth,
                                    directory_label,
                                } => {
                                    let index = *index;
                                    let view = view.clone();
                                    ListItem::new(("commit-files-file", ix))
                                        .spacing(ListItemSpacing::Sparse)
                                        .indent_level(*depth)
                                        .indent_step_size(px(TREE_INDENT))
                                        .toggle_state(selected == Some(index))
                                        .start_slot(git_status_icon(entry.status))
                                        .child(
                                            Label::new(entry.file_name.clone())
                                                .size(LabelSize::Small)
                                                .truncate(),
                                        )
                                        .when_some(directory_label.clone(), |this, label| {
                                            this.child(
                                                Label::new(label)
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted)
                                                    .truncate_start(),
                                            )
                                        })
                                        .on_click(move |_, window, cx| {
                                            view.update(cx, |view, cx| {
                                                window.focus(&view.focus_handle, cx);
                                                view.select_file(index, window, cx);
                                            })
                                            .ok();
                                        })
                                        .into_any_element()
                                }
                            })
                            .collect()
                    },
                )
                .when(self.view_mode.is_tree(), |list| {
                    list.with_decoration(
                        ui::indent_guides(px(TREE_INDENT), IndentGuideColors::panel(cx))
                            .with_left_offset(ui::LIST_ITEM_INDENT_GUIDE_LEFT_OFFSET - px(2.))
                            .with_compute_indents_fn(cx.entity(), move |_, range, _, _| {
                                range
                                    .map(|ix| match indent_rows.get(ix) {
                                        Some(FileRow::Directory { depth, .. })
                                        | Some(FileRow::File { depth, .. }) => *depth,
                                        None => 0,
                                    })
                                    .collect()
                            }),
                    )
                })
                .size_full()
                .track_scroll(&self.list_scroll_handle),
            )
            .vertical_scrollbar_for(&self.list_scroll_handle, window, cx)
    }

    /// The selected file's name, its lines added and removed, and the controls
    /// for how its changes are shown.
    fn render_file_header(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let index = self.selected?;
        let entry = self.file_entries.get(index)?;
        let file = self.files.get(index)?;
        let (added, removed) = self.file_stats.get(index).copied().unwrap_or_default();
        let exists_in_project = file.new_text.is_some()
            && self
                .repository
                .read(cx)
                .repo_path_to_project_path(&file.path, cx)
                .is_some();
        let view = cx.weak_entity();
        let workspace = self.workspace.clone();

        Some(
            h_flex()
                .h(px(32.))
                .flex_none()
                .px_2()
                .gap_1p5()
                .border_b_1()
                .border_color(cx.theme().colors().border)
                .child(git_status_icon(entry.status))
                .child(
                    Label::new(file.path.display(PathStyle::local()).to_string())
                        .size(LabelSize::Small)
                        .truncate(),
                )
                .child(DiffStat::new("commit-files-file-diff-stat", added, removed))
                .child(div().flex_1())
                .child(render_file_diff_mode_controls(
                    self.showing_full_file,
                    move |showing_full_file, cx| {
                        view.update(cx, |view, cx| {
                            view.set_showing_full_file(showing_full_file, cx)
                        })
                        .ok();
                        save_show_full_file(showing_full_file, &workspace, cx);
                    },
                ))
                .child(Divider::vertical())
                .children(self.editor.clone().map(DiffStyleControls::new))
                .when(exists_in_project, |this| {
                    this.child(Divider::vertical()).child(
                        IconButton::new("commit-files-open-file", IconName::ArrowUpRight)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Open File in Project"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_selected_file(window, cx)
                            })),
                    )
                }),
        )
    }
}

impl Render for CommitFilesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();

        h_flex()
            .size_full()
            .bg(colors.editor_background)
            .child(
                v_flex()
                    .w(DETAILS_PANEL_WIDTH)
                    .flex_none()
                    .h_full()
                    .border_r_1()
                    .border_color(colors.border)
                    .bg(colors.editor_background)
                    .child(self.render_metadata(window, cx))
                    .child(Divider::horizontal())
                    .child(self.render_message(window, cx))
                    .child(Divider::horizontal())
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .child(self.render_files_header(cx))
                            .child(self.render_file_list(window, cx)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .children(self.render_file_header(cx))
                    .child(div().flex_1().min_h_0().map(|this| {
                        match &self.editor {
                            Some(editor) => this.child(editor.clone()),
                            None => this.child(
                                h_flex()
                                    .size_full()
                                    .justify_center()
                                    .child(Label::new("Loading…").color(Color::Muted)),
                            ),
                        }
                    })),
            )
    }
}

impl EventEmitter<ItemEvent> for CommitFilesView {}

impl Focusable for CommitFilesView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for CommitFilesView {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::GitCommit).color(Color::Muted))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        let subject = truncate_and_trailoff(self.subject(), 20);
        format!("{} — {subject}", self.short_sha()).into()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(format!("{} — {}", self.short_sha(), self.subject()).into())
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(params.detail.unwrap_or_default(), cx))
            .color(params.text_color())
            .into_any_element()
    }
}
