//! Quick Open, like VS Code's: one picker that opens over the title bar's
//! command center and searches files, or by the query's first character
//! commands (`>`), symbols in the file (`@`), symbols in the project (`#`) or
//! a line (`:`). Erasing the prefix goes back to files.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{self, AtomicBool},
    },
};

use command_palette::humanize_action_name;
use command_palette_hooks::CommandPaletteFilter;
use editor::{Anchor, Editor, SelectionEffects, scroll::Autoscroll};
use file_icons::FileIcons;
use fuzzy_nucleo::{
    PathMatchOptions, StringMatch, StringMatchCandidate, StringMatchOptions, match_path_sets,
    match_strings,
};
use gpui::{
    Action, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, MouseButton,
    Pixels, Render, Task, WeakEntity, Window, px,
};
use language::{OutlineItem, Point};
use picker::{Picker, PickerDelegate};
use project::{PathMatchCandidateSet, Project, ProjectPath, WorktreeId};
use ui::{HighlightedLabel, KeyBinding, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt as _;
use workspace::{ModalView, Workspace};
use zed_actions::quick_open::Toggle;

const MAX_RECENT_FILES: usize = 20;
const MAX_MATCHES: usize = 100;
/// How far below the command center's top the picker opens, so its input
/// sits where the command center's box was.
const TOP_OFFSET: f32 = 0.;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, action: &Toggle, window, cx| {
            let query = action.query.clone().unwrap_or_default();
            QuickOpen::toggle(workspace, query, window, cx);
        });
    })
    .detach();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Files,
    Commands,
    FileSymbols,
    ProjectSymbols,
    Line,
    Text,
    Help,
}

impl Mode {
    /// The mode a query is for, and the query without its prefix.
    fn parse(query: &str) -> (Self, &str) {
        let query = query.trim_start();
        let mut chars = query.chars();
        let mode = match chars.next() {
            Some('>') => Self::Commands,
            Some('@') => Self::FileSymbols,
            Some('#') => Self::ProjectSymbols,
            Some(':') => Self::Line,
            Some('?') => Self::Help,
            Some('%') => Self::Text,
            _ => return (Self::Files, query.trim()),
        };
        (mode, chars.as_str().trim())
    }
}

pub struct QuickOpen {
    picker: Entity<Picker<QuickOpenDelegate>>,
}

impl QuickOpen {
    fn toggle(
        workspace: &mut Workspace,
        query: String,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        if let Some(quick_open) = workspace.active_modal::<Self>(cx) {
            // Toggling again with another prefix switches modes in place.
            if query.is_empty() {
                workspace.hide_modal(window, cx);
            } else {
                quick_open.update(cx, |quick_open, cx| {
                    quick_open
                        .picker
                        .update(cx, |picker, cx| picker.set_query(&query, window, cx))
                });
            }
            return;
        }
        // With nothing focused (no file open), commands run on the workspace.
        let previous_focus = window
            .focused(cx)
            .unwrap_or_else(|| workspace.focus_handle(cx));
        let project = workspace.project().clone();
        let weak_workspace = cx.entity().downgrade();
        let active_editor = workspace
            .active_item(cx)
            .and_then(|item| item.act_as::<Editor>(cx));
        let recent_files = workspace
            .recent_navigation_history(Some(MAX_RECENT_FILES), cx)
            .into_iter()
            .map(|(project_path, _)| project_path)
            .collect();
        // The available commands depend on the focus, so collect them before
        // Quick Open takes it.
        let filter = CommandPaletteFilter::try_global(cx);
        let mut commands: Vec<Command> = window
            .available_actions(cx)
            .into_iter()
            .filter(|action| !filter.is_some_and(|filter| filter.is_hidden(&**action)))
            .map(|action| Command {
                name: humanize_action_name(action.name()).into(),
                action,
            })
            .collect();
        commands.sort_by(|a, b| a.name.cmp(&b.name));

        workspace.toggle_modal(window, cx, move |window, cx| {
            let delegate = QuickOpenDelegate {
                quick_open: cx.entity().downgrade(),
                workspace: weak_workspace,
                project,
                previous_focus,
                active_editor: active_editor.clone(),
                recent_files,
                commands,
                symbols: Vec::new(),
                project_symbols: Vec::new(),
                file_line: None,
                matches: Vec::new(),
                separator_after: None,
                selected_index: 0,
                cancel_flag: Arc::new(AtomicBool::new(false)),
            };
            let picker = cx.new(|cx| {
                let picker = Picker::uniform_list(delegate, window, cx)
                    .initial_width(rems(quick_open_width(window, cx) / window.rem_size()))
                    .resizable(false)
                    .show_scrollbar(true);
                picker.set_query(&query, window, cx);
                picker
            });
            if let Some(editor) = active_editor {
                load_symbols(&picker, &editor, window, cx);
            }
            Self { picker }
        });
        workspace::quick_open::set_quick_open_shown(true, window, cx);
    }
}

