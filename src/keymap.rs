//! The app's shortcuts in one table: what Settings lists and rebinds, and
//! what labels in menus and the command palette show.

use gpui_kit::{Action, App, KeyBinding, NoAction};

use crate::{AskQuestion, FindInFiles, ReplaceInFiles, GoToFile, GoToSymbol, NewTask, SwitchTheme, OpenSettings, Quit, keys, palette, task};

/// Where a shortcut works.
#[derive(Clone, Copy, PartialEq)]
enum Scope {
    /// Everywhere, terminals too: bound in theirs, which would take it first.
    Everywhere,
    /// Everywhere but terminals, where the key stays the shell's.
    NotInTerminal,
    /// Nor Web tabs, where the page has the key (Cmd+K is many a site's search).
    NotInTerminalOrPage,
    /// Anywhere a terminal does not take it first.
    App,
    Editor,
    /// An open file: its editor, or the file drawn instead of its text.
    File,
    Terminal,
    /// The Changes tab's diff, while it has the keyboard: single letters, as
    /// nothing is typed there.
    Review,
    /// A Web tab: a browser's keys, over the window's where they clash.
    Browser,
    /// A picture with the keyboard: its zoom, over the same keys elsewhere.
    Picture,
    /// The editor of a `.sql` file, over what the same keys do in any editor.
    Sql,
    /// The editor of a `.http` file, the same way.
    Http,
}

pub struct Shortcut {
    pub id: &'static str,
    pub name: &'static str,
    /// Written the Linux way; [`keys::key`] makes the macOS one.
    default: &'static str,
    scope: Scope,
    action: fn() -> Box<dyn Action>,
    /// Gives its action a way in from the macOS menu bar when nothing on the
    /// focus path handles it: see `menu::reach`.
    pub reach: fn(&mut App),
}

macro_rules! shortcut {
    ($id:literal, $name:literal, $keys:literal, $scope:ident, $action:expr) => {
        Shortcut { id: $id, name: $name, default: $keys, scope: Scope::$scope, action: || Box::new($action), reach: |cx| crate::menu::reach(&$action, cx) }
    };
}

