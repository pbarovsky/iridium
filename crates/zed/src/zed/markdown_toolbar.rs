//! Buttons for moving between a Markdown file's preview and its source, like
//! the ones VS Code shows in a Markdown editor's title: in the preview, edit
//! the source in its place or side by side with the preview; in the source,
//! open the preview to the side.

use editor::Editor;
use gpui::{Context, Entity, EventEmitter, Focusable, IntoElement, Render, WeakEntity, Window};
use markdown_preview::{CloseAndReturnToEditor, markdown_preview_view::MarkdownPreviewView};
use ui::{Tooltip, prelude::*};
use workspace::{ItemHandle, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace};

enum ActiveMarkdown {
    Preview(Entity<MarkdownPreviewView>),
    Source(Entity<Editor>),
}

pub struct MarkdownToolbar {
    workspace: WeakEntity<Workspace>,
    active: Option<ActiveMarkdown>,
}

impl MarkdownToolbar {
    pub fn new(workspace: &Workspace) -> Self {
        Self {
            workspace: workspace.weak_handle(),
            active: None,
        }
    }

    /// Shows the source in the preview's place, with the preview moved next to
    /// it, so edits show up in the preview as they're typed.
    fn edit_side_by_side(
        preview: &Entity<MarkdownPreviewView>,
        workspace: &WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(editor) = preview.read(cx).source_editor() else {
            return;
        };
        let workspace = workspace.clone();
        preview
            .focus_handle(cx)
            .dispatch_action(&CloseAndReturnToEditor, window, cx);
        // Returning to the editor is deferred, so open the preview after it.
        window.defer(cx, move |window, cx| {
            window.defer(cx, move |window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        let Some(pane) = workspace.pane_for(&editor) else {
                            return;
                        };
                        MarkdownPreviewView::open_preview_to_the_side_of_pane(
                            workspace, editor, pane, window, cx,
                        );
                    })
                    .ok();
            });
        });
    }
}

impl EventEmitter<ToolbarItemEvent> for MarkdownToolbar {}

impl Render for MarkdownToolbar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let workspace = self.workspace.clone();
        match &self.active {
            Some(ActiveMarkdown::Preview(preview)) => h_flex()
                .gap_1()
                .child(
                    IconButton::new("markdown-edit-source", IconName::Pencil)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text(
                            "Edit (double-click the preview to edit there)",
                        ))
                        .on_click({
                            let preview = preview.clone();
                            move |_, window, cx| {
                                preview.focus_handle(cx).dispatch_action(
                                    &CloseAndReturnToEditor,
                                    window,
                                    cx,
                                );
                            }
                        }),
                )
                .child(
                    IconButton::new("markdown-edit-side-by-side", IconName::SplitAlt)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Edit Side by Side"))
                        .on_click({
                            let preview = preview.clone();
                            move |_, window, cx| {
                                Self::edit_side_by_side(&preview, &workspace, window, cx)
                            }
                        }),
                )
                .into_any_element(),
            Some(ActiveMarkdown::Source(editor)) => {
                IconButton::new("markdown-preview-to-the-side", IconName::SplitAlt)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Open Preview to the Side"))
                    .on_click({
                        let editor = editor.clone();
                        move |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    let Some(pane) = workspace.pane_for(&editor) else {
                                        return;
                                    };
                                    MarkdownPreviewView::open_preview_to_the_side_of_pane(
                                        workspace,
                                        editor.clone(),
                                        pane,
                                        window,
                                        cx,
                                    );
                                })
                                .ok();
                        }
                    })
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}

impl ToolbarItemView for MarkdownToolbar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        self.active = active_pane_item.and_then(|item| {
            if let Some(preview) = item.downcast::<MarkdownPreviewView>() {
                return Some(ActiveMarkdown::Preview(preview));
            }
            let editor = item.act_as::<Editor>(cx)?;
            MarkdownPreviewView::is_markdown_file(&editor, cx)
                .then_some(ActiveMarkdown::Source(editor))
        });
        cx.notify();
        if self.active.is_some() {
            ToolbarItemLocation::PrimaryRight
        } else {
            ToolbarItemLocation::Hidden
        }
    }
}
