//! Lists to pick from by typing: every command (Ctrl+Shift+A), recent tasks
//! and recent files (Ctrl+E, again for the other list).

use std::{path::PathBuf, rc::Rc};

use gpui_kit::component::{
    ActiveTheme as _, IndexPath, WindowExt as _,
    command::{Command, CommandItem, CommandState},
    h_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{task, workspace::Workspace};

actions!(ideae, [CommandPalette, RecentPlaces, NextWaiting]);

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct Entry {
    label: SharedString,
    detail: SharedString,
    keys: SharedString,
    run: Run,
    /// The agent's status, as the sidebar marks it.
    mark: Option<(&'static str, Hsla)>,
    /// Paused or archived: in grey, as the sidebar has a paused task.
    dim: bool,
    /// An archived task: shown only for a query, after the rest.
    archived: bool,
    /// The list has a column for `mark`.
    marked: bool,
}

impl Entry {
    fn new(label: impl Into<SharedString>, run: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self {
            label: label.into(),
            detail: SharedString::default(),
            keys: SharedString::default(),
            run: Rc::new(run),
            mark: None,
            dim: false,
            archived: false,
            marked: false,
        }
    }

    /// Runs a shortcut's action the way its keys would, from where focus is,
    /// with its keys as Settings has them.
    fn shortcut(label: &str, id: &str) -> Self {
        let shortcut = crate::keymap::find(id).expect("a shortcut in keymap.rs");
        let action: Rc<dyn Action> = Rc::from(shortcut.action());
        let mut entry = Self::new(label.to_string(), move |window, cx| window.dispatch_action(action.boxed_clone(), cx));
        entry.keys = crate::keymap::label(id).into();
        entry
    }

    fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = detail.into();
        self
    }

    fn keys(mut self, keys: String) -> Self {
        self.keys = keys.into();
        self
    }
}

struct Palette {
    entries: Rc<Vec<Entry>>,
    /// Indexes into `entries` that pass the query, best first.
    shown: Rc<Vec<usize>>,
    query: String,
}

impl Palette {
    /// Entries that came later, as the archived tasks read in the background.
    fn add(&mut self, more: Vec<Entry>, cx: &mut Context<Self>) {
        let mut entries = (*self.entries).clone();
        entries.extend(more);
        self.entries = Rc::new(entries);
        let query = std::mem::take(&mut self.query);
        self.query(&query, cx);
    }

    fn query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.query = query.to_string();
        let query: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
        let shown = if query.is_empty() {
            (0..self.entries.len()).filter(|&ix| !self.entries[ix].archived).collect()
        } else {
            let mut scored: Vec<(i64, usize)> = self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(ix, e)| {
                    let by_label = crate::finder::score(&e.label, &query);
                    let by_detail = crate::finder::score(&format!("{} {}", e.label, e.detail), &query).map(|s| s - 1000);
                    by_label.max(by_detail).map(|s| (s, ix))
                })
                .collect();
            let archived = |ix: usize| self.entries[ix].archived;
            scored.sort_by(|a, b| archived(a.1).cmp(&archived(b.1)).then(b.0.cmp(&a.0)).then(a.1.cmp(&b.1)));
            scored.into_iter().map(|(_, ix)| ix).collect()
        };
        self.shown = Rc::new(shown);
        cx.notify();
        cx.refresh_windows();
    }
}

/// The two lists Ctrl+E goes between.
#[derive(Clone, Copy, PartialEq)]
enum Recent {
    Tasks,
    Files,
}