pub static SHORTCUTS: &[Shortcut] = &[
    shortcut!("command_palette", "Command palette", "ctrl-shift-a", Everywhere, palette::CommandPalette),
    shortcut!("recent", "Recent tasks, then files", "ctrl-e", Everywhere, palette::RecentPlaces),
    shortcut!("next_waiting", "Next waiting task", "alt-down", Everywhere, palette::NextWaiting),
    shortcut!("new_task", "New task", "ctrl-shift-t", Everywhere, NewTask),
    shortcut!("ask_question", "Ask a question", "ctrl-i", Everywhere, AskQuestion),
    shortcut!("go_to_file", "Go to file", "ctrl-shift-n", Everywhere, GoToFile),
    shortcut!("branches", "Branches", "ctrl-shift-`", NotInTerminal, task::ShowBranches),
    shortcut!("go_to_symbol", "Go to class or symbol", "ctrl-n", NotInTerminal, GoToSymbol),
    shortcut!("find_in_files", "Find in files", "ctrl-shift-f", Everywhere, FindInFiles),
    shortcut!("replace_in_files", "Replace in files", "ctrl-shift-r", Everywhere, ReplaceInFiles),
    shortcut!("find_in_terminal", "Find in terminal", "ctrl-f", Terminal, task::FindInTerminal),
    shortcut!("agent", "Agent", "ctrl-1", Everywhere, task::ShowView(0)),
    shortcut!("changes", "Changes", "ctrl-2", Everywhere, task::ShowView(1)),
    shortcut!("history", "History", "ctrl-3", Everywhere, task::ShowView(2)),
    shortcut!("shell", "Shell", "ctrl-`", Everywhere, task::ShowView(3)),
    shortcut!("changes_panel", "Changes panel", "alt-1", Everywhere, task::ShowSidePanel(0)),
    shortcut!("files_panel", "Files panel", "alt-2", Everywhere, task::ShowSidePanel(1)),
    shortcut!("structure_panel", "Structure panel", "alt-3", Everywhere, task::ShowSidePanel(2)),
    shortcut!("run_panel", "Run panel", "alt-4", Everywhere, task::ShowSidePanel(3)),
    shortcut!("database_panel", "Database panel", "alt-5", Everywhere, task::ShowSidePanel(4)),
    shortcut!("http_panel", "HTTP panel", "alt-6", Everywhere, task::ShowSidePanel(5)),
    shortcut!("tabs_panel", "Tabs panel", "alt-7", Everywhere, task::ShowSidePanel(6)),
    shortcut!("next_tab", "Next tab", "ctrl-pagedown", Everywhere, task::NextTab),
    shortcut!("prev_tab", "Previous tab", "ctrl-pageup", Everywhere, task::PrevTab),
    shortcut!("close_tab", "Close tab", "ctrl-f4", Everywhere, task::CloseTab),
    shortcut!("reopen_tab", "Reopen closed tab", "ctrl-shift-f4", Everywhere, task::ReopenClosedTab),
    shortcut!("back", "Back", "alt-left", NotInTerminal, task::NavigateBack),
    shortcut!("forward", "Forward", "alt-right", NotInTerminal, task::NavigateForward),
    shortcut!("last_edit", "Last edit location", "ctrl-shift-backspace", NotInTerminal, task::LastEditLocation),
    // JetBrains' keys for Commit and Push; a terminal keeps them (Cmd+K clears it).
    shortcut!("commit", "Commit…", "ctrl-k", NotInTerminalOrPage, crate::menu::Git(task::GitCommand::Commit)),
    shortcut!("push", "Push", "ctrl-shift-k", NotInTerminalOrPage, crate::menu::Git(task::GitCommand::Push)),
    // No keys of its own: JetBrains' Ctrl+T is a slip from Ctrl+Shift+T (New task) away, and Pull changes the worktree.
    shortcut!("pull", "Pull", "", NotInTerminalOrPage, crate::menu::Git(task::GitCommand::Pull)),
    shortcut!("focus_mode", "Focus mode", "shift-escape", Everywhere, task::ToggleZoom),
    shortcut!("save", "Save", "ctrl-s", App, task::SaveFile),
    shortcut!("definition", "Go to definition", "ctrl-b", Editor, gpui_kit::component::input::GoToDefinition),
    shortcut!("usages", "Implementations and usages", "ctrl-alt-b", Editor, task::ShowUsages),
    shortcut!("outline", "File structure", "ctrl-f12", Editor, task::ShowOutline),
    shortcut!("expand_selection", "Expand selection", "ctrl-w", Editor, task::ExpandSelection),
    shortcut!("shrink_selection", "Shrink selection", "ctrl-shift-w", Editor, task::ShrinkSelection),
    shortcut!("comment", "Comment / uncomment", "ctrl-/", Editor, task::ToggleComment),
    shortcut!("delete_line", "Delete line", "ctrl-y", Editor, task::DeleteLine),
    shortcut!("duplicate", "Duplicate line or selection", "ctrl-d", Editor, task::DuplicateLine),
    shortcut!("move_line_up", "Move line up", "ctrl-shift-up", Editor, task::MoveLineUp),
    shortcut!("move_line_down", "Move line down", "ctrl-shift-down", Editor, task::MoveLineDown),
    shortcut!("join_lines", "Join lines", "ctrl-shift-j", Editor, task::JoinLines),
    shortcut!("toggle_case", "Upper / lower case", "ctrl-shift-u", Editor, task::ToggleCase),
    shortcut!("next_occurrence", "Select next occurrence", "alt-j", Editor, task::NextOccurrence),
    shortcut!("go_to_line", "Go to line", "ctrl-g", Editor, task::GoToLine),
    shortcut!("complete", "Complete here", "ctrl-space", Editor, gpui_kit::component::input::ShowCompletions),
    shortcut!("quick_fix", "Quick fixes", "alt-enter", Editor, gpui_kit::component::input::ToggleCodeActions),
    shortcut!("rename", "Rename", "shift-f6", Editor, task::RenameSymbol),
    shortcut!("next_problem", "Next problem", "f2", Editor, task::NextProblem),
    shortcut!("prev_problem", "Previous problem", "shift-f2", Editor, task::PrevProblem),
    shortcut!("format", "Reformat file", "ctrl-alt-l", Editor, task::FormatFile),
    shortcut!("fold", "Fold", "ctrl--", Editor, task::Fold),
    shortcut!("unfold", "Unfold", "ctrl-=", Editor, task::Unfold),
    shortcut!("fold_all", "Fold all", "ctrl-shift--", Editor, task::FoldAll),
    shortcut!("unfold_all", "Unfold all", "ctrl-shift-=", Editor, task::UnfoldAll),
    shortcut!("review_next_change", "Next change", "j", Review, task::NextChange),
    shortcut!("review_prev_change", "Previous change", "k", Review, task::PrevChange),
    shortcut!("next_difference", "Next difference", "f7", Review, task::NextChange),
    shortcut!("prev_difference", "Previous difference", "shift-f7", Review, task::PrevChange),
    shortcut!("review_next_file", "Next file", "n", Review, task::NextChangedFile),
    shortcut!("review_prev_file", "Previous file", "p", Review, task::PrevChangedFile),
    shortcut!("review_viewed", "Viewed, then the next file", "v", Review, task::ToggleFileViewed),
    shortcut!("review_stage", "Stage the change", "s", Review, task::StageChange),
    shortcut!("review_revert", "Revert the change", "r", Review, task::RevertChange),
    shortcut!("review_comment", "Comment on the change", "c", Review, task::CommentOnChange),
    shortcut!("review_open_file", "Open the file at the line", "f4", Review, task::JumpToSource),
    shortcut!("copy_reference", "Copy reference (path:line)", "ctrl-alt-shift-c", Editor, task::CopyReference),
    shortcut!("bookmark", "Toggle bookmark", "f11", Editor, task::ToggleBookmark),
    shortcut!("bookmarks", "Bookmarks", "shift-f11", NotInTerminal, task::ShowBookmarks),
    shortcut!("next_bookmark", "Next bookmark", "alt-pagedown", Editor, task::NextBookmark),
    shortcut!("prev_bookmark", "Previous bookmark", "alt-pageup", Editor, task::PrevBookmark),
    // No keys of its own, as in JetBrains IDEs: the gutter's menu has it.
    shortcut!("annotate", "Annotate with Git Blame", "", Editor, task::ToggleBlame),
    // VS Code's key for a Markdown preview.
    shortcut!("cycle_look", "Code, split, preview", "ctrl-shift-v", File, task::CycleLook),
    shortcut!("sql_run", "Run the SQL statement", "ctrl-enter", Sql, task::RunStatement),
    shortcut!("sql_run_file", "Run the whole SQL file", "ctrl-shift-enter", Sql, task::RunFile),
    shortcut!("sql_definition", "SQL: definition, alias or schema", "ctrl-b", Sql, task::SqlDefinition),
    shortcut!("sql_stop", "Stop the SQL statement", "ctrl-f2", Sql, task::StopStatement),
    shortcut!("http_run", "Send the HTTP request", "ctrl-enter", Http, task::RunRequest),
    shortcut!("http_run_all", "Send every HTTP request of the file", "ctrl-shift-enter", Http, task::RunAllRequests),
    shortcut!("parameter_info", "Parameter info", "ctrl-p", Editor, task::ParameterInfo),
    shortcut!("quick_doc", "Quick documentation", "f1", Editor, task::QuickDocumentation),
    shortcut!("optimize_imports", "Optimize imports", "ctrl-alt-o", Editor, task::OptimizeImports),
    shortcut!("show_in_files", "Show in Files", "alt-f1", File, task::ShowFileInFiles),
    // Ctrl+Y is the line's: the editor's own Redo moves here.
    shortcut!("redo", "Redo", "ctrl-shift-z", Editor, gpui_kit::component::input::Redo),
    shortcut!("web_address", "Address bar", "ctrl-l", Browser, crate::browser::BrowserFocusAddress),
    shortcut!("web_reload", "Reload", "ctrl-r", Browser, crate::browser::BrowserReload),
    shortcut!("web_hard_reload", "Reload, not from the cache", "ctrl-shift-r", Browser, crate::browser::BrowserHardReload),
    shortcut!("web_back", "Page back", "alt-left", Browser, crate::browser::BrowserBack),
    shortcut!("web_forward", "Page forward", "alt-right", Browser, crate::browser::BrowserForward),
    shortcut!("web_find", "Find in page", "ctrl-f", Browser, crate::browser::BrowserFind),
    shortcut!("web_zoom_in", "Zoom in", "ctrl-=", Browser, crate::browser::BrowserZoomIn),
    shortcut!("web_zoom_out", "Zoom out", "ctrl--", Browser, crate::browser::BrowserZoomOut),
    shortcut!("web_zoom_reset", "Actual size", "ctrl-0", Browser, crate::browser::BrowserZoomReset),
    shortcut!("picture_zoom_in", "Zoom in", "ctrl-=", Picture, crate::picture::PictureZoomIn),
    shortcut!("picture_zoom_out", "Zoom out", "ctrl--", Picture, crate::picture::PictureZoomOut),
    shortcut!("picture_actual_size", "Actual size", "ctrl-0", Picture, crate::picture::PictureActualSize),
    // As macOS Preview has it.
    shortcut!("picture_fit", "Fit", "ctrl-9", Picture, crate::picture::PictureFit),
    shortcut!("web_devtools", "DevTools", "f12", Browser, crate::browser::BrowserDevTools),
    // No keys of its own: a theme is seldom changed, and the menu has it.
    shortcut!("theme", "Switch theme", "", Everywhere, SwitchTheme),
    shortcut!("settings", "Settings", "ctrl-,", Everywhere, OpenSettings),
    // K for keys: Ctrl+K is Commit's, Ctrl+Shift+A the palette's.
    shortcut!("keyboard_shortcuts", "Keyboard shortcuts", "ctrl-alt-k", Everywhere, crate::menu::KeyboardShortcuts),
    shortcut!("quit", "Exit", "ctrl-q", Everywhere, Quit),
];