/// Loads the active file's symbols for `@`, which can take a moment.
fn load_symbols(
    picker: &Entity<Picker<QuickOpenDelegate>>,
    editor: &Entity<Editor>,
    window: &mut Window,
    cx: &mut App,
) {
    let multibuffer = editor.read(cx).buffer().read(cx).snapshot(cx);
    let Some(buffer) = multibuffer.as_singleton() else {
        return;
    };
    let buffer_id = buffer.remote_id();
    let items = editor.update(cx, |editor, cx| editor.buffer_outline_items(buffer_id, cx));
    let picker = picker.downgrade();
    window
        .spawn(cx, async move |cx| {
            let symbols = items
                .await
                .into_iter()
                .filter_map(|item: OutlineItem<language::Anchor>| {
                    Some(Symbol {
                        name: item.text.into(),
                        depth: item.depth,
                        position: multibuffer.anchor_in_buffer(item.range.start)?,
                    })
                })
                .collect();
            picker
                .update_in(cx, |picker, window, cx| {
                    picker.delegate.symbols = symbols;
                    if Mode::parse(&picker.query(cx)).0 == Mode::FileSymbols {
                        picker.refresh(window, cx);
                    }
                })
                .ok();
        })
        .detach();
}

impl EventEmitter<DismissEvent> for QuickOpen {}

impl Focusable for QuickOpen {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl ModalView for QuickOpen {
    // Positioned over the command center rather than where modals go.
    fn render_bare(&self) -> bool {
        true
    }

    fn on_before_dismiss(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> workspace::DismissDecision {
        workspace::quick_open::set_quick_open_shown(false, window, cx);
        workspace::DismissDecision::Dismiss(true)
    }
}

impl Render for QuickOpen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let width = quick_open_width(window, cx);
        let (left, top) = match workspace::quick_open::command_center_bounds(window, cx) {
            Some(bounds) => {
                let center = bounds.center().x;
                let left = (center - width / 2.)
                    .max(px(8.))
                    .min(viewport.width - width - px(8.));
                (left, bounds.top() + px(TOP_OFFSET))
            }
            None => ((viewport.width - width) / 2., px(8.)),
        };
        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(DismissEvent)),
            )
            .child(
                div()
                    .absolute()
                    .left(left)
                    .top(top)
                    .w(width)
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.picker.clone()),
            )
    }
}

struct Command {
    name: SharedString,
    action: Box<dyn Action>,
}

struct Symbol {
    name: SharedString,
    depth: usize,
    position: Anchor,
}

enum Match {
    File {
        project_path: ProjectPath,
        name: SharedString,
        name_positions: Vec<usize>,
        directory: SharedString,
    },
    Command(StringMatch),
    Symbol(StringMatch),
    ProjectSymbol(StringMatch),
    Line {
        row: u32,
        column: Option<u32>,
    },
    /// One of [`HELP`]: a prefix to switch to, or an action to run.
    Help(usize),
    /// Searches the project for text, from `%`.
    Text(String),
    /// A hint, not something to pick.
    Hint(SharedString),
}

/// What Quick Open offers on an empty query and for `?`, like VS Code's
/// command center: each mode with the shortcut of Zed's own picker for it.
struct HelpEntry {
    /// The prefix to switch to, or `None` to run `action`.
    prefix: Option<&'static str>,
    description: &'static str,
    /// Whose shortcut is shown, and what runs when there's no prefix.
    action: fn() -> Box<dyn Action>,
}