/// Opens a list; with `recent`, which of the two it is and how to open the
/// other, which Ctrl+E and a click on its tab do.
fn open(placeholder: &str, entries: Vec<Entry>, recent: Option<(Recent, Run)>, window: &mut Window, cx: &mut App) -> Entity<Palette> {
    let again = recent.as_ref().map(|(_, run)| run.clone());
    let tab_shown = recent.as_ref().map(|(which, _)| *which);
    let command = cx.new(|cx| CommandState::new(window, cx));
    let palette = cx.new(|_| Palette { entries: Rc::new(entries), shown: Rc::default(), query: String::new() });
    palette.update(cx, |p, cx| p.query("", cx));
    let placeholder: SharedString = placeholder.to_string().into();
    let focused = Rc::new(std::cell::Cell::new(false));
    let opened = palette.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let (palette, command, focused, placeholder, again) =
            (palette.clone(), command.clone(), focused.clone(), placeholder.clone(), again.clone());
        dialog.close_button(false).p_0().w(px(640.)).content(move |content, window, cx| {
            if !focused.replace(true) {
                let command = command.clone();
                window.defer(cx, move |window, cx| command.read(cx).focus_handle(cx).focus(window, cx));
            }
            let (entries, shown) = {
                let p = palette.read(cx);
                (p.entries.clone(), p.shown.clone())
            };
            let items = shown.iter().map(|&ix| item(&entries[ix]));
            let (query_owner, confirm_owner) = (palette.clone(), palette.clone());
            let again = again.clone();
            let tabs_again = again.clone();
            let mut list = Command::new(&command)
                .bordered(false)
                .filterable(false)
                .placeholder(placeholder.clone())
                // The list's height follows what it holds, up to this.
                .max_h(px(360.))
                .items(items);
            if let Some(tab_shown) = tab_shown {
                list = list.header(move |_, _, cx| {
                    let tab = |which: Recent, label: &'static str, cx: &App| {
                        let again = tabs_again.clone();
                        let current = which == tab_shown;
                        div()
                            .id(label)
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_sm()
                            .when(current, |d| d.bg(cx.theme().accent).text_color(cx.theme().foreground))
                            .when(!current, |d| {
                                d.text_color(cx.theme().muted_foreground).cursor_pointer().hover(|d| d.text_color(cx.theme().foreground))
                            })
                            .child(label)
                            .when(!current, |d| {
                                d.on_click(move |_, window, cx| {
                                    if let Some(again) = &again {
                                        window.close_dialog(cx);
                                        again(window, cx);
                                    }
                                })
                            })
                    };
                    h_flex()
                        .gap_1()
                        .px_2()
                        .py_1p5()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(tab(Recent::Tasks, "Recent tasks", cx))
                        .child(tab(Recent::Files, "Recent files", cx))
                        .child(div().flex_1())
                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} switches", crate::keymap::label("recent"))))
                });
            }
            content.child(
                div()
                    .on_action(move |_: &RecentPlaces, window, cx| {
                        if let Some(again) = &again {
                            window.close_dialog(cx);
                            again(window, cx);
                        }
                    })
                    .child(
                        list.on_query(move |query, _, cx| {
                                let _ = query_owner.update(cx, |p, cx| p.query(query, cx));
                            })
                            .on_confirm(move |index: IndexPath, window, cx| {
                                let run = {
                                    let p = confirm_owner.read(cx);
                                    p.shown.get(index.row).map(|&ix| p.entries[ix].run.clone())
                                };
                                if let Some(run) = run {
                                    window.close_dialog(cx);
                                    run(window, cx);
                                }
                            }),
                    ),
            )
        })
    });
    opened
}

fn item(entry: &Entry) -> CommandItem {
    let entry = entry.clone();
    CommandItem::new().label(entry.label.clone()).child(move |_, cx| {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .gap_3()
            .min_w_0()
            .w_full()
            .when(entry.marked, |row| {
                row.child(
                    div()
                        .flex_none()
                        .w(px(10.))
                        .text_xs()
                        .when_some(entry.mark, |d, (glyph, color)| d.text_color(color).child(glyph)),
                )
            })
            .child(div().flex_none().when(entry.dim, |d| d.text_color(muted)).child(entry.label.clone()))
            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(cx.theme().muted_foreground).child(entry.detail.clone()))
            .child(div().flex_none().text_xs().text_color(cx.theme().muted_foreground).child(entry.keys.clone()))
            .into_any_element()
    })
}

