//! A commit shown one file at a time, like VS Code: the commit and its changed
//! files on the left, the selected file in full with its changes on the right.
//! Used instead of [`CommitView`](crate::commit_view::CommitView) when
//! `git_panel.entry_primary_click_action` is `"file_diff"`.
//!
//! Only the selected file gets a buffer and a diff; picking another file
//! replaces them, so a commit with many files costs no more than one file.

use std::{ops::Range, sync::Arc};

use anyhow::Result;
use buffer_diff::BufferDiff;
use editor::{DiffStyleControls, EditorSettings, MultiBuffer, SplittableEditor};
use git::repository::{CommitDetails, CommitFileStatus};
use git::status::{FileStatus, StatusCode, TrackedStatus};
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, IntoElement, KeyContext, ParentElement, Render, ScrollStrategy, Styled,
    Task, UniformListScrollHandle, WeakEntity, Window, uniform_list,
};
use language::{Capability, Point};
use multi_buffer::PathKey;
use project::{
    Project,
    git_store::{CommitFile, Repository},
};
use settings::{DiffViewStyle, GitPanelClickBehavior, Settings};
use ui::{Divider, Tooltip, prelude::*};
use util::{ResultExt as _, paths::PathStyle, truncate_and_trailoff};
use workspace::{
    Item, ItemHandle as _, Workspace,
    item::{ItemEvent, TabContentParams},
};

use crate::{
    commit_view::{GitBlob, build_buffer, build_buffer_diff, worktree_id_for_repo_path},
    git_panel_settings::GitPanelSettings,
    git_status_icon,
};

const FILE_LIST_WIDTH: Pixels = px(280.);
const BINARY_PLACEHOLDER: &str = "(binary file not shown)";