const HELP: [HelpEntry; 8] = [
    HelpEntry {
        prefix: Some(""),
        description: "Go to File",
        action: || Box::new(workspace::ToggleFileFinder::default()),
    },
    HelpEntry {
        prefix: Some(">"),
        description: "Show and Run Commands",
        action: || Box::new(zed_actions::command_palette::Toggle),
    },
    HelpEntry {
        prefix: Some("%"),
        description: "Search for Text",
        action: || Box::new(workspace::DeploySearch::default()),
    },
    HelpEntry {
        prefix: Some("@"),
        description: "Go to Symbol in Editor",
        action: || Box::new(zed_actions::outline::ToggleOutline),
    },
    HelpEntry {
        prefix: Some("#"),
        description: "Go to Symbol in Workspace",
        action: || Box::new(workspace::ToggleProjectSymbols),
    },
    HelpEntry {
        prefix: Some(":"),
        description: "Go to Line/Column",
        action: || Box::new(editor::actions::ToggleGoToLine),
    },
    HelpEntry {
        prefix: None,
        description: "Run Task",
        action: || Box::new(zed_actions::Spawn::modal()),
    },
    HelpEntry {
        prefix: Some("?"),
        description: "More",
        action: || Box::new(Toggle::default()),
    },
];

/// A query for a file, and the line (and column) to open it at, as in
/// `src/main.rs:12:5`.
fn parse_file_query(query: &str) -> (&str, Option<(u32, Option<u32>)>) {
    let mut parts = query.rsplitn(3, ':');
    let last = parts.next().and_then(|part| part.parse::<u32>().ok());
    let middle = parts.next();
    let rest = parts.next();
    match (last, middle, rest) {
        (Some(column), Some(middle), Some(path)) => match middle.parse::<u32>() {
            Ok(row) => (path, Some((row, Some(column)))),
            Err(_) => (
                query.rsplit_once(':').map_or(query, |(path, _)| path),
                Some((column, None)),
            ),
        },
        (Some(row), Some(path), None) => (path, Some((row, None))),
        _ => (query, None),
    }
}

struct QuickOpenDelegate {
    quick_open: WeakEntity<QuickOpen>,
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    previous_focus: FocusHandle,
    active_editor: Option<Entity<Editor>>,
    recent_files: Vec<ProjectPath>,
    commands: Vec<Command>,
    symbols: Vec<Symbol>,
    project_symbols: Vec<project::Symbol>,
    /// The line to open the picked file at, from a `path:line` query.
    file_line: Option<(u32, Option<u32>)>,
    matches: Vec<Match>,
    /// Draws a line between the help entries and the recent files.
    separator_after: Option<usize>,
    selected_index: usize,
    cancel_flag: Arc<AtomicBool>,
}

impl QuickOpenDelegate {
    fn file_match(&self, project_path: ProjectPath, full_path: &str, positions: &[usize]) -> Match {
        let name_start = full_path.rfind('/').map_or(0, |slash| slash + 1);
        Match::File {
            name: full_path[name_start..].to_string().into(),
            name_positions: positions
                .iter()
                .filter(|&&position| position >= name_start)
                .map(|position| position - name_start)
                .collect(),
            directory: full_path[..name_start.saturating_sub(1)].to_string().into(),
            project_path,
        }
    }

    fn recent_file_matches(&self, cx: &App) -> Vec<Match> {
        let project = self.project.read(cx);
        self.recent_files
            .iter()
            .filter_map(|project_path| {
                let worktree = project.worktree_for_id(project_path.worktree_id, cx)?;
                let root = worktree.read(cx).root_name_str().to_string();
                let full_path = if project.visible_worktrees(cx).count() > 1 {
                    format!("{root}/{}", project_path.path.as_unix_str())
                } else {
                    project_path.path.as_unix_str().to_string()
                };
                Some(self.file_match(project_path.clone(), &full_path, &[]))
            })
            .collect()
    }

    fn search_files(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let project = self.project.read(cx);
        let worktrees: Vec<_> = project.visible_worktrees(cx).collect();
        let include_root_name = worktrees.len() > 1;
        let candidate_sets: Vec<_> = worktrees
            .iter()
            .map(|worktree| {
                let worktree = worktree.read(cx);
                PathMatchCandidateSet {
                    snapshot: worktree.snapshot(),
                    include_ignored: false,
                    include_root_name,
                    candidates: project::Candidates::Files,
                }
            })
            .collect();
        self.cancel_flag.store(true, atomic::Ordering::Release);
        self.cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_flag = self.cancel_flag.clone();
        cx.spawn_in(window, async move |picker, cx| {
            let matches = match_path_sets(
                candidate_sets.as_slice(),
                &query,
                &None,
                PathMatchOptions {
                    max_results: fuzzy_nucleo::MaxResults::limit::<MAX_MATCHES>(),
                    cancel_flag: Some(&cancel_flag),
                    ..Default::default()
                },
                cx.background_executor().clone(),
            )
            .await;
            if cancel_flag.load(atomic::Ordering::Acquire) {
                return;
            }
            picker
                .update(cx, |picker, cx| {
                    let delegate = &mut picker.delegate;
                    delegate.matches = matches
                        .into_iter()
                        .map(|path_match| {
                            let full_path = path_match.path_prefix.join(&path_match.path);
                            delegate.file_match(
                                ProjectPath {
                                    worktree_id: WorktreeId::from_usize(path_match.worktree_id),
                                    path: path_match.path.clone(),
                                },
                                full_path.as_unix_str(),
                                &path_match.positions,
                            )
                        })
                        .collect();
                    delegate.selected_index = 0;
                    cx.notify();
                })
                .ok();
        })
    }