pub fn find(id: &str) -> Option<&'static Shortcut> {
    SHORTCUTS.iter().find(|s| s.id == id)
}

impl Shortcut {
    pub fn action(&self) -> Box<dyn Action> {
        (self.action)()
    }

    /// The heading Settings lists it under.
    pub fn area(&self) -> &'static str {
        match self.scope {
            Scope::Editor | Scope::File | Scope::Sql | Scope::Http => "Editor",
            Scope::Terminal => "Terminal",
            Scope::Review => "Changes",
            Scope::Browser => "Web",
            Scope::Picture => "Picture",
            _ => "Window",
        }
    }

    /// This platform's default keystrokes ("ctrl-e", "cmd-e").
    pub fn default_keys(&self) -> String {
        keys::key(self.default)
    }

    /// What is in force: Settings' choice, else the default; empty for none.
    pub fn keys(&self) -> String {
        crate::state::settings().keys.get(self.id).cloned().unwrap_or_else(|| self.default_keys())
    }

    fn bindings(&self, keys: &str, unbind: bool, cx: &App) -> Vec<KeyBinding> {
        if keys.is_empty() {
            return Vec::new();
        }
        let terminal = Some(gpui_cn::terminal::KEY_CONTEXT);
        let action = || if unbind { Box::new(NoAction) as Box<dyn Action> } else { (self.action)() };
        let bind = |context: Option<&str>, action: Box<dyn Action>| {
            let predicate = context.and_then(|c| gpui_kit::KeyBindingContextPredicate::parse(c).ok()).map(std::rc::Rc::new);
            KeyBinding::load(keys, action, predicate, false, None, cx)
        };
        let made = match self.scope {
            Scope::Everywhere => vec![bind(None, action()), bind(terminal, action())],
            // A key the terminal binds itself (Cmd+K, its Clear on macOS) is
            // its own already, and a NoAction there would take it from it.
            Scope::NotInTerminal | Scope::NotInTerminalOrPage => {
                let outside = bind(None, action());
                let terminal_s = outside.as_ref().is_ok_and(|outside| terminal_binds(outside.keystrokes()));
                let mut made = vec![outside];
                if !terminal_s {
                    made.push(bind(terminal, Box::new(NoAction)));
                }
                if self.scope == Scope::NotInTerminalOrPage {
                    made.push(bind(Some(crate::browser::KEY_CONTEXT), Box::new(NoAction)));
                }
                made
            }
            Scope::App => vec![bind(None, action())],
            Scope::Editor => vec![bind(Some("Input"), action())],
            Scope::File => vec![bind(Some("Input"), action()), bind(Some(crate::task::PREVIEW_CONTEXT), action())],
            Scope::Terminal => vec![bind(terminal, action())],
            Scope::Review => vec![bind(Some("Changes > Diff"), action())],
            Scope::Browser => vec![bind(Some(crate::browser::KEY_CONTEXT), action())],
            Scope::Picture => vec![bind(Some(crate::picture::KEY_CONTEXT), action())],
            Scope::Sql => vec![bind(Some(crate::task::SQL_INPUT_CONTEXT), action())],
            Scope::Http => vec![bind(Some(crate::task::HTTP_INPUT_CONTEXT), action())],
        };
        made.into_iter().filter_map(Result::ok).collect()
    }
}