pub struct CommitFilesView {
    commit: CommitDetails,
    files: Vec<CommitFile>,
    is_shallow_boundary: bool,
    selected: Option<usize>,
    editor: Option<Entity<SplittableEditor>>,
    repository: Entity<Repository>,
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    list_scroll_handle: UniformListScrollHandle,
    _load_file: Task<()>,
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
        file_filter: Option<git::repository::RepoPath>,
        window: &mut Window,
        cx: &mut App,
    ) {
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
                        let selected = file_filter
                            .and_then(|path| {
                                commit_diff.files.iter().position(|file| file.path == path)
                            })
                            .unwrap_or(0);
                        let view = cx.new(|cx| {
                            let mut view = Self {
                                commit: commit_details,
                                files: commit_diff.files,
                                is_shallow_boundary: commit_diff.is_shallow_boundary,
                                selected: None,
                                editor: None,
                                repository,
                                project,
                                workspace: workspace_handle,
                                focus_handle: cx.focus_handle(),
                                list_scroll_handle: UniformListScrollHandle::new(),
                                _load_file: Task::ready(()),
                            };
                            view.select_file(selected, window, cx);
                            view
                        });

                        // Opening the same commit again replaces its tab.
                        let pane = workspace.active_pane().clone();
                        pane.update(cx, |pane, cx| {
                            let existing = pane.items().enumerate().find_map(|(index, item)| {
                                let other = item.downcast::<CommitFilesView>()?;
                                (other.read(cx).commit.sha == view.read(cx).commit.sha)
                                    .then(|| (index, other.item_id()))
                            });
                            if let Some((index, item_id)) = existing {
                                pane.remove_item(item_id, false, false, window, cx);
                                pane.add_item(Box::new(view), true, true, Some(index), window, cx);
                            } else {
                                pane.add_item(Box::new(view), true, true, None, window, cx);
                            }
                        });
                    })
                    .log_err()
            })
            .detach();
    }

    fn select_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected == Some(index) && self.editor.is_some() {
            return;
        }
        let Some(file) = self.files.get(index) else {
            return;
        };
        self.selected = Some(index);
        self.list_scroll_handle
            .scroll_to_item(index, ScrollStrategy::Nearest);
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
        let short_sha = self.short_sha().to_string();
        let file_name = path
            .file_name()
            .map(|name| name.to_string())
            .unwrap_or_else(|| path.display(PathStyle::local()).to_string());
        let blob = Arc::new(GitBlob {
            path: path.clone(),
            worktree_id,
            is_deleted,
            is_binary,
            display_name: format!("{short_sha} - {file_name}"),
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
                    let multibuffer = cx.new(|cx| {
                        let mut multibuffer = MultiBuffer::new(Capability::ReadOnly);
                        multibuffer.set_all_diff_hunks_expanded(cx);
                        multibuffer
                    });
                    let project = this.project.clone();
                    let editor = cx.new(|cx| {
                        SplittableEditor::new(
                            diff_view_style,
                            multibuffer,
                            project,
                            workspace,
                            window,
                            cx,
                        )
                    });
                    editor.update(cx, |editor, cx| {
                        let whole_file: Range<Point> =
                            Point::zero()..buffer.read(cx).snapshot().max_point();
                        editor.update_excerpts_for_path(
                            PathKey::with_sort_prefix(0, path.as_ref().clone()),
                            buffer,
                            [whole_file],
                            0,
                            diff,
                            cx,
                        );
                        if diff_view_style == DiffViewStyle::Split {
                            editor.split(window, cx);
                        }
                        editor.rhs_editor().update(cx, |editor, cx| {
                            editor.set_show_bookmarks(false, cx);
                            editor.set_show_breakpoints(false, cx);
                        });
                    });
                    this.editor = Some(editor);
                    cx.notify();
                })?;
                Ok(())
            }
            .await;
            result.log_err();
        });
    }

    fn select_next(&mut self, _: &menu::SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.selected.map_or(0, |selected| selected + 1);
        if next < self.files.len() {
            self.select_file(next, window, cx);
        }
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(previous) = self.selected.and_then(|selected| selected.checked_sub(1)) {
            self.select_file(previous, window, cx);
        }
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

    fn file_status(file: &CommitFile) -> FileStatus {
        let index_status = match file.status() {
            CommitFileStatus::Added => StatusCode::Added,
            CommitFileStatus::Modified => StatusCode::Modified,
            CommitFileStatus::Deleted => StatusCode::Deleted,
        };
        FileStatus::Tracked(TrackedStatus {
            index_status,
            worktree_status: StatusCode::Unmodified,
        })
    }

    fn render_commit_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let commit_date = time::OffsetDateTime::from_unix_timestamp(self.commit.commit_timestamp)
            .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
        let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        let date = time_format::format_localized_timestamp(
            commit_date,
            time::OffsetDateTime::now_utc(),
            local_offset,
            time_format::TimestampFormat::MediumAbsolute,
        );
        let body: SharedString = self
            .commit
            .message
            .split_once('\n')
            .map(|(_, body)| body.trim().to_string())
            .unwrap_or_default()
            .into();
        let sha = self.commit.sha.clone();

        v_flex()
            .p_2()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(Label::new(self.subject().to_string()).weight(FontWeight::SEMIBOLD))
            .when(!body.is_empty(), |this| {
                this.child(
                    div()
                        .id("commit-files-view-body")
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .child(Label::new(body).size(LabelSize::Small).color(Color::Muted)),
                )
            })
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Label::new(self.commit.author_name.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .child(Label::new("·").size(LabelSize::Small).color(Color::Muted))
                    .child(Label::new(date).size(LabelSize::Small).color(Color::Muted)),
            )
            .child(
                Button::new("commit-files-view-sha", self.short_sha().to_string())
                    .label_size(LabelSize::Small)
                    .color(Color::Muted)
                    .start_icon(Icon::new(IconName::Copy).size(IconSize::XSmall))
                    .tooltip(Tooltip::text("Copy Commit SHA"))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(sha.to_string()));
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

    fn render_file_rows(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = cx.theme().colors();
        self.files[range.clone()]
            .iter()
            .zip(range)
            .map(|(file, index)| {
                let is_selected = self.selected == Some(index);
                let file_name: SharedString = file
                    .path
                    .file_name()
                    .map(|name| name.to_string())
                    .unwrap_or_else(|| file.path.display(PathStyle::local()).to_string())
                    .into();
                let directory: Option<SharedString> = file
                    .path
                    .parent()
                    .filter(|parent| !parent.is_empty())
                    .map(|parent| parent.display(PathStyle::local()).to_string().into());

                h_flex()
                    .id(("commit-file", index))
                    .h(px(24.))
                    .w_full()
                    .px_2()
                    .gap_1p5()
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(colors.ghost_element_selected))
                    .hover(|style| style.bg(colors.ghost_element_hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus_handle, cx);
                        this.select_file(index, window, cx);
                    }))
                    .child(git_status_icon(Self::file_status(file)))
                    .child(Label::new(file_name).size(LabelSize::Small).truncate())
                    .children(directory.map(|directory| {
                        Label::new(directory)
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate()
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn render_file_header(&self, cx: &App) -> Option<impl IntoElement> {
        let file = self.files.get(self.selected?)?;
        Some(
            h_flex()
                .h(px(32.))
                .flex_none()
                .px_2()
                .gap_1p5()
                .border_b_1()
                .border_color(cx.theme().colors().border)
                .child(git_status_icon(Self::file_status(file)))
                .child(
                    Label::new(file.path.display(PathStyle::local()).to_string())
                        .size(LabelSize::Small)
                        .truncate(),
                )
                .child(div().flex_1())
                .children(
                    self.editor
                        .clone()
                        .map(|editor| DiffStyleControls::new(editor)),
                ),
        )
    }
}

impl Render for CommitFilesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let mut list_context = KeyContext::default();
        list_context.add("CommitFilesView");
        list_context.add("menu");
        let file_count = self.files.len();

        h_flex()
            .size_full()
            .bg(colors.editor_background)
            .child(
                v_flex()
                    .w(FILE_LIST_WIDTH)
                    .flex_none()
                    .h_full()
                    .border_r_1()
                    .border_color(colors.border)
                    .bg(colors.panel_background)
                    .child(self.render_commit_details(cx))
                    .child(
                        h_flex().px_2().pt_2().pb_1().child(
                            Label::new(if file_count == 1 {
                                "1 changed file".to_string()
                            } else {
                                format!("{file_count} changed files")
                            })
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                        ),
                    )
                    .child(
                        v_flex()
                            .id("commit-files-view-list")
                            .flex_1()
                            .min_h_0()
                            .key_context(list_context)
                            .track_focus(&self.focus_handle)
                            .on_action(cx.listener(Self::select_next))
                            .on_action(cx.listener(Self::select_previous))
                            .child(
                                uniform_list(
                                    "commit-files-view-files",
                                    file_count,
                                    cx.processor(Self::render_file_rows),
                                )
                                .size_full()
                                .track_scroll(&self.list_scroll_handle),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .children(self.render_file_header(cx))
                    .child(Divider::horizontal().color(ui::DividerColor::Border))
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

/// Restores Zed's own git panel defaults, which the git UI tests were written
/// for: all changed files in one view and the classic panel layout.
#[cfg(test)]
pub(crate) fn use_zed_git_panel_defaults(cx: &mut App) {
    cx.update_global::<settings::SettingsStore, _>(|store, cx| {
        store.update_user_settings(cx, |settings| {
            let git_panel = settings.git_panel.get_or_insert_default();
            git_panel.entry_primary_click_action = Some(GitPanelClickBehavior::ProjectDiff);
            git_panel.layout = Some(settings::GitPanelLayout::Classic);
        });
    });
}