    fn search_project_symbols(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let symbols = self
            .project
            .update(cx, |project, cx| project.symbols(&query, cx));
        cx.spawn_in(window, async move |picker, cx| {
            let symbols = symbols.await.log_err().unwrap_or_default();
            picker
                .update(cx, |picker, cx| {
                    let delegate = &mut picker.delegate;
                    let names: Vec<SharedString> = symbols
                        .iter()
                        .map(|symbol| symbol.name.clone().into())
                        .collect();
                    let matches = Self::match_names(names.iter(), &query);
                    delegate.project_symbols = symbols;
                    delegate.matches = if matches.is_empty() {
                        vec![Match::Hint("No symbols found".into())]
                    } else {
                        matches.into_iter().map(Match::ProjectSymbol).collect()
                    };
                    delegate.selected_index = 0;
                    cx.notify();
                })
                .ok();
        })
    }

    fn open_project_symbol(
        &self,
        symbol: project::Symbol,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let buffer = self.project.update(cx, |project, cx| {
            project.open_buffer_for_symbol(&symbol, cx)
        });
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            let buffer = buffer.await?;
            workspace.update_in(cx, |workspace, window, cx| {
                let position = buffer
                    .read(cx)
                    .clip_point_utf16(symbol.range.start, language::Bias::Left);
                let editor = workspace
                    .open_project_item::<Editor>(None, buffer, true, true, true, true, window, cx);
                editor.update(cx, |editor, cx| {
                    let snapshot = editor.buffer().read(cx).snapshot(cx);
                    let Some(buffer_snapshot) = snapshot.as_singleton() else {
                        return;
                    };
                    let Some(anchor) =
                        snapshot.anchor_in_buffer(buffer_snapshot.anchor_before(position))
                    else {
                        return;
                    };
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()).unfold(),
                        window,
                        cx,
                        |selections| selections.select_anchor_ranges([anchor..anchor]),
                    );
                });
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn match_names<'a>(
        names: impl Iterator<Item = &'a SharedString>,
        query: &str,
    ) -> Vec<StringMatch> {
        let candidates: Vec<_> = names
            .enumerate()
            .map(|(id, name)| StringMatchCandidate::new(id, name.clone()))
            .collect();
        match_strings(
            &candidates,
            query,
            StringMatchOptions {
                max_results: fuzzy_nucleo::MaxResults::limit::<MAX_MATCHES>(),
                ..Default::default()
            },
        )
    }

    fn dismiss(&self, cx: &mut Context<Picker<Self>>) {
        self.quick_open
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }
}