/// Whether a terminal has these keys bound for itself (its Copy, Clear).
fn terminal_binds(keys: &[gpui_kit::KeybindingKeystroke]) -> bool {
    gpui_cn::terminal::default_key_bindings().iter().any(|own| own.keystrokes() == keys)
}

/// Every shortcut as Settings has it, for startup.
pub fn bind_all(cx: &mut App) {
    for s in SHORTCUTS {
        let bindings = s.bindings(&s.keys(), false, cx);
        cx.bind_keys(bindings);
    }
}

/// After Settings changed some: the old keys first lose their action, then
/// the new ones get theirs, so two shortcuts can swap keys.
pub fn rebind(old: &crate::state::Settings, cx: &mut App) {
    let before = |s: &Shortcut| old.keys.get(s.id).cloned().unwrap_or_else(|| s.default_keys());
    let changed: Vec<&Shortcut> = SHORTCUTS.iter().filter(|s| before(s) != s.keys()).collect();
    for s in &changed {
        let bindings = s.bindings(&before(s), true, cx);
        cx.bind_keys(bindings);
    }
    for s in &changed {
        let bindings = s.bindings(&s.keys(), false, cx);
        cx.bind_keys(bindings);
    }
    crate::menu::bind_system_keys(cx);
}

/// "Ctrl+Shift+N", or "⌘⇧O" on macOS, for keystrokes like "ctrl-shift-n".
pub fn display(keystrokes: &str) -> String {
    let mac = cfg!(target_os = "macos");
    keystrokes
        .split(' ')
        .filter(|c| !c.is_empty())
        .map(|chord| {
            // "ctrl--" is Ctrl and the minus key.
            let mut parts: Vec<&str> = chord.split('-').collect();
            if chord.ends_with("--") {
                parts.truncate(parts.len() - 2);
                parts.push("-");
            }
            let (key, mods) = parts.split_last().map_or(("", &[][..]), |(k, m)| (*k, m));
            let mut out: Vec<String> = mods
                .iter()
                .map(|m| {
                    match (*m, mac) {
                        ("ctrl", false) => "Ctrl",
                        ("ctrl", true) => "⌃",
                        ("cmd" | "super" | "platform", true) => "⌘",
                        ("cmd" | "super" | "platform", false) => "Super",
                        ("alt", false) => "Alt",
                        ("alt", true) => "⌥",
                        ("shift", false) => "Shift",
                        ("shift", true) => "⇧",
                        (other, _) => other,
                    }
                    .to_string()
                })
                .collect();
            let key = match key {
                "pagedown" => "PgDn".to_string(),
                "pageup" => "PgUp".to_string(),
                "escape" => "Esc".to_string(),
                "left" if mac => "←".into(),
                "right" if mac => "→".into(),
                "up" if mac => "↑".into(),
                "down" if mac => "↓".into(),
                k if k.chars().count() == 1 => k.to_uppercase(),
                k => {
                    let mut c = k.chars();
                    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                }
            };
            out.push(key);
            out.join(if mac { "" } else { "+" })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The label of a shortcut by its id, as in force now; empty when unbound.
pub fn label(id: &str) -> String {
    find(id).map(|s| display(&s.keys())).unwrap_or_default()
}

/// A tooltip's text with the shortcut's keys after it, " (Ctrl+3)"; just
/// the text when its keys were taken off in Settings.
pub fn tip(text: &str, id: &str) -> String {
    match label(id) {
        keys if keys.is_empty() => text.to_string(),
        keys => format!("{text} ({keys})"),
    }
}

#[cfg(test)]
mod tests {
    use super::{SHORTCUTS, Scope, display, terminal_binds};

    #[test]
    fn ids_are_unique_and_labels_read() {
        let mut ids: Vec<_> = SHORTCUTS.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), SHORTCUTS.len());
        if !cfg!(target_os = "macos") {
            assert_eq!(display("ctrl-shift-n"), "Ctrl+Shift+N");
            assert_eq!(display("alt-down"), "Alt+Down");
            assert_eq!(display("ctrl-pagedown"), "Ctrl+PgDn");
            assert_eq!(display("ctrl--"), "Ctrl+-");
        }
    }

    /// No two shortcuts on one key where both work, and none bound in a
    /// terminal over a key the terminal binds for itself.
    #[test]
    fn no_shortcut_takes_another_s_key() {
        use Scope::*;
        // Where each binds: outside everything (None), or in a context.
        let places = |scope: Scope| -> &'static [&'static str] {
            match scope {
                Everywhere => &["", "Terminal"],
                NotInTerminal | NotInTerminalOrPage | App => &[""],
                Editor => &["Input"],
                File => &["Input", "Preview"],
                Terminal => &["Terminal"],
                Review => &["Review"],
                Browser => &["Browser"],
                Picture => &["Picture"],
                Sql => &["Sql"],
                Http => &["Http"],
            }
        };
        let keyed: Vec<_> = SHORTCUTS.iter().filter(|s| !s.default.is_empty()).collect();
        for (i, a) in keyed.iter().enumerate() {
            for b in &keyed[i + 1..] {
                if a.default != b.default || a.action().partial_eq(&*b.action()) {
                    continue;
                }
                let shared = places(a.scope).iter().any(|p| places(b.scope).contains(p));
                // Over the editor's keys, a SQL file's own: as they are meant.
                let meant = matches!((a.scope, b.scope), (Sql | Http, Editor) | (Editor, Sql | Http) | (Browser | Picture, _) | (_, Browser | Picture));
                assert!(!shared || meant, "{} and {} are both on {}", a.id, b.id, a.default);
            }
        }
        // Meant to be over a terminal's own: the palette, as JetBrains' Find
        // Action, works everywhere; the terminal's Select All (Ctrl+Shift+A on
        // Linux and Windows) is seldom wanted.
        const OVER_THE_TERMINAL: &[&str] = &["command_palette"];
        for s in SHORTCUTS.iter().filter(|s| s.scope == Everywhere && !s.default.is_empty() && !OVER_THE_TERMINAL.contains(&s.id)) {
            let keys = super::keys::key(s.default);
            let binding = gpui_kit::KeyBinding::new(Box::leak(keys.into_boxed_str()), gpui_kit::NoAction, None);
            assert!(!terminal_binds(binding.keystrokes()), "{} takes a terminal's own key", s.id);
        }
    }
}