impl Workspace {
    pub fn command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let with_task = |f: fn(&mut task::TaskView, &mut Window, &mut Context<task::TaskView>)| {
            let this = this.clone();
            move |window: &mut Window, cx: &mut App| {
                if let Some(t) = this.upgrade().and_then(|w| w.read(cx).active_view()) {
                    t.update(cx, |t, cx| f(t, window, cx));
                }
            }
        };
        // A command of the workspace, with the word that says what kind it is.
        let own = |label: &str, kind: &str, run: fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
            let this = this.clone();
            Entry::new(label.to_string(), move |window, cx| {
                let _ = this.update(cx, |w, cx| run(w, window, cx));
            })
            .detail(kind.to_string())
        };
        let key = |entry: Entry, id: &str| entry.keys(crate::keymap::label(id));
        let keyed = |label: &str, id: &str, kind: &str| Entry::shortcut(label, id).detail(kind.to_string());
        let (presenting, saving) = (self.is_presenting(), crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed));
        // In the order of the menu: what to start, where to go, the task's
        // views, git, the editor, the window, the app.
        // The spaces to go to, when there are some.
        let mut to_spaces: Vec<Entry> = Vec::new();
        if !self.spaces.is_empty() {
            let go = |label: String, to: Option<String>| {
                let this = this.clone();
                Entry::new(label, move |window, cx| {
                    let to = to.clone();
                    let _ = this.update(cx, |w, cx| w.switch_space(to, window, cx));
                })
                .detail("space: the projects in the window")
            };
            to_spaces.push(go("All spaces".into(), None));
            to_spaces.extend(self.spaces.iter().map(|space| go(format!("Space: {}", space.name), Some(space.name.clone()))));
        }
        to_spaces.push(own("Spaces…", "space: add, name, remove", |w, window, cx| w.spaces_dialog(window, cx)));
        // The task's own and git's, where the task in hand has them to do.
        let mut of_task: Vec<Entry> = Vec::new();
        let of_git: Vec<Entry>;
        {
            use crate::commands::TaskCommand as T;
            use crate::task::GitCommand as G;
            // On the variants' screen the card in hand, which the command acts on.
            let hand = self.in_hand(cx);
            let starred = hand.is_some_and(|t| t.starred);
            let task = |what: T| {
                let this = this.clone();
                Entry::new(crate::commands::sentence(what.name(starred)), move |window, cx| {
                    let _ = this.update(cx, |w, cx| w.task_command(what, window, cx));
                })
                .detail(what.hint())
            };
            let git = |what: G| {
                let this = this.clone();
                Entry::new(crate::commands::sentence(crate::commands::git_name(what)), move |window, cx| {
                    let _ = this.update(cx, |w, cx| w.git_command(what, window, cx));
                })
                .detail(crate::commands::git_hint(what))
            };
            use T::*;
            of_task.extend([NewTerminal, NewBrowserTab, RestartAgent, CompareThisFile, Star, CopyTitle, CopyBranchName, Rename, AnotherVariant, KeepVariant, StartServices, StopServices, Pause, Archive, Defer, RemoveSide].into_iter().filter(|what| hand.is_some_and(|t| t.allows(*what))).map(task));
            of_git = G::ALL.into_iter().filter(|what| hand.is_some_and(|t| t.git(*what))).map(git).collect();
        }
        let mut entries = vec![
            keyed("New task…", "new_task", "task"),
            keyed("Ask a question…", "ask_question", "task: beside the main thread's agent, which goes on"),
            keyed("Next waiting task", "next_waiting", "task: an agent that asks or is done"),
            key(own("Recent tasks", "task", |w, window, cx| w.recent_tasks(window, cx)), "recent"),
            own("Recent files", "go to", |w, window, cx| w.recent_files(window, cx)).keys({
                let e = crate::keymap::label("recent");
                if e.is_empty() { e } else { format!("{e} {e}") }
            }),
            own("Add project…", "task", |w, window, cx| w.add_project_dialog(window, cx)),
            own("Archive merged tasks…", "task: of the project in hand", |w, window, cx| {
                if let Some(project) = w.project_in_hand() {
                    w.archive_merged(project, window, cx);
                }
            }),
            own("Archived tasks…", "task: search, restore, delete", |w, window, cx| w.archived_dialog(None, window, cx)),
            keyed("Go to file…", "go_to_file", "go to"),
            keyed("Go to class or symbol…", "go_to_symbol", "go to: anywhere in the project"),
            keyed("Find in files…", "find_in_files", "go to"),
            keyed("Replace in files…", "replace_in_files", "editor: across the project"),
            keyed("Find in terminal…", "find_in_terminal", "go to: the scrollback"),
            keyed("Go to line…", "go_to_line", "go to"),
            keyed("File structure", "outline", "go to: declarations of the open file"),
            keyed("Go to definition", "definition", "go to"),
            keyed("Implementations and usages", "usages", "go to"),
            keyed("Back", "back", "go to"),
            keyed("Forward", "forward", "go to"),
            keyed("Agent", "agent", "view"),
            keyed("Changes", "changes", "view: the diff"),
            keyed("History", "history", "view: the branch's commits"),
            keyed("Shell", "shell", "view: terminal"),
            keyed("Next tab", "next_tab", "view"),
            keyed("Previous tab", "prev_tab", "view"),
            keyed("Focus mode", "focus_mode", "view: zoom the pane"),
            keyed("Changes panel", "changes_panel", "panel"),
            keyed("Files panel", "files_panel", "panel: project tree"),
            keyed("Structure panel", "structure_panel", "panel"),
            keyed("Run panel", "run_panel", "panel: services, dev servers, workers"),
            keyed("Database panel", "database_panel", "panel: connections, databases, tables, consoles"),
            keyed("Branches…", "branches", "git: open as a task, compare, delete"),
            Entry::new(crate::commands::sentence(crate::commands::TaskCommand::CompareWithBranch.name(false)), with_task(|t, window, cx| t.compare_with_dialog(None, window, cx))).detail("git: the working tree"),
            keyed("Complete here", "complete", "editor"),
            keyed("Quick fixes", "quick_fix", "editor: what the language server offers"),
            keyed("Rename…", "rename", "editor: the symbol, everywhere"),
            keyed("Reformat file", "format", "editor"),
            keyed("Parameter info", "parameter_info", "editor: the call being written"),
            keyed("Fold", "fold", "editor"),
            keyed("Unfold", "unfold", "editor"),
            keyed("Fold all", "fold_all", "editor"),
            keyed("Unfold all", "unfold_all", "editor"),
            keyed("Annotate with Git Blame", "annotate", "editor: who changed each line; again to close"),
            keyed("Code, split, preview", "cycle_look", "editor: Markdown, SVG, a table or a diagram as text, drawn, or both"),
            keyed("Toggle bookmark", "bookmark", "editor: the cursor's line"),
            keyed("Bookmarks", "bookmarks", "go to: the task's bookmarked lines"),
            keyed("Next bookmark", "next_bookmark", "go to: in the file"),
            keyed("Previous bookmark", "prev_bookmark", "go to: in the file"),
            key(own("Theme…", "window: each is drawn as you go through them", |w, window, cx| w.theme_picker(window, cx)), "theme"),
            own("Light theme", "window", |w, window, cx| w.set_appearance(crate::theme::Appearance::Light, window, cx)),
            own("Dark theme", "window", |w, window, cx| w.set_appearance(crate::theme::Appearance::Dark, window, cx)),
            own("Theme as the system", "window: light by day, dark by night", |w, window, cx| w.set_appearance(crate::theme::Appearance::Auto, window, cx)),
            own("Text of its usual size", "window: editor, diff and terminal", |_, _, cx| crate::zoom::reset_all(cx)),
            own(if presenting { "End presentation" } else { "Enter presentation mode…" }, "window: only the tasks chosen", |w, window, cx| w.toggle_presentation(window, cx)),
            own(if saving { "Leave power save mode" } else { "Power save mode" }, "window: language servers and background work", |w, _, cx| w.toggle_power_saving(cx)),
            keyed("Settings…", "settings", "app"),
            own("Keyboard shortcuts…", "app", |w, window, cx| w.open_settings_at(crate::settings_ui::KEYS_PAGE, window, cx)),
            own("Restart", "app", |w, window, cx| w.restart(window, cx)),
            keyed("Quit", "quit", "app"),
        ];
        let at = |entries: &Vec<Entry>, label: &str| entries.iter().position(|entry| entry.label.as_ref() == label).unwrap_or(entries.len());
        let after_tasks = at(&entries, "Go to file…");
        entries.splice(after_tasks..after_tasks, of_task);
        let with_git = at(&entries, "Complete here");
        entries.splice(with_git..with_git, of_git);
        // With the window's own: before the app's.
        let app = entries.iter().position(|entry| entry.label.as_ref() == "Settings…").unwrap_or(entries.len());
        entries.splice(app..app, to_spaces);
        open("Run a command…", entries, None, window, cx);
    }

    /// Tasks by when you were last in them, the current one left out, each
    /// with its agent's status; a query finds archived ones too, after
    /// them, which open Restore.
    pub fn recent_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        struct Row {
            when: u64,
            path: PathBuf,
            title: String,
            detail: String,
            mark: Option<(&'static str, Hsla)>,
            paused: bool,
            /// A question asked aside in the task, and whether it was closed.
            question: Option<(u32, bool)>,
        }
        let mut tasks: Vec<Row> = Vec::new();
        for (pi, p) in self.projects.iter().enumerate() {
            for (ti, t) in p.tasks.iter().enumerate() {
                if self.active.as_deref() == Some(t.path.as_path()) || t.busy.is_some() || !self.shows(pi, ti) {
                    continue;
                }
                let mut detail = vec![p.name.clone()];
                detail.extend(Some(crate::sidebar::ago(t.last_active)).filter(|when| !when.is_empty()));
                let (mark, paused) = match &t.view {
                    Some(view) => {
                        let v = view.read(cx);
                        let changes = v.change_count();
                        if changes > 0 {
                            detail.push(if changes == 1 { "1 file changed".into() } else { format!("{changes} files changed") });
                        }
                        (task::status_glyph(&v.agent.status), false)
                    }
                    None => {
                        detail.push("paused".into());
                        (None, true)
                    }
                };
                tasks.push(Row { when: t.last_active, path: t.path.clone(), title: self.task_title(pi, ti, cx), detail: detail.join(" · "), mark, paused, question: None });
            }
            // Its questions, open ones with the tasks, closed ones found as archived tasks are.
            for (ti, t) in p.tasks.iter().enumerate() {
                // A main thread paused is not listed, its questions are.
                if t.busy.is_some() || !self.in_space(pi) || (!t.main && !self.shows(pi, ti)) {
                    continue;
                }
                let view = t.view.as_ref().map(|v| v.read(cx));
                for q in &t.questions {
                    let closed = q.closed > 0;
                    let status = view.filter(|_| !closed).and_then(|v| v.agent_of(task::Who::Question(q.id))).and_then(|a| task::status_glyph(&a.status));
                    let when = if closed { q.closed } else { q.created.max(q.active).max(q.opened) };
                    let mut detail = format!("question · {}", p.name);
                    if !t.main {
                        detail.push_str(&format!(" · {}", self.task_title(pi, ti, cx)));
                    }
                    detail.push_str(&if closed { format!(" · closed {}", crate::sidebar::ago(q.closed)) } else { format!(" · {}", crate::sidebar::ago(when)) });
                    let mark = status.or(Some(("?", hsla(0., 0., 0.55, 1.))));
                    tasks.push(Row { when, path: t.path.clone(), title: format!("? {}", q.title), detail, mark, paused: closed, question: Some((q.id, closed)) });
                }
            }
        }
        tasks.sort_by(|a, b| b.when.cmp(&a.when));
        let this = cx.weak_entity();
        let entries = tasks
            .into_iter()
            .map(|row| {
                let this = this.clone();
                let path = row.path;
                let question = row.question;
                let mut entry = Entry::new(row.title, move |window, cx| {
                    let _ = this.update(cx, |w, cx| match question {
                        Some((n, false)) => w.show_question(&path, n, window, cx),
                        Some((n, true)) => w.reopen_question(&path, n, window, cx),
                        None => {
                            w.switch(&path, window, cx);
                            if let Some(view) = w.active_view() {
                                view.update(cx, |t, cx| t.show_view(0, window, cx));
                            }
                        }
                    });
                })
                .detail(row.detail);
                entry.mark = row.mark;
                entry.dim = row.paused;
                // Closed, it comes when looked for, after the rest.
                entry.archived = matches!(question, Some((_, true)));
                entry.marked = true;
                entry
            })
            .collect();
        let this = cx.weak_entity();
        let again: Run = Rc::new(move |window, cx| {
            let _ = this.update(cx, |w, cx| w.recent_files(window, cx));
        });
        let palette = open("Recent tasks…", entries, Some((Recent::Tasks, again)), window, cx);
        if self.is_presenting() {
            return;
        }
        // The archived ones of the projects in view, read in the background.
        let names: std::collections::HashMap<PathBuf, String> =
            self.projects.iter().enumerate().filter(|(pi, _)| self.in_space(*pi)).map(|(_, p)| (p.path.clone(), p.name.clone())).collect();
        let root = self.archive_dir.clone();
        let this = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let found = cx
                .background_spawn(async move {
                    crate::archive::list(&root).into_iter().filter(|a| names.contains_key(&a.meta.project)).map(|a| {
                        let name = names.get(&a.meta.project).cloned().unwrap_or_default();
                        (a, name)
                    }).collect::<Vec<_>>()
                })
                .await;
            let entries: Vec<Entry> = found
                .into_iter()
                .map(|(item, project)| {
                    let this = this.clone();
                    let title = item.meta.title.clone();
                    let detail = match item.deferred {
                        Some(d) => format!("{project} · {}", d.told()),
                        None => format!("{project} · archived {}", crate::sidebar::ago(item.meta.archived)),
                    };
                    let mark = if item.deferred.is_some() { "◷" } else { "↺" };
                    let mut entry = Entry::new(title, move |window, cx| {
                        let item = item.clone();
                        let _ = this.update(cx, |w, cx| w.restore_dialog(item, window, cx));
                    })
                    .detail(detail);
                    entry.mark = Some((mark, hsla(0., 0., 0.55, 1.)));
                    entry.dim = true;
                    entry.archived = true;
                    entry.marked = true;
                    entry
                })
                .collect();
            let _ = palette.update(cx, |p, cx| p.add(entries, cx));
        })
        .detach();
    }

    /// Files of the current task by when you were last in them.
    pub fn recent_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.active_view() else { return };
        let (root, files) = {
            let t = view.read(cx);
            (t.root.clone(), t.recent_files(cx))
        };
        let task = view.downgrade();
        let entries = files
            .into_iter()
            .map(|path| {
                let rel = path.strip_prefix(&root).unwrap_or(&path).to_path_buf();
                let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = rel.parent().map(|d| d.display().to_string()).unwrap_or_default();
                let task = task.clone();
                Entry::new(name, move |window, cx| {
                    let path = path.clone();
                    let _ = task.update(cx, |t, cx| t.reopen_recent(path, window, cx));
                })
                .detail(dir)
            })
            .collect();
        let this = cx.weak_entity();
        let again: Run = Rc::new(move |window, cx| {
            let _ = this.update(cx, |w, cx| w.recent_tasks(window, cx));
        });
        open("Recent files…", entries, Some((Recent::Files, again)), window, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui_kit::component::command::{Command, CommandItem, CommandState};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Bounds, Entity, IntoElement, Point, Render, TestAppContext, Window, WindowBounds, WindowOptions, px, size,
    };

    use super::{Entry, Palette};

    struct Host(Entity<CommandState>, Rc<Cell<Option<usize>>>);

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            let confirmed = self.1.clone();
            Command::new(&self.0)
                .max_h(px(200.))
                .items((0..40).map(|n| CommandItem::new().label(format!("item {n}"))))
                .on_confirm(move |index, _, _| confirmed.set(Some(index.row)))
        }
    }

    /// Page Down goes as far as the list shows and stops at the end; Page
    /// Up comes back as far and stops at the start.
    #[gpui_kit::test]
    fn page_keys_go_by_what_the_list_shows(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let confirmed: Rc<Cell<Option<usize>>> = Rc::default();
        let mut command = None;
        let handle: AnyWindowHandle = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(800.), px(600.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            let confirmed = confirmed.clone();
            gpui_kit::open_window(options, cx, |window, cx| {
                let state = cx.new(|cx| CommandState::new(window, cx));
                command = Some(state.clone());
                cx.new(|_| Host(state, confirmed))
            })
            .expect("a test window")
            .0
        });
        let command = command.unwrap();
        let row_after = |keys: &[&str], cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                command.update(cx, |state, cx| state.focus(window, cx));
                for key in keys {
                    window.press(key, cx);
                }
                window.press("enter", cx);
            })
            .unwrap();
            cx.run_until_parked();
            confirmed.take().expect("a row confirmed")
        };
        let page = row_after(&["pagedown"], cx);
        assert!((2..20).contains(&page), "a page is what 200px show, less one: {page}");
        assert_eq!(row_after(&["pagedown"; 30], cx), 39, "no wrapping past the end");
        assert_eq!(row_after(&["pageup"], cx), 39 - page);
        assert_eq!(row_after(&["pageup"; 30], cx), 0, "no wrapping past the start");
    }

    /// A query put in for the user is selected: what is typed replaces it.
    #[gpui_kit::test]
    fn a_query_put_in_is_replaced_by_typing(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut command = None;
        let handle: AnyWindowHandle = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(800.), px(600.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            gpui_kit::open_window(options, cx, |window, cx| {
                let state = cx.new(|cx| CommandState::new(window, cx));
                state.update(cx, |c, cx| {
                    c.set_query("needle", window, cx);
                    c.select_query(window, cx);
                });
                command = Some(state.clone());
                cx.new(|_| Host(state, Rc::default()))
            })
            .expect("a test window")
            .0
        });
        let command = command.unwrap();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            command.update(cx, |state, cx| state.focus(window, cx));
            window.press("x", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(command.read(cx).query(cx).as_ref(), "x"));
    }

    /// Archived tasks are not in the list until something is typed, and
    /// then come after the tasks there are, however well they match.
    #[gpui_kit::test]
    fn archived_tasks_come_with_a_query_and_last(cx: &mut TestAppContext) {
        let palette = cx.new(|_| Palette { entries: Rc::new(Vec::new()), shown: Rc::default(), query: String::new() });
        let entry = |label: &str, archived: bool| {
            let mut entry = Entry::new(label.to_string(), |_, _| {});
            entry.archived = archived;
            entry
        };
        palette.update(cx, |p, cx| {
            p.add(vec![entry("web review", false), entry("review tools", false)], cx);
            p.add(vec![entry("review", true), entry("unrelated", true)], cx);
            assert_eq!(*p.shown, vec![0, 1]);
            p.query("review", cx);
            assert_eq!(p.shown.len(), 3);
            assert_eq!(p.shown[2], 2, "the archived one, an exact match, after the others");
            p.query("", cx);
            assert_eq!(*p.shown, vec![0, 1]);
        });
    }
}