impl PickerDelegate for QuickOpenDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "QuickOpen"
    }

    fn separators_after_indices(&self) -> Vec<usize> {
        self.separator_after.into_iter().collect()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = ix;
    }

    fn can_select(&self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) -> bool {
        !matches!(self.matches.get(ix), Some(Match::Hint(_)))
    }

    fn placeholder_text(&self, _: &mut Window, _: &mut App) -> Arc<str> {
        "Search files by name (append : to go to a line, ? for help)".into()
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let (mode, query) = Mode::parse(&query);
        self.selected_index = 0;
        self.file_line = None;
        let (file_query, file_line) = parse_file_query(query);
        self.separator_after = None;
        self.matches = match mode {
            Mode::Files if query.is_empty() => {
                // Like VS Code's command center: the modes first, then the
                // recently opened files.
                let recent = self.recent_file_matches(cx);
                if !recent.is_empty() {
                    self.separator_after = Some(HELP.len() - 1);
                }
                (0..HELP.len()).map(Match::Help).chain(recent).collect()
            }
            Mode::Text if query.is_empty() => {
                vec![Match::Hint("Type text to search for in the project".into())]
            }
            Mode::Text => vec![Match::Text(query.to_string())],
            Mode::Files => {
                self.file_line = file_line;
                return self.search_files(file_query.to_string(), window, cx);
            }
            Mode::Help => (0..HELP.len()).map(Match::Help).collect(),
            Mode::Commands => Self::match_names(self.commands.iter().map(|c| &c.name), query)
                .into_iter()
                .map(Match::Command)
                .collect(),
            Mode::FileSymbols if self.active_editor.is_none() => {
                vec![Match::Hint("Open a file to go to its symbols".into())]
            }
            Mode::FileSymbols => {
                let symbols = Self::match_names(self.symbols.iter().map(|s| &s.name), query);
                if symbols.is_empty() {
                    vec![Match::Hint("No symbols".into())]
                } else {
                    symbols.into_iter().map(Match::Symbol).collect()
                }
            }
            Mode::ProjectSymbols if query.is_empty() => {
                vec![Match::Hint("Type to search symbols in the project".into())]
            }
            Mode::ProjectSymbols => {
                return self.search_project_symbols(query.to_string(), window, cx);
            }
            Mode::Line if self.active_editor.is_none() => {
                vec![Match::Hint("Open a file to go to a line".into())]
            }
            Mode::Line => {
                let mut parts = query.split(':');
                let row = parts.next().and_then(|row| row.trim().parse::<u32>().ok());
                let column = parts
                    .next()
                    .and_then(|column| column.trim().parse::<u32>().ok());
                match row {
                    Some(row) => vec![Match::Line { row, column }],
                    None => vec![Match::Hint("Type a line number, or line:column".into())],
                }
            }
        };
        Task::ready(())
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(selected) = self.matches.get(self.selected_index) else {
            return;
        };
        match selected {
            Match::File { project_path, .. } => {
                let project_path = project_path.clone();
                let line = self.file_line;
                self.dismiss(cx);
                let Some(open) = self
                    .workspace
                    .update(cx, |workspace, cx| {
                        workspace.open_path(project_path, None, true, window, cx)
                    })
                    .log_err()
                else {
                    return;
                };
                window
                    .spawn(cx, async move |cx| {
                        let item = open.await?;
                        let (Some((row, column)), Some(editor)) =
                            (line, cx.update(|_, cx| item.act_as::<Editor>(cx))?)
                        else {
                            return anyhow::Ok(());
                        };
                        cx.update(|window, cx| go_to_line(&editor, row, column, window, cx))?;
                        anyhow::Ok(())
                    })
                    .detach_and_log_err(cx);
            }
            Match::ProjectSymbol(symbol) => {
                let Some(symbol) = self.project_symbols.get(symbol.candidate_id).cloned() else {
                    return;
                };
                self.dismiss(cx);
                self.open_project_symbol(symbol, window, cx);
            }
            Match::Help(index) => {
                // Prefixes are handled by `confirm_update_query`.
                let Some(entry) = HELP.get(*index).filter(|entry| entry.prefix.is_none()) else {
                    return;
                };
                window.focus(&self.previous_focus, cx);
                self.dismiss(cx);
                window.dispatch_action((entry.action)(), cx);
            }
            Match::Text(text) => {
                let action = Box::new(workspace::DeploySearch {
                    query: Some(text.clone()),
                    ..Default::default()
                });
                window.focus(&self.previous_focus, cx);
                self.dismiss(cx);
                window.dispatch_action(action, cx);
            }
            Match::Command(command) => {
                let Some(command) = self.commands.get(command.candidate_id) else {
                    return;
                };
                let action = command.action.boxed_clone();
                window.focus(&self.previous_focus, cx);
                self.dismiss(cx);
                window.dispatch_action(action, cx);
            }
            Match::Symbol(symbol) => {
                let (Some(symbol), Some(editor)) =
                    (self.symbols.get(symbol.candidate_id), &self.active_editor)
                else {
                    return;
                };
                let position = symbol.position;
                let editor = editor.clone();
                self.dismiss(cx);
                editor.update(cx, |editor, cx| {
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()),
                        window,
                        cx,
                        |selections| selections.select_anchor_ranges([position..position]),
                    );
                    window.focus(&editor.focus_handle(cx), cx);
                });
            }
            Match::Line { row, column } => {
                let Some(editor) = self.active_editor.clone() else {
                    return;
                };
                let (row, column) = (*row, *column);
                self.dismiss(cx);
                go_to_line(&editor, row, column, window, cx);
            }
            Match::Hint(_) => {}
        }
    }

    fn confirm_update_query(
        &mut self,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<String> {
        match self.matches.get(self.selected_index)? {
            Match::Help(index) => HELP.get(*index)?.prefix.map(str::to_string),
            _ => None,
        }
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.dismiss(cx);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let item = ListItem::new(ix)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected);
        Some(match self.matches.get(ix)? {
            Match::File {
                name,
                name_positions,
                directory,
                ..
            } => item
                .start_slot::<Icon>(
                    FileIcons::get_icon(Path::new(name.as_ref()), cx)
                        .map(|icon| Icon::from_path(icon).color(Color::Muted)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(HighlightedLabel::new(name.clone(), name_positions.clone()))
                        .child(
                            Label::new(directory.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate(),
                        ),
                ),
            Match::Command(command_match) => {
                let command = self.commands.get(command_match.candidate_id)?;
                item.child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .child(HighlightedLabel::new(
                            command.name.clone(),
                            command_match.positions.clone(),
                        ))
                        .child(KeyBinding::for_action_in(
                            &*command.action,
                            &self.previous_focus,
                            cx,
                        )),
                )
            }
            Match::Symbol(symbol_match) => {
                let symbol = self.symbols.get(symbol_match.candidate_id)?;
                item.child(
                    h_flex()
                        .pl(rems(symbol.depth as f32))
                        .child(HighlightedLabel::new(
                            symbol.name.clone(),
                            symbol_match.positions.clone(),
                        )),
                )
            }
            Match::ProjectSymbol(symbol_match) => {
                let symbol = self.project_symbols.get(symbol_match.candidate_id)?;
                let location: SharedString = match &symbol.path {
                    project::lsp_store::SymbolLocation::InProject(project_path) => {
                        project_path.path.as_unix_str().to_string().into()
                    }
                    project::lsp_store::SymbolLocation::OutsideProject { abs_path, .. } => {
                        abs_path.to_string_lossy().to_string().into()
                    }
                };
                item.child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(HighlightedLabel::new(
                            symbol.name.clone(),
                            symbol_match.positions.clone(),
                        ))
                        .child(
                            Label::new(location)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate(),
                        ),
                )
            }
            Match::Help(index) => {
                let entry = HELP.get(*index)?;
                item.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(Label::new(entry.description))
                        .children(
                            entry
                                .prefix
                                .filter(|prefix| !prefix.is_empty())
                                .map(|prefix| Label::new(prefix).color(Color::Muted)),
                        )
                        .child(div().flex_1())
                        .child(KeyBinding::for_action_in(
                            &*(entry.action)(),
                            &self.previous_focus,
                            cx,
                        )),
                )
            }
            Match::Text(text) => {
                item.child(Label::new(format!("Search for “{text}” in the project")))
            }
            Match::Line { row, column } => item.child(Label::new(match column {
                Some(column) => format!("Go to line {row}, column {column}"),
                None => format!("Go to line {row}"),
            })),
            Match::Hint(hint) => item.child(Label::new(hint.clone()).color(Color::Muted)),
        })
    }
}

fn go_to_line(
    editor: &Entity<Editor>,
    row: u32,
    column: Option<u32>,
    window: &mut Window,
    cx: &mut App,
) {
    let point = Point::new(row.saturating_sub(1), column.unwrap_or(1).saturating_sub(1));
    editor.update(cx, |editor, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let point = snapshot.clip_point(point, language::Bias::Left);
        editor.change_selections(
            SelectionEffects::scroll(Autoscroll::center()),
            window,
            cx,
            |selections| selections.select_ranges([point..point]),
        );
        window.focus(&editor.focus_handle(cx), cx);
    });
}

/// As wide as the command center it opens over, like VS Code's, but never
/// too narrow to read.
fn quick_open_width(window: &Window, cx: &App) -> Pixels {
    const DEFAULT_WIDTH: f32 = 600.;
    const MIN_WIDTH: f32 = 360.;
    let width = workspace::quick_open::command_center_bounds(window, cx)
        .map_or(px(DEFAULT_WIDTH), |bounds| bounds.size.width)
        .max(px(MIN_WIDTH));
    width.min(window.viewport_size().width - px(16.))
}
