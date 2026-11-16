use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_cn::terminal::{
    LocalTerminalOptions, Terminal, TerminalConfig, TerminalEvent, TerminalState, WorkingDirectory,
};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, StyledExt as _, h_flex,
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffAnnotation, DiffEvent, DiffFile, DiffFileStatus, DiffLinePosition, DiffLineRange, DiffMode, DiffSide, DiffState},
    input::{CodeActionProvider, CompletionProvider, DefinitionProvider, DocumentRangeSemanticTokensProvider, Editor, EditorState, HoverProvider, InputEvent, Position, Rope, RopeExt as _},
    resizable::{h_resizable, resizable_panel},
    tab::{Tab, TabBar},
    list::ListItem,
    tree::{TreeEvent, TreeItem, TreeState, tree},
    Disableable as _, IconName, Selectable as _, WindowExt as _,
    menu::{ContextMenuExt as _, DropdownMenu as _},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use notify::{RecursiveMode, Watcher as _};

use crate::{git, history::HistoryView, lsp};
use gpui_kit::component::notification::Notification;

mod agent_tab;
mod lookups;
mod branches;
mod refresh;
mod nav;
mod branch_bar;
mod open;
mod panels;
mod git_ui;
mod helpers;
mod term;
mod file;
mod changes;
mod session;
mod change_tree;
mod services;
mod pr;
mod replace;
mod annotate;
mod bookmarks;
mod file_tree;
use file_tree::*;
use git_ui::*;
pub use helpers::*;
// Named, to come before the one of the same name in gpui's own glob.
use helpers::outline;
pub use term::*;
pub use file::*;
pub use changes::*;
pub use session::*;
pub use annotate::ToggleBlame;
pub use bookmarks::{NextBookmark, PrevBookmark, ShowBookmarks, ToggleBookmark};
use change_tree::*;
use pr::*;

// ---------------------------------------------------------------- task

enum TabKind {
    Terminal(Entity<TermView>),
    Changes,
    History,
    File(Entity<FileView>),
    Browser(Entity<crate::browser::BrowserView>),
    /// A file that cannot be shown as text: binary, or unreadable.
    Binary(Entity<BinaryView>),
}

pub struct BinaryView {
    path: PathBuf,
    reason: String,
}

impl Render for BinaryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path = self.path.clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(div().text_lg().child(
                self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            ))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.reason.clone()))
            .child(
                Button::new("open-system")
                    .small()
                    .outline()
                    .label("Open in the system app")
                    .on_click(move |_, _, cx| cx.open_with_system(&path)),
            )
    }
}

/// Set when a tab drag starts. Pane dividers drag too (gpui's drag-and-drop
/// does not say what is dragged), and those must not show the drop zones.
static TAB_DRAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn tab_drag_active(cx: &App) -> bool {
    if !cx.has_active_drag() {
        TAB_DRAG.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    cx.has_active_drag() && TAB_DRAG.load(std::sync::atomic::Ordering::Relaxed)
}

/// A tab being dragged to the other pane; also its drag preview.
#[derive(Clone)]
struct DraggedTab {
    tab_id: usize,
    title: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .text_sm()
            .rounded_md()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().primary)
            .child(self.title.clone())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum RightPane {
    Changes,
    Files,
    /// The declarations of the file last in view.
    Structure,
    /// The task's services (`.ideae.toml`): start, stop, open.
    Run,
}

const NAV_LIMIT: usize = 100;
/// How far down the viewport a jump puts its line, unless already in view.
const JUMP_AT: f32 = 1. / 3.;

/// A place in the navigation history. Files go by path, so a closed one
/// opens again; other tabs by id, and are skipped once closed.
#[derive(Clone, Debug)]
enum NavPoint {
    File { path: PathBuf, line: u32, col: u32 },
    Tab(usize),
}

impl NavPoint {
    /// The same line counts as the same place: a step back should go somewhere.
    fn same_place(&self, other: &NavPoint) -> bool {
        match (self, other) {
            (NavPoint::File { path: a, line: la, .. }, NavPoint::File { path: b, line: lb, .. }) => a == b && la == lb,
            (NavPoint::Tab(a), NavPoint::Tab(b)) => a == b,
            _ => false,
        }
    }
}

struct TabItem {
    /// Stable across inserts and removals, unlike the index.
    id: usize,
    title: SharedString,
    kind: TabKind,
    /// In the right pane; every tab is in the left one when not split.
    right: bool,
    /// What the tab's label needs of its view, kept here: the task is not
    /// to be drawn again whenever a terminal or a page draws a frame, as it
    /// would be if drawing it read them. The terminal's or the page's
    /// title, whether the file has unsaved edits, the service it runs.
    shown: SharedString,
    dirty: bool,
    service: Option<String>,
    /// A file opened to look at, not to work in: the next one opened that
    /// way takes its place. Editing it, double-clicking it or moving it to
    /// the other pane keeps it.
    preview: bool,
    /// Pinned: left alone by Close others and the like, by the preview tab
    /// and by the limit of file tabs.
    pinned: bool,
    /// When it was last brought to the front, for closing the file tabs
    /// left longest when there are more than the limit.
    used: u64,
}

fn use_tick() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl TabItem {
    fn new(title: impl Into<SharedString>, kind: TabKind) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
        Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            title: title.into(),
            kind,
            right: false,
            shown: SharedString::default(),
            dirty: false,
            service: None,
            preview: false,
            pinned: false,
            used: use_tick(),
        }
    }
}

struct ClosedTab {
    path: PathBuf,
    line: u32,
    col: u32,
    right: bool,
}

/// How a file is opened: to look at (a jump, a search result, a click in
/// the tree), to keep, or back from the last run, when its tabs are not to
/// be closed for the limit while they are coming back.
#[derive(Clone, Copy, PartialEq)]
pub enum OpenAs {
    Preview,
    Kept,
    Restored,
}

struct ChangedFile {
    path: SharedString,
    /// Of its part of the diff, to tell whether it changed since viewed.
    hash: u64,
    first_line: u32,
    status: DiffFileStatus,
    additions: usize,
    deletions: usize,
}

pub enum TaskEvent {
    Changed,
}

impl EventEmitter<TaskEvent> for TaskView {}

pub struct TaskView {
    pub root: PathBuf,
    pub branch: SharedString,
    pub agent_status: crate::agent::AgentStatus,
    /// What the working agent does now: its latest tool call, in words.
    pub agent_activity: Option<String>,
    /// The file the agent is writing to now, and those it wrote to since
    /// the last prompt: marked in the Changes and Files panels.
    agent_editing: Option<SharedString>,
    agent_touched: Rc<HashSet<SharedString>>,
    /// The paths of `files`, for the tree's marks.
    changed_paths: Rc<HashSet<SharedString>>,
    /// The command that starts the agent, for Restart.
    agent: Option<String>,
    /// Text for the agent's prompt once it is ready to take it, and whether
    /// it is sent too.
    pending_prompt: Option<(String, bool)>,
    /// A message for an agent that was busy: sent when its turn ends.
    queued_message: Option<String>,
    /// Changed files you marked viewed, with their diff's hash then: a new
    /// change to one unmarks it.
    viewed: BTreeMap<String, u64>,
    /// The bookmarks of files not open, by their path in the worktree.
    bookmarks: BTreeMap<String, Vec<u32>>,
    /// Files you were in, the latest first, with where the cursor was.
    recent: Vec<(PathBuf, u32, u32)>,
    /// Its pull request, as last seen on GitHub.
    pub pr: Option<crate::pr::Pr>,
    /// Its share of the ports of `.ideae.toml`'s services, and those services.
    pub port_slot: u32,
    services: Vec<crate::services::Service>,
    /// Why `.ideae.toml` could not be read, and what the project seems to
    /// run, for the Run panel; read when it is shown.
    services_error: Option<String>,
    services_found: Vec<crate::services::Found>,
    /// Ports the task's processes listen on, while the Run panel shows.
    listening: Vec<crate::services::Listening>,
    _listening_poll: Option<Task<()>>,
    hook_socket: Option<String>,
    /// Numbers terminals so each has a unique element id and default title.
    terminal_seq: usize,
    /// Status changed to one that wants attention while the task was not in view.
    pub unseen: bool,
    tabs: Vec<TabItem>,
    /// The tab shown in each pane, by id. `right_active` is None unless split.
    left_active: usize,
    right_active: Option<usize>,
    /// The pane you work in: new tabs open there.
    focus_right: bool,
    /// Focus mode: this pane (right or not) fills the window alone.
    zoomed: Option<bool>,
    last_tab_click: Option<(usize, std::time::Instant)>,
    /// The tab a right click opened the menu of; none for the bar around them.
    menu_tab: Rc<std::cell::Cell<Option<usize>>>,
    /// Files whose tabs were closed, latest last, for Reopen closed tab.
    closed: Vec<ClosedTab>,
    /// Held when the tab in view takes no focus of its own (Changes, History,
    /// a placeholder), so keys such as Shift+Esc still reach the task.
    focus: FocusHandle,
    changes: Entity<ChangesView>,
    history: Entity<HistoryView>,
    files: Vec<ChangedFile>,
    /// Parsed diffs of both scopes; the Changes tab shows one of them.
    uncommitted_diff: Vec<DiffFile>,
    branch_diff: Vec<DiffFile>,
    /// The first changed line of each file, per scope, for "open".
    uncommitted_lines: Rc<HashMap<String, u32>>,
    branch_lines: Rc<HashMap<String, u32>>,
    /// Each file's part of the patch, hashed: what tells the diff view which
    /// files are as they were.
    uncommitted_keys: Rc<HashMap<String, u64>>,
    branch_keys: Rc<HashMap<String, u64>>,
    /// The files the branch changed since its base, lock files aside.
    branch_paths: Rc<BTreeSet<String>>,
    /// The single changes of each, to take back or stage one alone.
    uncommitted_hunks: Rc<Vec<git::Hunk>>,
    /// Files with conflict markers left in them, and whether committing
    /// them as they are was asked for a second time.
    conflict_marked: Vec<(String, u32)>,
    markers_accepted: bool,
    /// Work put away in the repository, the latest first.
    stashes: Vec<git::Stash>,
    branch_hunks: Rc<Vec<git::Hunk>>,
    /// The Changes tab shows an older diff than the one read last.
    diff_stale: bool,
    branch_info: git::BranchInfo,
    right: RightPane,
    /// The file the Structure panel shows (tab id), and its outline cached by
    /// (tab id, edit version).
    structure_file: Option<usize>,
    /// Keyboard focus of the side panel (Alt+1/3), and the row the arrows are on.
    side_focus: FocusHandle,
    side_selected: usize,
    /// Where the Changes and Structure panels are scrolled to.
    changes_scroll: UniformListScrollHandle,
    structure_scroll: UniformListScrollHandle,
    /// Whether the side panel holds the keyboard, set each render.
    side_has_focus: std::cell::Cell<bool>,
    structure: std::cell::RefCell<Option<(usize, u64, Rc<Vec<crate::finder::OutlineItem>>)>>,
    /// The outline being made, by what it is of.
    structure_job: std::cell::RefCell<(Option<(usize, u64)>, Task<()>)>,
    tree: Entity<TreeState>,
    /// The file list the tree was built from, and the folders open in it.
    tree_files: Arc<Vec<String>>,
    /// The tree is of an older list: it is built when the Files panel shows.
    tree_stale: bool,
    /// The same list for definition lookups running off the UI thread.
    shared_files: Arc<std::sync::RwLock<Arc<Vec<String>>>>,
    expanded: HashSet<SharedString>,
    /// The tree's colors: what git says of each changed file and folder.
    tree_marks: Rc<HashMap<String, Mark>>,
    /// What `.gitignore` keeps out, as git status names it.
    tree_ignored: Rc<Vec<String>>,
    /// The ignored folders open in the tree, as read from disk.
    ignored_listed: HashMap<SharedString, Listing>,
    ignored_reading: HashSet<SharedString>,
    /// Deleted, and still known to git: left out of the tree.
    tree_gone: Rc<HashSet<String>>,
    /// Presenting with ignored files hidden: none in the tree.
    tree_hides_ignored: bool,
    /// Folders folded in the Changes tree, as "staged:dir" / "changes:dir".
    folded_changes: HashSet<String>,
    /// Out of view long enough (or never shown): no language servers run;
    /// they start when the task comes into view.
    pub lsp_asleep: bool,
    file_states: HashMap<String, git::FileState>,
    commit_input: Entity<gpui_kit::component::input::TextareaState>,
    /// The git/gh action running, and the outcome of the last one.
    busy: Option<&'static str>,
    /// The file the last Discard threw away, for Undo.
    /// It is taken where the discard runs, off the UI thread.
    discard_undo: Option<Arc<std::sync::Mutex<Option<git::FileSnapshot>>>>,
    action: Option<(bool, String)>,
    /// Wakes the refresh loop: file events, actions, the fallback tick.
    refresh_tx: async_channel::Sender<()>,
    /// The branch "Branch vs …" compares with, when picked by hand; shared
    /// with the refresh loop.
    base_override: Arc<std::sync::Mutex<Option<String>>>,
    /// Kept for as long as the task: the refresh, off the UI thread, tells
    /// it what to watch.
    _watching: Arc<std::sync::Mutex<Watching>>,
    /// One server per (program, project root).
    lsp: HashMap<(&'static str, PathBuf), Arc<lsp::LspClient>>,
    /// Since when no open file uses a server.
    lsp_unused: HashMap<(&'static str, PathBuf), std::time::Instant>,
    /// Where you were before each jump, for Back and Forward.
    nav_back: Vec<NavPoint>,
    nav_forward: Vec<NavPoint>,
    navigating: bool,
    visible: bool,
    _poll: Task<()>,
}

impl TaskView {
    pub fn new(
        root: PathBuf,
        branch: String,
        agent: Option<String>,
        hook_socket: Option<String>,
        port_slot: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let services = crate::services::load(&root).unwrap_or_default();
        let task_env = crate::services::task_env(&services, port_slot);
        let weak = cx.weak_entity();
        let changes = cx.new(|cx| ChangesView::new(root.clone(), weak.clone(), cx));
        let history = cx.new(|cx| HistoryView::new(root.clone(), weak, cx));
        let tree = cx.new(|cx| TreeState::new(cx));
        cx.subscribe(&tree, |this: &mut Self, _, event: &TreeEvent, cx| match event {
            TreeEvent::Expanded(id) => this.tree_expanded(id, cx),
            TreeEvent::Collapsed(id) => this.tree_collapsed(id),
        })
        .detach();

        let mut tabs = Vec::new();
        // The agent's terminal, or a shell when there is no agent; more shells
        // are a "+" away.
        match &agent {
            Some(agent) => tabs.push(Self::make_terminal(&root, hook_socket.as_ref(), "agent", 0, Some(agent.clone()), task_env.clone(), None, window, cx)),
            None => tabs.push(Self::make_terminal(&root, hook_socket.as_ref(), "shell", 1, None, task_env.clone(), None, window, cx)),
        }
        // Changes and History open on demand: the Log button, a changed file,
        // Ctrl+2/3.

        let (refresh_tx, refresh_rx) = async_channel::unbounded::<()>();
        let watch_tx = refresh_tx.clone();
        let filter: Arc<std::sync::RwLock<WatchFilter>> = Arc::default();
        let event_filter = filter.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                let wanted = !whole_trees() || event.paths.is_empty() || {
                    let filter = event_filter.read().unwrap();
                    event.paths.iter().any(|path| filter.lets_through(path))
                };
                if wanted && !matches!(event.kind, notify::EventKind::Access(_)) {
                    let _ = watch_tx.try_send(());
                }
            }
        })
        .inspect_err(|e| eprintln!("file watcher off, polling only: {e}"))
        .ok();
        let watching = Arc::new(std::sync::Mutex::new(Watching::new(watcher, filter)));
        let poll_watching = watching.clone();
        // Fallback for anything the watcher misses (new directories, remotes).
        // Out of view a task is looked at once a minute: every open task pays
        // for its refresh with a handful of git processes.
        let tick_tx = refresh_tx.clone();
        cx.spawn(async move |this, cx| {
            let mut n = 0u32;
            loop {
                cx.background_executor().timer(Duration::from_secs(10)).await;
                n += 1;
                let Ok(visible) = this.read_with(cx, |t, _| t.visible) else { break };
                if (visible || n % 6 == 0) && tick_tx.send(()).await.is_err() {
                    break;
                }
            }
        })
        .detach();

        let poll_root = root.clone();
        let base_override: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
        let poll_base = base_override.clone();
        let poll = cx.spawn_in(window, async move |this, cx| {
            // The patches shown, by hash: only a new one is parsed.
            let mut seen = (text_hash(""), text_hash(""));
            let mut known = git::Known::default();
            loop {
                let root = poll_root.clone();
                let picked = poll_base.lock().unwrap().clone();
                let Ok((open, open_known)) = this.update(cx, |this, cx| this.open_files_known(cx)) else { break };
                let Ok(light) = this.read_with(cx, |this, _| !this.visible) else { break };
                let watching = poll_watching.clone();
                let (read, patch, branch, shared, staged, on_disk, now_seen, now_known) = cx
                    .background_spawn(async move {
                        let mut read = git::refresh(&root, picked, light, &mut known);
                        // Here, not on the UI thread: a call for every directory that came or went.
                        watching.lock().unwrap().sync(std::mem::take(&mut read.watch));
                        // None of them when the patches are those of the refresh before.
                        let (patch, branch, shared, staged) = match read.patches.take() {
                            Some(git::Patches { working, branch, staged }) => {
                                // Nothing committed on the branch yet: one patch, parsed
                                // once and kept once (`shared`).
                                let same = branch == working;
                                let patch = parse_patch(working, &mut seen.0);
                                let (branch, shared) = match &patch {
                                    Some(_) if same => {
                                        seen.1 = seen.0;
                                        (None, true)
                                    }
                                    _ => (parse_patch(branch, &mut seen.1), false),
                                };
                                let staged: HashSet<(String, String)> = git::hunks(&staged).into_iter().map(|hunk| (hunk.path, hunk.changes)).collect();
                                (patch, branch, shared, Some(staged))
                            }
                            None => (None, None, false, None),
                        };
                        (read, patch, branch, shared, staged, changed_on_disk(open_known), seen, known)
                    })
                    .await;
                let git::Refreshed { files, ignored, info, states, base, stashes, failed, .. } = read;
                known = now_known;
                seen = now_seen;
                let applied = this.update_in(cx, |this, window, cx| {
                    for (file, change) in open.iter().zip(on_disk) {
                        if let Some(change) = change {
                            let _ = file.update(cx, |f, cx| f.apply_disk(change, window, cx));
                        }
                    }
                    // Git did not answer: what was read before stays, rather
                    // than an empty tree and no changes in its place.
                    if failed {
                        return;
                    }
                    if this.file_states != states {
                        this.file_states = states;
                        if this.mark_tree() {
                            this.tree_stale = true;
                            this.fill_tree(cx);
                        }
                        cx.notify();
                    }
                    if this.stashes != stashes {
                        this.stashes = stashes;
                        cx.notify();
                    }
                    if let Some(patch) = patch {
                        this.apply_patch(patch, cx);
                    }
                    if let Some(staged) = staged.filter(|staged| this.changes.read(cx).staged.as_ref() != staged) {
                        this.changes.update(cx, |changes, cx| {
                            changes.staged = Rc::new(staged);
                            cx.notify();
                        });
                    }
                    this.apply_branch_patch(base, branch, shared, cx);
                    this.apply_files(files, cx);
                    this.apply_ignored(ignored, cx);
                    this.apply_branch(info, cx);
                });
                if applied.is_err() {
                    break;
                }
                // Wait for a change, then let a burst of events settle. A
                // task out of view (its agent writing away) waits longer,
                // unless it comes into view meanwhile.
                if refresh_rx.recv().await.is_err() {
                    break;
                }
                for _ in 0..HIDDEN_REFRESH_WAITS {
                    cx.background_executor().timer(Duration::from_millis(150)).await;
                    if !matches!(this.read_with(cx, |t, _| t.visible), Ok(false)) {
                        break;
                    }
                }
                while refresh_rx.try_recv().is_ok() {}
            }
        });

        // Grows with the message, up to eight lines.
        let commit_input = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx).auto_grow(1, 8).placeholder("Commit message")
        });

        let mut this = Self {
            root,
            branch: branch.into(),
            agent_status: crate::agent::AgentStatus::Unknown,
            agent_activity: None,
            agent_editing: None,
            agent_touched: Rc::default(),
            changed_paths: Rc::default(),
            agent: agent.clone(),
            pending_prompt: None,
            queued_message: None,
            recent: Vec::new(),
            pr: None,
            viewed: BTreeMap::new(),
            bookmarks: BTreeMap::new(),
            port_slot,
            services,
            services_error: None,
            services_found: Vec::new(),
            listening: Vec::new(),
            _listening_poll: None,
            hook_socket,
            terminal_seq: 1,
            unseen: false,
            tabs,
            left_active: 0,
            right_active: None,
            focus_right: false,
            zoomed: None,
            last_tab_click: None,
            menu_tab: Rc::default(),
            closed: Vec::new(),
            focus: cx.focus_handle(),
            changes,
            history,
            files: Vec::new(),
            uncommitted_diff: Vec::new(),
            branch_diff: Vec::new(),
            uncommitted_lines: Rc::default(),
            branch_lines: Rc::default(),
            uncommitted_keys: Rc::default(),
            branch_keys: Rc::default(),
            branch_paths: Rc::default(),
            uncommitted_hunks: Rc::default(),
            conflict_marked: Vec::new(),
            markers_accepted: false,
            stashes: Vec::new(),
            branch_hunks: Rc::default(),
            diff_stale: false,
            branch_info: git::BranchInfo::default(),
            right: RightPane::Changes,
            structure_file: None,
            side_focus: cx.focus_handle(),
            side_selected: 0,
            changes_scroll: UniformListScrollHandle::new(),
            structure_scroll: UniformListScrollHandle::new(),
            side_has_focus: Default::default(),
            structure: Default::default(),
            structure_job: std::cell::RefCell::new((None, Task::ready(()))),
            tree,
            tree_files: Arc::default(),
            tree_stale: false,
            shared_files: Arc::default(),
            expanded: HashSet::new(),
            tree_marks: Rc::default(),
            tree_ignored: Rc::default(),
            ignored_listed: HashMap::new(),
            ignored_reading: HashSet::new(),
            tree_gone: Rc::default(),
            tree_hides_ignored: false,
            folded_changes: HashSet::new(),
            lsp_asleep: false,
            file_states: HashMap::new(),
            commit_input,
            busy: None,
            discard_undo: None,
            action: None,
            refresh_tx,
            base_override,
            _watching: watching,
            lsp: HashMap::new(),
            lsp_unused: HashMap::new(),
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
            navigating: false,
            visible: false,
            _poll: poll,
        };
        // Debug aid for headless screenshots: start on a given tab.
        let first = std::env::var("IDEAE_TAB").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        this.left_active = this.tabs.get(first).or(this.tabs.first()).map_or(0, |t| t.id);
        this.sync_visibility(cx);
        this
    }

    fn make_terminal(
        root: &PathBuf,
        hook_socket: Option<&String>,
        title: &str,
        seq: usize,
        input: Option<String>,
        extra_env: Vec<(String, String)>,
        service: Option<(String, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TabItem {
        let mut env: Vec<(String, String)> = extra_env;
        // Every terminal of the task can reach the hook socket, so a claude
        // started by hand reports too once given the settings file.
        if let Some(socket) = hook_socket {
            env.push((crate::agent::SOCK_ENV.into(), socket.clone()));
            env.push((crate::agent::TASK_ENV.into(), root.display().to_string()));
        }
        // Set either way: an ideae started from an agent's terminal inherits "agent".
        let role = if service.is_some() {
            "service"
        } else if input.is_some() {
            "agent"
        } else {
            "terminal"
        };
        let cwd = service.as_ref().map_or_else(|| root.clone(), |(_, cwd)| cwd.clone());
        env.push((crate::agent::ROLE_ENV.into(), role.into()));
        // Enter is a carriage return; PowerShell's line editor takes a bare
        // newline for a line break inside the command and waits for more.
        let enter = if cfg!(windows) { "\r" } else { "\n" };
        let input = input.map(|input| format!("{input}{enter}"));
        let config = TerminalConfig::default().with_colors(crate::theme::terminal_colors(cx));
        // In the daemon the program outlives the window; a restart attaches
        // to it again under the same session name.
        let session = format!(
            "{}#{}",
            root.display(),
            match (&service, role) {
                (Some((name, _)), _) => format!("service-{name}"),
                (None, "agent") => "agent".to_string(),
                _ => format!("shell-{seq}"),
            }
        );
        #[cfg(unix)]
        let daemon = crate::daemon::client::get();
        #[cfg(not(unix))]
        let daemon: Option<()> = None;
        #[cfg(not(unix))]
        let _ = &session;
        let (state, session) = match daemon {
            #[cfg(unix)]
            Some(daemon) => {
                let spec = crate::daemon::proto::Spec { cwd: cwd.clone(), env, input };
                let id = session.clone();
                (cx.new(|cx| daemon.terminal(id, spec, config, window, cx)), Some(session))
            }
            _ => {
                let mut options = LocalTerminalOptions::default().with_cwd(WorkingDirectory::Path(cwd.clone()));
                for (name, value) in env {
                    options = options.with_env(name, value);
                }
                if let Some(input) = input {
                    options = options.with_input(input);
                }
                (cx.new(|cx| TerminalState::local(options, config, window, cx)), None)
            }
        };
        cx.subscribe_in(&state, window, |this: &mut Self, state, event: &TerminalEvent, window, cx| match event {
            // A shell that exits (Ctrl+D, `exit`) takes its tab with it, as in a
            // terminal app; the agent's tab stays and shows the exit.
            TerminalEvent::Exited(_) => {
                let ix = this.tabs.iter().position(|t| match &t.kind {
                    TabKind::Terminal(v) => v.read(cx).state == *state,
                    _ => false,
                });
                match ix {
                    Some(ix) if this.tabs[ix].title != "agent" => {
                        // Its thread and what it kept of the screen go with the tab.
                        state.update(cx, |state, cx| state.close(cx));
                        this.remove_tab(ix, window, cx)
                    }
                    Some(_) => this.agent_status = crate::agent::AgentStatus::Exited,
                    None => {}
                }
                cx.emit(TaskEvent::Changed);
                cx.notify();
            }
            // Claude Code that crashed or was killed sends no SessionEnd.
            TerminalEvent::ForegroundChanged => {
                let ended = this.tabs.iter().any(|t| match &t.kind {
                    TabKind::Terminal(v) if t.title == "agent" && v.read(cx).state == *state => v.read(cx).agent_ended(cx),
                    _ => false,
                });
                if ended && this.agent_status != crate::agent::AgentStatus::Exited {
                    this.agent_status = crate::agent::AgentStatus::Exited;
                    cx.emit(TaskEvent::Changed);
                    cx.notify();
                }
            }
            TerminalEvent::TitleChanged | TerminalEvent::Bell => {
                let title = state.read(cx).title().clone();
                if let Some(tab) = this.tabs.iter_mut().find(|t| matches!(&t.kind, TabKind::Terminal(v) if v.read(cx).state == *state)) {
                    tab.shown = title;
                }
                cx.emit(TaskEvent::Changed);
                cx.notify();
            }
            TerminalEvent::OpenPath(target) => {
                let cwd = state.read(cx).cwd().map(|p| p.to_path_buf());
                this.open_terminal_path(target, cwd, window, cx);
            }
            _ => {}
        })
        .detach();
        let id = SharedString::from(format!("{}-term-{seq}", root.display()));
        let task = (role == "agent").then(|| cx.weak_entity());
        let service_name = service.map(|(name, _)| name);
        let shown = state.read(cx).title().clone();
        let view = cx.new(|cx| TermView::new(id, state, session, seq, task, service_name.clone(), cx));
        TabItem { shown, service: service_name, ..TabItem::new(title.to_string(), TabKind::Terminal(view)) }
    }

    fn ix_of(&self, id: usize) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn is_split(&self) -> bool {
        self.tabs.iter().any(|t| t.right)
    }

    /// Keeps the panes consistent after tabs move or close: an empty left pane
    /// takes the right one's tabs (the split closes), and each pane shows one
    /// of its own tabs.
    fn normalize(&mut self) {
        if self.tabs.iter().all(|t| t.right) {
            for tab in &mut self.tabs {
                tab.right = false;
            }
        }
        let shows = |id: Option<usize>, right: bool, tabs: &[TabItem]| {
            id.is_some_and(|id| tabs.iter().any(|t| t.id == id && t.right == right))
        };
        if !shows(Some(self.left_active), false, &self.tabs) {
            self.left_active = self.tabs.iter().find(|t| !t.right).map_or(0, |t| t.id);
        }
        if !shows(self.right_active, true, &self.tabs) {
            self.right_active = self.tabs.iter().find(|t| t.right).map(|t| t.id);
        }
        if self.right_active.is_none() {
            self.focus_right = false;
        }
    }

    /// Moves a tab to the other pane and shows it there, splitting if needed.
    fn move_to_other_pane(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(ix) else { return };
        // The only tab on the left cannot leave it: there would be nothing to split from.
        if !tab.right && self.tabs.iter().filter(|t| !t.right).count() < 2 {
            return;
        }
        let tab = &mut self.tabs[ix];
        tab.right = !tab.right;
        tab.preview = false;
        let (id, right) = (tab.id, tab.right);
        if right {
            self.right_active = Some(id);
        } else {
            self.left_active = id;
        }
        self.focus_right = right;
        self.normalize();
        self.sync_visibility(cx);
        self.show_diff_if_seen(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The right pane's tabs all go back to the left one.
    fn join_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            tab.right = false;
        }
        self.normalize();
        self.sync_visibility(cx);
        self.show_diff_if_seen(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The zoomed pane, for the window to float over everything. While it is
    /// up, the task's own layout leaves that pane's slot empty: a view (a
    /// terminal above all) is drawn in one place only.
    pub fn render_zoomed(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let right = self.zoomed?;
        Some(
            div()
                .size_full()
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &ToggleZoom, window, cx| this.toggle_zoom_focused(window, cx)))
                .on_action(cx.listener(|this, _: &ShowOutline, window, cx| this.show_outline(window, cx)))
                .on_action(cx.listener(|this, _: &ShowBookmarks, window, cx| this.show_bookmarks(window, cx)))
                .on_action(cx.listener(|this, _: &ShowUsages, window, cx| this.show_usages_at_cursor(window, cx)))
                .on_action(cx.listener(|this, _: &SendToAgent, window, cx| this.send_editor_selection(window, cx)))
                .child(self.render_pane(right, cx))
                .into_any_element(),
        )
    }

    /// Focus mode for a pane, like Zed's: it takes the whole window, over the
    /// sidebars and the other pane. The same gesture brings everything back.
    /// Shift+Esc: focus mode for the pane you are in.
    pub fn toggle_zoom_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let right = self.focus_right;
        self.toggle_zoom(right, window, cx)
    }

    fn toggle_zoom(&mut self, right: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.zoomed = if self.zoomed.is_some() { None } else { Some(right && self.is_split()) };
        self.focus_right = right && self.is_split();
        self.focus_active(window, cx);
        cx.emit(TaskEvent::Changed);
        cx.notify();
    }

    fn focus_pane(&mut self, right: bool, cx: &mut Context<Self>) {
        if self.focus_right != right && (!right || self.is_split()) {
            self.focus_right = right;
            self.autosave_unfocused(cx);
            cx.notify();
        }
    }

    fn tab_content(&self, ix: Option<usize>) -> AnyElement {
        match ix.and_then(|ix| self.tabs.get(ix)).map(|t| &t.kind) {
            Some(TabKind::Terminal(view)) => view.clone().into_any_element(),
            Some(TabKind::Changes) => self.changes.clone().into_any_element(),
            Some(TabKind::History) => self.history.clone().into_any_element(),
            Some(TabKind::File(file)) => file.clone().into_any_element(),
            Some(TabKind::Browser(view)) => view.clone().into_any_element(),
            Some(TabKind::Binary(view)) => view.clone().into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// A tab dropped on a pane moves there (or just shows, if already there).
    fn drop_tab(&mut self, tab_id: usize, right: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.ix_of(tab_id) else { return };
        if self.tabs[ix].right == right {
            self.activate(ix, window, cx);
        } else {
            self.move_to_other_pane(ix, window, cx);
        }
    }

    /// One pane: its own tab bar over the tab it shows.
    fn render_pane(&self, right: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.is_split() && self.focus_right == right;
        let shown = if right { self.right_active } else { Some(self.left_active) };
        let highlight = cx.theme().primary.opacity(0.18);
        // While a tab is dragged, a transparent layer over the pane takes the
        // drop and shows the highlight: content such as a terminal paints an
        // opaque background over anything set on the pane itself. Unsplit,
        // the right edge is a separate zone that opens the right pane.
        let dragging = tab_drag_active(cx);
        let split_zone = !self.is_split() && dragging;
        v_flex()
            .id(if right { "pane-right" } else { "pane-left" })
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| this.focus_pane(right, cx)))
            .child(
                div()
                    .border_t_2()
                    .border_color(if focused { cx.theme().primary } else { cx.theme().transparent })
                    .child(self.render_tabs(right, cx)),
            )
            .child(div().flex_1().min_h_0().child(self.tab_content(shown.and_then(|id| self.ix_of(id)))))
            .when(dragging, |pane| {
                pane.child(
                    div()
                        .id("drop-layer")
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(if split_zone { relative(0.7) } else { relative(1.) })
                        .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(highlight))
                        .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                            this.drop_tab(dragged.tab_id, right, window, cx)
                        })),
                )
            })
            .when(split_zone, |pane| {
                pane.child(
                    div()
                        .id("split-zone")
                        .absolute()
                        .top_0()
                        .right_0()
                        .h_full()
                        .w(relative(0.3))
                        .border_l_2()
                        .border_color(cx.theme().primary)
                        .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(highlight))
                        .on_drop(cx.listener(|this, dragged: &DraggedTab, window, cx| {
                            if let Some(ix) = this.ix_of(dragged.tab_id) {
                                this.move_to_other_pane(ix, window, cx);
                            }
                        }))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Drop to split"),
                )
            })
    }

    pub fn show_commit(&mut self, hash: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tab(|k| matches!(k, TabKind::History), window, cx);
        self.history.update(cx, |h, cx| h.select_hash(hash, cx));
    }

    /// Files dropped on the window from a file manager open as tabs.
    pub fn open_dropped(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        for path in paths.iter().filter(|p| p.is_file()) {
            self.open_file_as(path.clone(), 0, 0, OpenAs::Kept, window, cx);
        }
    }

    pub fn open_finder(&mut self, kind: crate::finder::FinderKind, window: &mut Window, cx: &mut Context<Self>) {
        let files = self.tree_files.clone();
        if kind == crate::finder::FinderKind::Symbols {
            let symbols = symbol_source(self.root.clone(), self.lsp.values().cloned().collect(), files);
            return crate::finder::Finder::open_symbols(self.root.clone(), symbols, cx.weak_entity(), window, cx);
        }
        crate::finder::Finder::open(kind, self.root.clone(), files, cx.weak_entity(), window, cx);
    }

    /// A browser tab in the pane you are in. Playwright MCP can drive the
    /// same page through the endpoint shown in its toolbar.
    pub fn new_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_browser("about:blank", window, cx);
    }

    pub fn open_browser(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| crate::browser::BrowserView::new(url, window, cx));
        cx.subscribe_in(&view, window, |this, view, event: &crate::browser::BrowserEvent, window, cx| match event {
            crate::browser::BrowserEvent::Navigated => {
                let title = view.read(cx).title();
                if let Some(tab) = this.tabs.iter_mut().find(|t| matches!(&t.kind, TabKind::Browser(v) if v == view)) {
                    tab.shown = title;
                }
                cx.notify()
            }
            crate::browser::BrowserEvent::Picked(message) => this.paste_to_agent(message.clone(), window, cx),
        })
        .detach();
        let shown = view.read(cx).title();
        let mut tab = TabItem { shown, ..TabItem::new("web", TabKind::Browser(view)) };
        tab.right = self.focus_right && self.is_split();
        self.tabs.push(tab);
        self.activate(self.tabs.len() - 1, window, cx);
    }

    /// A Ctrl+clicked `path[:line]` from terminal output: relative to the
    /// terminal's directory, else the worktree.
    fn open_terminal_path(&mut self, target: &str, cwd: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(symbol) = target.strip_prefix("symbol:") {
            return self.open_symbol(symbol, window, cx);
        }
        let (path, line) = match target.rsplit_once(':') {
            Some((path, line)) if line.parse::<u32>().is_ok() => (path, line.parse::<u32>().unwrap_or(1)),
            _ => (target, 1),
        };
        let path = path.strip_prefix("~/").map_or_else(
            || PathBuf::from(path),
            |rest| std::env::home_dir().unwrap_or_default().join(rest),
        );
        let candidates = if path.is_absolute() {
            vec![path.clone()]
        } else {
            cwd.into_iter().map(|d| d.join(&path)).chain([self.root.join(&path)]).collect()
        };
        let found = candidates.into_iter().find(|p| p.is_file()).or_else(|| {
            // Agents and compilers often print a bare name ("Providers.kt:554")
            // or a partial path: match the end of a project file's path.
            let needle = path.to_string_lossy().trim_start_matches("./").to_string();
            let suffix = format!("/{needle}");
            self.tree_files
                .iter()
                .filter(|f| **f == needle || f.ends_with(&suffix))
                .min_by_key(|f| f.len())
                .map(|f| self.root.join(f))
        });
        if let Some(found) = found {
            self.open_file(found, line.saturating_sub(1), window, cx);
        }
    }

    /// `Class.member` or `Class` from terminal output: a file of that name, or
    /// one named after the class (then the member by its outline), or the
    /// class's declaration found by ripgrep.
    fn open_symbol(&mut self, symbol: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (root, files, symbol) = (self.root.clone(), self.shared_files.clone(), symbol.to_string());
        cx.spawn_in(window, async move |this, cx| {
            // It searches the project: not on the UI thread.
            let places = cx
                .background_spawn(async move {
                    let files = files.read().unwrap().clone();
                    find_symbol(&root, &files, &symbol)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match places.len() {
                0 => {}
                1 => {
                    let (path, line, _) = places.into_iter().next().unwrap();
                    this.open_file(path, line, window, cx);
                }
                _ => {
                    let hits = places
                        .into_iter()
                        .map(|(path, line, code)| {
                            let shown = path.strip_prefix(&this.root).unwrap_or(&path).display().to_string();
                            crate::finder::Hit { text: format!("{}   {shown}", code.trim()), path: shown, line: line + 1, col: 0, ranges: Vec::new() }
                        })
                        .collect();
                    crate::finder::Finder::open_hits(this.root.clone(), hits, cx.weak_entity(), window, cx)
                }
            });
        })
        .detach();
    }

    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.new_terminal_tab(window, cx);
        self.tabs[at].right = self.focus_right && self.is_split();
        self.activate(at, window, cx);
    }

    /// Adds a shell tab after the last terminal and returns its index.
    fn new_terminal_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> usize {
        self.terminal_seq += 1;
        self.terminal_tab(self.terminal_seq, window, cx)
    }

    /// A shell tab numbered `seq`: a restored one gets its old session back.
    fn terminal_tab(&mut self, seq: usize, window: &mut Window, cx: &mut Context<Self>) -> usize {
        self.terminal_seq = self.terminal_seq.max(seq);
        let env = crate::services::task_env(&self.services, self.port_slot);
        let tab = Self::make_terminal(&self.root, self.hook_socket.as_ref(), &format!("shell {seq}"), seq, None, env, None, window, cx);
        // After the last terminal, so terminals stay together left of the views.
        let at = self.tabs.iter().rposition(|t| matches!(t.kind, TabKind::Terminal(_))).map_or(0, |ix| ix + 1);
        self.tabs.insert(at, tab);
        at
    }

    /// The agent terminal's title, which Claude Code uses as a status line.
    pub fn agent_title(&self, _: &App) -> Option<SharedString> {
        self.tabs.iter().find_map(|tab| match &tab.kind {
            TabKind::Terminal(_) if tab.title == "agent" => Some(tab.shown.clone()),
            _ => None,
        })
    }

    /// You left a file (another tab, the other pane): save it, JetBrains-style.
    /// The editor sends no blur events here, so the task tracks it instead.
    fn autosave_unfocused(&mut self, cx: &mut Context<Self>) {
        let focused = self.focused_ix().map(|ix| self.tabs[ix].id);
        for tab in &self.tabs {
            if let TabKind::File(file) = &tab.kind {
                if Some(tab.id) != focused {
                    file.update(cx, |f, cx| f.autosave(cx));
                }
            }
        }
    }

    /// The open files with edits not on disk, but for the one that asks
    /// (which is being updated, and cannot be read).
    fn unsaved_files(&self, but: EntityId, cx: &App) -> Vec<PathBuf> {
        self.tabs
            .iter()
            .filter_map(|tab| match &tab.kind {
                TabKind::File(file) if file.entity_id() != but => Some(file.read(cx)).filter(|file| file.dirty).map(|file| file.path.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn autosave_all(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if let TabKind::File(file) = &tab.kind {
                file.update(cx, |f, cx| f.autosave(cx));
            }
        }
    }

    /// As `autosave_all`, and on disk when it returns: the task or the app
    /// goes next.
    pub fn save_all_now(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if let TabKind::File(file) = &tab.kind {
                file.update(cx, |f, cx| f.save_now(cx));
            }
        }
    }

    pub fn set_terminal_colors(&mut self, colors: &gpui_cn::terminal::TerminalColors, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if let TabKind::Terminal(view) = &tab.kind {
                let state = view.read(cx).state.clone();
                state.update(cx, |s, cx| s.set_colors(colors.clone(), cx));
            }
        }
    }

    /// What of the agent's work the task and its row show, to tell a report
    /// that changes it from one that does not.
    pub fn agent_shown(&self) -> (Option<SharedString>, Option<SharedString>, usize) {
        (self.agent_activity.clone().map(SharedString::from), self.agent_editing.clone(), self.agent_touched.len())
    }

    pub fn set_agent_status(&mut self, status: crate::agent::AgentStatus, seen: bool, cx: &mut Context<Self>) {
        // An agent at work reports every tool it uses: drawn again only for
        // what shows another way than before.
        let before = (self.unseen, self.agent_activity.is_some(), self.agent_editing.is_some());
        self.unseen = status.is_attention() && !seen;
        if status != crate::agent::AgentStatus::Working {
            self.agent_activity = None;
            self.agent_editing = None;
        }
        let changed = status != self.agent_status || before != (self.unseen, self.agent_activity.is_some(), self.agent_editing.is_some());
        self.agent_status = status;
        if changed {
            cx.emit(TaskEvent::Changed);
            cx.notify();
        }
    }

    /// Keeps up with what the agent writes: `file` is what a tool call is
    /// about to change. A new prompt starts the turn's list over.
    pub fn follow_agent(&mut self, event: &str, file: Option<&str>) {
        match event {
            "UserPromptSubmit" => {
                self.agent_touched = Rc::default();
                self.agent_editing = None;
            }
            "PreToolUse" => {
                self.agent_editing = file.and_then(|file| path_in(&self.root, std::path::Path::new(file))).map(SharedString::from);
                Rc::make_mut(&mut self.agent_touched).extend(self.agent_editing.clone());
            }
            _ => {}
        }
    }

    /// "editing" by the file the agent writes to now, a dot by those it
    /// wrote to since the last prompt.
    fn agent_mark(&self, path: &str) -> Option<Div> {
        let editing = self.agent_editing.as_deref() == Some(path);
        (editing || self.agent_touched.contains(path))
            .then(|| div().text_xs().text_color(gpui_kit::blue()).child(if editing { "editing" } else { "●" }))
    }

    /// The open files look again at what they differ from: after the
    /// setting of what the gutter compares with changed.
    pub fn reload_gutters(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if let TabKind::File(file) = &tab.kind {
                file.update(cx, |f, cx| f.load_blame(cx));
            }
        }
    }

    /// The files this task changed since it left its base, as its refresh
    /// read them; None on the base branch itself.
    pub fn touched(&self, cx: &App) -> Option<BTreeSet<String>> {
        self.changes.read(cx).base.as_ref().map(|_| (*self.branch_paths).clone())
    }

    pub fn change_count(&self) -> usize {
        self.files.len()
    }

    /// Commits not pushed: ahead of the upstream, or the branch's own ones
    /// before it is published.
    pub fn unpushed(&self) -> u32 {
        self.branch_info.ahead
    }

    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = visible;
        self.fill_tree(cx);
        self.sync_visibility(cx);
        if visible {
            // What it skipped while out of view.
            let _ = self.refresh_tx.try_send(());
            self.show_diff_if_seen(cx);
            self.fill_tree(cx);
            self.focus_active(window, cx);
            if self.right == RightPane::Run && self._listening_poll.is_none() {
                self.refresh_services(cx);
                self.watch_listening(cx);
            }
        }
    }

    /// Hidden terminals keep parsing but stop producing frames.
    fn sync_visibility(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            let shown = tab.id == self.left_active || Some(tab.id) == self.right_active;
            let visible = self.visible && shown;
            match &tab.kind {
                TabKind::Terminal(view) => view.read(cx).state.clone().update(cx, |s, cx| s.set_visible(visible, cx)),
                TabKind::Browser(view) => view.update(cx, |b, cx| b.set_visible(visible, cx)),
                _ => {}
            }
        }
    }

    /// The tab in the pane that has focus.
    fn focused_ix(&self) -> Option<usize> {
        let id = if self.focus_right { self.right_active } else { Some(self.left_active) };
        id.and_then(|id| self.ix_of(id))
    }

    /// Whether the keyboard is with the tab in front, for tests.
    #[cfg(test)]
    pub fn front_tab_has_focus(&self, window: &Window, cx: &App) -> bool {
        match self.focused_ix().and_then(|ix| self.tabs.get(ix)).map(|t| &t.kind) {
            Some(TabKind::Terminal(view)) => view.read(cx).state.read(cx).focus_handle().is_focused(window),
            Some(TabKind::File(file)) => file.read(cx).editor.read(cx).focus_handle(cx).is_focused(window),
            Some(TabKind::Browser(view)) => view.focus_handle(cx).is_focused(window),
            _ => self.focus.is_focused(window),
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        // A task out of view has nothing the keyboard can go to: asked for
        // there (a tab restored at the start, a prompt sent as its agent's turn
        // ends), the focus would leave the task that is typed in for nothing
        // that is drawn. It takes the focus when it comes into view.
        if !self.visible {
            return;
        }
        match self.focused_ix().and_then(|ix| self.tabs.get(ix)).map(|t| &t.kind) {
            Some(TabKind::Terminal(view)) => {
                view.read(cx).state.read(cx).focus_handle().clone().focus(window, cx)
            }
            Some(TabKind::File(file)) => {
                file.read(cx).editor.read(cx).focus_handle(cx).focus(window, cx)
            }
            Some(TabKind::Browser(view)) => view.focus_handle(cx).focus(window, cx),
            _ => self.focus.focus(window, cx),
        }
    }

    /// Shows tab `ix` in its own pane and focuses that pane.
    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        if self.focused_ix() != Some(ix) {
            self.record_nav(cx);
            self.note_recent(cx);
        }
        self.tabs[ix].used = use_tick();
        let tab = &self.tabs[ix];
        if matches!(tab.kind, TabKind::File(_)) {
            self.structure_file = Some(tab.id);
        }
        if tab.right {
            self.right_active = Some(tab.id);
            self.focus_right = true;
        } else {
            self.left_active = tab.id;
            self.focus_right = false;
        }
        self.autosave_unfocused(cx);
        self.sync_visibility(cx);
        self.show_diff_if_seen(cx);
        self.focus_active(window, cx);
        self.note_recent(cx);
        cx.notify();
    }

    /// The focused file goes to the front of the recent ones.
    fn note_recent(&mut self, cx: &App) {
        let Some(TabKind::File(file)) = self.focused_ix().map(|ix| &self.tabs[ix].kind) else { return };
        let f = file.read(cx);
        let entry = (f.path.clone(), f.cursor_line, f.cursor_col);
        self.recent.retain(|(p, _, _)| *p != entry.0);
        self.recent.insert(0, entry);
        self.recent.truncate(50);
    }

    /// Recent files for Ctrl+E, the one you are in left out.
    pub fn recent_files(&self, cx: &App) -> Vec<PathBuf> {
        let current = match self.focused_ix().map(|ix| &self.tabs[ix].kind) {
            Some(TabKind::File(f)) => Some(f.read(cx).path.clone()),
            _ => None,
        };
        let mut files: Vec<PathBuf> = self.recent.iter().map(|(p, _, _)| p.clone()).collect();
        // Open tabs restored from the last run count too, after the ones visited.
        for tab in &self.tabs {
            if let TabKind::File(f) = &tab.kind {
                let path = f.read(cx).path.clone();
                if !files.contains(&path) {
                    files.push(path);
                }
            }
        }
        files.retain(|p| Some(p) != current.as_ref() && p.exists());
        files
    }

    /// Back to a recent file: its tab as it is, or opened where the cursor was.
    pub fn reopen_recent(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.tabs.iter().position(|t| matches!(&t.kind, TabKind::File(f) if f.read(cx).path == path));
        match open {
            Some(ix) => {
                self.record_nav(cx);
                self.activate(ix, window, cx);
            }
            None => {
                let (line, col) = self.recent.iter().find(|(p, _, _)| *p == path).map_or((0, 0), |(_, l, c)| (*l, *c));
                self.open_file_as(path, line, col, OpenAs::Kept, window, cx);
            }
        }
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(ix).map(|t| &t.kind) {
            Some(TabKind::File(file)) if file.read(cx).dirty => {
                let title = self.tabs[ix].title.clone();
                let task = cx.weak_entity();
                let file_id = file.entity_id();
                window.open_alert_dialog(cx, move |dialog, _, _| {
                    let task = task.clone();
                    dialog
                        .confirm()
                        .title(format!("Discard changes to {title}?"))
                        .description("The unsaved edits will be lost.")
                        .ok_text("Discard")
                        .on_ok(move |_, window, cx| {
                            let _ = task.update(cx, |t, cx| {
                                if let Some(ix) = t.tabs.iter().position(|tab| match &tab.kind {
                                    TabKind::File(f) => f.entity_id() == file_id,
                                    _ => false,
                                }) {
                                    t.remember_closed(ix, cx);
                                    t.remove_tab(ix, window, cx);
                                }
                            });
                            true
                        })
                })
            }
            Some(TabKind::File(_) | TabKind::Browser(_) | TabKind::Binary(_) | TabKind::Changes | TabKind::History) => {
                self.remember_closed(ix, cx);
                self.remove_tab(ix, window, cx)
            }
            Some(TabKind::Terminal(_)) if self.tabs[ix].title == "agent" => {}
            Some(TabKind::Terminal(view)) => {
                let state = view.read(cx).state.clone();
                if !state.read(cx).needs_confirm_close() {
                    TermView::end(&view, cx);
                    self.remove_tab(ix, window, cx);
                    return;
                }
                // Something other than the shell runs there (claude, a server…).
                let program = state
                    .read(cx)
                    .foreground()
                    .map(|p| format!("`{}`", p.name()))
                    .unwrap_or_else(|| "a program".into());
                let title = self.tabs[ix].title.clone();
                let task = cx.weak_entity();
                let view_id = view.entity_id();
                let view = view.clone();
                window.open_alert_dialog(cx, move |dialog, _, _| {
                    let task = task.clone();
                    let view = view.clone();
                    dialog
                        .confirm()
                        .title(format!("Close \"{title}\"?"))
                        .description(format!("It is still running {program}, which will be stopped."))
                        .ok_text("Close")
                        .on_ok(move |_, window, cx| {
                            TermView::end(&view, cx);
                            let _ = task.update(cx, |t, cx| {
                                if let Some(ix) = t.tabs.iter().position(|tab| match &tab.kind {
                                    TabKind::Terminal(v) => v.entity_id() == view_id,
                                    _ => false,
                                }) {
                                    t.remove_tab(ix, window, cx);
                                }
                            });
                            true
                        })
                })
            }
            _ => {}
        }
    }

    fn remember_closed(&mut self, ix: usize, cx: &App) {
        let Some(tab) = self.tabs.get(ix) else { return };
        let (path, line, col) = match &tab.kind {
            TabKind::File(f) => {
                let f = f.read(cx);
                (f.path.clone(), f.cursor_line, f.cursor_col)
            }
            TabKind::Binary(b) => (b.read(cx).path.clone(), 0, 0),
            _ => return,
        };
        self.closed.retain(|c| c.path != path);
        self.closed.push(ClosedTab { path, line, col, right: tab.right });
        if self.closed.len() > 30 {
            self.closed.remove(0);
        }
    }

    pub fn reopen_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(closed) = self.closed.pop() {
            if closed.path.is_file() {
                self.focus_right = closed.right && self.is_split();
                self.open_file_as(closed.path, closed.line, closed.col, OpenAs::Kept, window, cx);
                return;
            }
        }
    }

    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.focused_ix() {
            self.close_tab(ix, window, cx);
        }
    }

    /// The tabs `which` picks in a pane, closed without asking: those of
    /// files, pictures and pages. Pinned ones, files with unsaved edits,
    /// terminals and the agent stay.
    fn close_many(&mut self, right: bool, which: impl Fn(usize, &TabItem) -> bool, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(ix, t)| t.right == right && !t.pinned && which(*ix, t))
            .filter(|(_, t)| match &t.kind {
                TabKind::File(f) => !f.read(cx).dirty && f.read(cx).saving.is_none(),
                TabKind::Binary(_) | TabKind::Browser(_) => true,
                _ => false,
            })
            .map(|(_, t)| t.id)
            .collect();
        for id in ids {
            if let Some(ix) = self.ix_of(id) {
                self.remember_closed(ix, cx);
                self.remove_tab(ix, window, cx);
            }
        }
    }

    fn toggle_pin(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.pinned = !tab.pinned;
            tab.preview = false;
            cx.notify();
        }
    }

    fn remove_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let removed = self.tabs.remove(ix);
        // The diff laid out and coloured goes with its tab; it is made
        // again from what the task keeps when the tab is next looked at.
        if matches!(removed.kind, TabKind::Changes) {
            let diff = self.changes.read(cx).diff.clone();
            diff.update(cx, |state, cx| state.set_files([], cx));
            self.changes.update(cx, |changes, _| (changes.hunks, changes.first_lines) = Default::default());
            self.diff_stale = true;
        }
        // The server keeps every text it was given until told otherwise.
        if let TabKind::File(file) = &removed.kind {
            self.keep_bookmarks(file, cx);
            let f = file.read(cx);
            if let Some(client) = f.lsp_client.clone() {
                let path = f.path.clone();
                cx.background_spawn(crate::procs::on_thread(move || {
                    let _ = client.did_close(&path);
                }))
                .detach();
            }
        }
        // The pane that showed it moves to its left neighbour in that pane, not
        // onto whatever comes next (a diff the same click would hit).
        let neighbour = self.tabs[..ix.min(self.tabs.len())]
            .iter()
            .rev()
            .chain(self.tabs[ix.min(self.tabs.len())..].iter())
            .find(|t| t.right == removed.right)
            .map(|t| t.id);
        if removed.right {
            if self.right_active == Some(removed.id) {
                self.right_active = neighbour;
            }
        } else if self.left_active == removed.id {
            self.left_active = neighbour.unwrap_or(0);
        }
        self.normalize();
        self.sync_visibility(cx);
        // The tab that comes to the front may be Changes, with an older diff.
        self.show_diff_if_seen(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn show_view(&mut self, which: usize, window: &mut Window, cx: &mut Context<Self>) {
        match which {
            0 => {
                if let Some(ix) = self.tabs.iter().position(|t| t.title == "agent") {
                    self.activate(ix, window, cx);
                }
            }
            1 => self.show_tab(|k| matches!(k, TabKind::Changes), window, cx),
            2 => self.show_tab(|k| matches!(k, TabKind::History), window, cx),
            _ => match self.tabs.iter().position(|t| t.title != "agent" && matches!(t.kind, TabKind::Terminal(_))) {
                Some(ix) => self.activate(ix, window, cx),
                None => self.new_terminal(window, cx),
            },
        }
    }

    /// Alt+1/2/3: shows the panel and moves the keyboard there. Arrows move,
    /// Enter opens, Escape goes back to the tab.
    pub fn show_side_panel(&mut self, which: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.right = match which {
            0 => RightPane::Changes,
            1 => RightPane::Files,
            2 => RightPane::Structure,
            _ => RightPane::Run,
        };
        self.fill_tree(cx);
        if self.right == RightPane::Run {
            self.refresh_services(cx);
            self.watch_listening(cx);
        }
        self.side_selected = match self.right {
            // Start on the declaration the cursor is in.
            RightPane::Structure => self.structure_items(cx).iter().rposition(|i| i.line <= self.structure_cursor(cx)).unwrap_or(0),
            _ => 0,
        };
        match self.right {
            RightPane::Files => self.tree.update(cx, |t, cx| t.focus(window, cx)),
            _ => self.side_focus.focus(window, cx),
        }
        cx.notify();
    }

    fn side_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let len = match self.right {
            RightPane::Changes => self.files.len(),
            RightPane::Structure => self.structure_items(cx).len(),
            RightPane::Files | RightPane::Run => return,
        };
        match event.keystroke.key.as_str() {
            "down" if len > 0 => self.side_selected = (self.side_selected + 1).min(len - 1),
            "up" => self.side_selected = self.side_selected.saturating_sub(1),
            "enter" if len > 0 => {
                let ix = self.side_selected.min(len - 1);
                match self.right {
                    RightPane::Changes => {
                        let path = self.files[ix].path.clone();
                        self.show_changed_file(path, window, cx);
                    }
                    _ => {
                        let line = self.structure_items(cx)[ix].line;
                        if let Some((id, file)) = self.structure_target() {
                            if let Some(tab) = self.tabs.iter().position(|t| t.id == id) {
                                self.activate(tab, window, cx);
                            }
                            file.update(cx, |f, cx| f.go_to_line(line, 0, window, cx));
                        }
                    }
                }
                return;
            }
            "escape" => return self.focus_active(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn structure_target(&self) -> Option<(usize, Entity<FileView>)> {
        let id = self.structure_file?;
        self.tabs.iter().find_map(|t| match &t.kind {
            TabKind::File(f) if t.id == id => Some((id, f.clone())),
            _ => None,
        })
    }

    fn structure_cursor(&self, cx: &App) -> u32 {
        self.structure_target().map_or(0, |(_, f)| f.read(cx).cursor_line)
    }

    /// The outline of the Structure file, parsed again only after edits.
    fn structure_items(&self, cx: &App) -> Rc<Vec<crate::finder::OutlineItem>> {
        let Some((id, file)) = self.structure_target() else { return Rc::default() };
        let (path, version) = {
            let f = file.read(cx);
            (f.path.clone(), f.edit_version)
        };
        // While you type, the outline of a moment ago: `refresh_structure`
        // brings the new one once the typing pauses.
        if let Some((_, _, items)) = self.structure.borrow().as_ref().filter(|(i, _, _)| *i == id) {
            return items.clone();
        }
        let text = file.read(cx).editor.read(cx).text().to_string();
        let items = Rc::new(outline(&path, &text));
        *self.structure.borrow_mut() = Some((id, version, items.clone()));
        items
    }

    /// Parses the Structure panel's file again, off the UI thread, when its
    /// text is not the one the outline was made from.
    fn refresh_structure(&self, cx: &mut Context<Self>) {
        let Some((id, file)) = self.structure_target() else { return };
        let (path, version) = {
            let f = file.read(cx);
            (f.path.clone(), f.edit_version)
        };
        let current = Some((id, version));
        if self.structure.borrow().as_ref().map(|(i, v, _)| (*i, *v)) == current || self.structure_job.borrow().0 == current {
            return;
        }
        let job = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let (version, text) = file.read_with(cx, |f, cx| (f.edit_version, f.editor.read(cx).text().to_string()));
            let items = cx.background_spawn(async move { outline(&path, &text) }).await;
            let _ = this.update(cx, |this, cx| {
                *this.structure.borrow_mut() = Some((id, version, Rc::new(items)));
                cx.notify();
            });
        });
        *self.structure_job.borrow_mut() = (current, job);
    }

    /// Shows the Changes or History tab, opening it in the pane you are in
    /// when it is not open yet.
    fn show_tab(&mut self, pick: impl Fn(&TabKind) -> bool, window: &mut Window, cx: &mut Context<Self>) {
        let ix = match self.tabs.iter().position(|t| pick(&t.kind)) {
            Some(ix) => ix,
            None => {
                let (title, kind) = if pick(&TabKind::Changes) {
                    ("Changes", TabKind::Changes)
                } else if pick(&TabKind::History) {
                    ("History", TabKind::History)
                } else {
                    return;
                };
                let mut tab = TabItem::new(title, kind);
                tab.right = self.focus_right && self.is_split();
                self.tabs.push(tab);
                self.tabs.len() - 1
            }
        };
        self.activate(ix, window, cx);
    }

    fn show_changed_file(&mut self, path: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tab(|k| matches!(k, TabKind::Changes), window, cx);
        let diff = self.changes.read(cx).diff.clone();
        diff.update(cx, |state, cx| {
            state.set_file_collapsed(&path, false, cx);
            state.scroll_to_file(&path, cx);
        });
    }

}

impl Render for TaskView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.side_has_focus.set(self.side_focus.is_focused(window));
        let slot = |this: &Self, right: bool, cx: &mut Context<Self>| -> AnyElement {
            if this.zoomed == Some(right) {
                div().size_full().into_any_element()
            } else {
                this.render_pane(right, cx).into_any_element()
            }
        };
        let content: AnyElement = if self.is_split() {
            h_resizable("panes")
                .child(resizable_panel().child(slot(self, false, cx)))
                .child(resizable_panel().child(slot(self, true, cx)))
                .into_any_element()
        } else {
            slot(self, false, cx)
        };
        div()
            .size_full()
            .track_focus(&self.focus)
            // Capture phase: terminals and editors handle mouse downs themselves.
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if let MouseButton::Navigate(direction) = event.button {
                    cx.stop_propagation();
                    this.navigate(direction == NavigationDirection::Back, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleZoom, window, cx| this.toggle_zoom_focused(window, cx)))
            .on_action(cx.listener(|this, _: &ShowOutline, window, cx| this.show_outline(window, cx)))
            .on_action(cx.listener(|this, _: &ShowBookmarks, window, cx| this.show_bookmarks(window, cx)))
            .on_action(cx.listener(|this, _: &ShowUsages, window, cx| this.show_usages_at_cursor(window, cx)))
            .on_action(cx.listener(|this, _: &SendToAgent, window, cx| this.send_editor_selection(window, cx)))
            .on_action(cx.listener(|this, _: &ShowBranches, window, cx| this.branches_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &CompareFileWith, window, cx| this.compare_open_file(window, cx)))
            .child(
                h_resizable("task")
                    .on_resize(|state, _, cx| {
                        if let Some(width) = state.read(cx).sizes().get(1).copied() {
                            cx.set_global(SidePanelWidth(width));
                        }
                    })
                    .child(resizable_panel().child(
                        v_flex()
                            .size_full()
                            .min_w_0()
                            .child(self.render_branch(cx))
                            .child(div().flex_1().min_h_0().child(content)),
                    ))
                    .child(
                        resizable_panel()
                            .size(cx.try_global::<SidePanelWidth>().map_or(px(300.), |w| w.0))
                            .size_range(px(200.)..px(640.))
                            .fixed()
                            .child(self.render_files(cx)),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod disk_tests {
    use super::{DiskChange, Rope, changed_on_disk, mtime, parse_patch, same_text, text_hash, write_in_order};

    #[test]
    fn the_editor_text_is_compared_in_place() {
        // Long enough to be in several chunks of the rope.
        let text = "fn main() { println!(\"привет\"); }\n".repeat(4000);
        let rope = Rope::from(text.as_str());
        assert!(same_text(&rope, &text));
        assert!(!same_text(&rope, &text.replacen("main", "mine", 1)), "the same length, another text");
        assert!(!same_text(&rope, &text[..text.len() - 1]));
        assert!(same_text(&Rope::from(""), ""));
        // The same told from a print of the text, in whatever pieces the editor holds it.
        let print = super::file::Print::of(&text);
        assert!(print.is(&rope) && super::file::Print::of("").is(&Rope::from("")));
        assert!(!print.is(&Rope::from(text.replacen("main", "mine", 1).as_str())) && !print.is(&Rope::from(&text[..text.len() - 1])));
    }

    #[test]
    fn writes_keep_their_order_and_changes_on_disk_are_read() {
        let dir = std::env::temp_dir().join(format!("ideae-disk-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        let gate = super::file::WriteGate::default();
        let written = write_in_order(&gate, 2, &file, None, b"later").unwrap().unwrap();
        assert_eq!(written, mtime(&file));
        // A write asked for earlier that only starts now.
        write_in_order(&gate, 1, &file, None, b"earlier").unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "later");
        // One asked before the view heard of the last: the file is as that left it.
        write_in_order(&gate, 3, &file, None, b"third").unwrap().unwrap();
        // Another wrote it since, and no refresh has told: it is not written over.
        let theirs = mtime(&file).unwrap() + std::time::Duration::from_secs(7);
        std::fs::write(&file, "the agent's").unwrap();
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(theirs).unwrap();
        assert_eq!(write_in_order(&gate, 4, &file, written, b"mine").unwrap(), Err(super::file::ChangedOnDisk));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "the agent's");
        // Known as it is now (Keep mine), it is.
        let written = write_in_order(&gate, 5, &file, Some(theirs), b"later").unwrap().unwrap();

        let gone = dir.join("gone.txt");
        let stale = written.map(|t| t - std::time::Duration::from_secs(5));
        let found = changed_on_disk(vec![(file.clone(), written), (file.clone(), stale), (file.clone(), None), (gone, None)]);
        assert!(found[0].is_none(), "as known");
        assert!(matches!(&found[1], Some(DiskChange { known, now, content }) if *known == stale && Some(*now) == written && content == "later"));
        assert!(found[2].is_some() && found[3].is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_patch_is_parsed_once() {
        let patch = "diff --git a/a.txt b/a.txt\nindex 5626abf..f719efd 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n one\n+two\n";
        let mut seen = text_hash("");
        assert!(parse_patch(String::new(), &mut seen).is_none(), "nothing changed, as at the start");
        let parsed = parse_patch(patch.into(), &mut seen).unwrap();
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.first_lines.get("a.txt"), Some(&1));
        assert!(parsed.hashes.contains_key("a.txt"));
        assert!(parse_patch(patch.into(), &mut seen).is_none(), "the same again");
        assert!(parse_patch(String::new(), &mut seen).is_some_and(|p| p.files.is_empty()), "committed: no changes left");
    }
}

#[cfg(all(test, unix))]
mod open_file_scenario {
    use super::{FileView, TabKind, TaskView};
    use crate::state::{ProjectState, State};
    use crate::workspace::Workspace;
    use gpui_kit::{App, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds, WindowOptions, px, size};
    use gpui_kit::test::TestWindowExt as _;
    use std::time::Duration;

    /// A file open in a task of a real repository: typing, saving, the
    /// agent writing it meanwhile, the app quitting mid-save.
    #[gpui_kit::test]
    #[ignore]
    fn an_open_file_is_saved_and_follows_the_disk(cx: &mut TestAppContext) {
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        // The task's terminal is real: its threads wake the app when they like.
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("ideae-open-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = "git init -q -b main repo; cd repo; git config user.email t@t; git config user.name t; echo one > f.txt; git add f.txt; git commit -qm base";
        assert!(std::process::Command::new("sh").arg("-ec").arg(script).current_dir(&dir).status().unwrap().success());
        let repo = dir.join("repo").canonicalize().unwrap();
        let path = repo.join("f.txt");
        // SAFETY: set before anything of the app reads them.
        unsafe {
            std::env::set_var("IDEAE_STATE", dir.join("state.json"));
            std::env::set_var("IDEAE_NO_DAEMON", "1");
        }
        let saved = State { projects: vec![ProjectState { path: repo.clone(), ..Default::default() }], ..Default::default() };
        std::fs::write(dir.join("state.json"), serde_json::to_string(&saved).unwrap()).unwrap();
        crate::state::set_settings(crate::state::Settings { notifications: false, pr_status: false, ..Default::default() });
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_cn::init(cx);
            crate::theme::init(cx);
        });
        let (handle, workspace) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1300.), px(800.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(Vec::new(), Some(None), None, window, cx))).expect("a test window")
        });
        cx.update_window(handle.into(), |_, window, cx| workspace.update(cx, |w, cx| w.open_main(0, window, cx))).unwrap();
        cx.run_until_parked();
        let task: Entity<TaskView> = workspace.update(cx, |w, _| w.active_view().expect("the main checkout in view"));
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.open_file(path.clone(), 0, window, cx))).unwrap();
        let file: Entity<FileView> = task.update(cx, |t, _| {
            t.tabs.iter().find_map(|tab| match &tab.kind {
                TabKind::File(f) => Some(f.clone()),
                _ => None,
            })
            .expect("the file's tab")
        });
        let typed = |cx: &mut TestAppContext, text: &str| {
            cx.update_window(handle.into(), |_, window, cx| file.update(cx, |f, cx| f.editor.update(cx, |e, cx| e.insert(text, window, cx)))).unwrap();
            cx.run_until_parked();
        };
        // The refresh a file event starts, and its pause for more of them.
        let refreshed = |cx: &mut TestAppContext| {
            task.update(cx, |t, _| t.refresh_tx.try_send(()).unwrap());
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(200));
            cx.run_until_parked();
        };
        let on_disk = || std::fs::read_to_string(&path).unwrap();
        let shown = |cx: &mut TestAppContext| file.update(cx, |f, cx| f.editor.read(cx).value().to_string());

        typed(cx, "zero\n");
        file.update(cx, |f, cx| {
            assert!(f.dirty, "typed in");
            f.save(cx);
            assert!(f.dirty && f.saving.is_some() && f.disk_known().is_none(), "being written, off this thread");
        });
        assert_eq!(on_disk(), "one\n");
        cx.run_until_parked();
        assert_eq!(on_disk(), "zero\none\n");
        file.update(cx, |f, _| assert!(!f.dirty && f.saving.is_none() && f.error.is_none() && f.disk_mtime == super::mtime(&path)));
        refreshed(cx);
        task.update(cx, |t, _| {
            assert_eq!(t.files.len(), 1, "one changed file");
            assert_eq!((t.uncommitted_diff.len(), t.uncommitted_lines.get("f.txt")), (1, Some(&1)));
        });
        // The Changes panel draws it: a list of rows of which those in view are laid out.
        cx.update_window(handle.into(), |_, window, cx| {
            task.update(cx, |t, cx| {
                t.right = super::RightPane::Changes;
                cx.notify();
            });
            window.render_frame(cx);
            let row = window.find(("file", 0usize));
            assert!(row.visible() && row.bounds().size.height <= px(super::git_ui::CHANGE_ROW), "the changed file's row, whole in the room every row has: {:?}", row.bounds());
        })
        .unwrap();
        assert_eq!(shown(cx), "zero\none\n", "its own save is no change on disk");

        // The agent writes the file: a clean buffer follows.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&path, "agent\n").unwrap();
        refreshed(cx);
        assert_eq!(shown(cx), "agent\n");
        file.update(cx, |f, _| assert!(!f.dirty && !f.conflict));

        // And again while there are edits: a choice, and neither side lost.
        typed(cx, "mine ");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&path, "theirs\n").unwrap();
        refreshed(cx);
        file.update(cx, |f, cx| {
            assert!(f.dirty && f.conflict);
            f.autosave(cx);
            f.save_now(cx);
        });
        cx.run_until_parked();
        assert_eq!((shown(cx), on_disk()), ("agent\nmine ".to_string(), "theirs\n".to_string()));

        // Quitting right after a save, with more typed since: all of it is on disk at once.
        file.update(cx, |f, cx| {
            f.conflict = false;
            f.save(cx);
        });
        cx.update_window(handle.into(), |_, window, cx| file.update(cx, |f, cx| f.editor.update(cx, |e, cx| e.insert("and ", window, cx)))).unwrap();
        file.update(cx, |f, cx| f.save_now(cx));
        let last = shown(cx);
        assert!(last.contains("and ") && last.contains("mine "));
        assert_eq!(on_disk(), last);
        cx.run_until_parked();
        assert_eq!(on_disk(), last, "the earlier write does not come after");
        file.update(cx, |f, _| assert!(!f.dirty && f.saving.is_none()));

        // The Changes tab in front has the diff as it is; behind the file's
        // tab it is left as it was, and given the new one when it comes to
        // the front again, however it does: here by the file's tab closing.
        let shown = |cx: &mut TestAppContext| task.update(cx, |t, cx| t.changes.read(cx).diff.read(cx).files().len());
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.show_tab(|k| matches!(k, TabKind::Changes), window, cx))).unwrap();
        refreshed(cx);
        task.update(cx, |t, _| assert!(!t.diff_stale));
        assert_eq!(shown(cx), 1, "the one changed file");
        // Another file's tab, opened after it and so to its right, comes in front.
        std::fs::write(repo.join("new.txt"), "new\n").unwrap();
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.open_file(repo.join("new.txt"), 0, window, cx))).unwrap();
        refreshed(cx);
        task.update(cx, |t, _| assert!(t.diff_stale && t.files.len() == 2, "read, and not laid out behind the file's tab"));
        assert_eq!(shown(cx), 1);
        cx.update_window(handle.into(), |_, window, cx| {
            task.update(cx, |t, cx| {
                let ix = t.tabs.iter().rposition(|tab| matches!(tab.kind, TabKind::File(_))).unwrap();
                t.remove_tab(ix, window, cx);
            })
        })
        .unwrap();
        task.update(cx, |t, _| assert!(!t.diff_stale, "in front again, with the diff of now"));
        assert_eq!(shown(cx), 2);

        // Files opened to look at share one tab; one opened to keep, or
        // edited, has its own; past the limit, the one left longest closes.
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(repo.join(name), "x\n").unwrap();
        }
        let open = |cx: &mut TestAppContext, name: &str, how: super::OpenAs| {
            cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.open_file_as(repo.join(name), 0, 0, how, window, cx))).unwrap();
            cx.run_until_parked();
        };
        let open_files = |cx: &mut TestAppContext| {
            task.update(cx, |t, cx| {
                t.tabs
                    .iter()
                    .filter_map(|tab| match &tab.kind {
                        TabKind::File(f) => Some(format!("{}{}", f.read(cx).path.file_name().unwrap().to_string_lossy(), if tab.preview { "?" } else { "" })),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(open_files(cx), ["f.txt"], "edited, so kept");
        open(cx, "a.txt", super::OpenAs::Preview);
        open(cx, "b.txt", super::OpenAs::Preview);
        assert_eq!(open_files(cx), ["f.txt", "b.txt?"]);
        open(cx, "b.txt", super::OpenAs::Kept);
        open(cx, "a.txt", super::OpenAs::Preview);
        assert_eq!(open_files(cx), ["f.txt", "b.txt", "a.txt?"]);
        crate::state::set_settings(crate::state::Settings { notifications: false, pr_status: false, tab_limit: 2, ..Default::default() });
        open(cx, "c.txt", super::OpenAs::Kept);
        assert_eq!(open_files(cx), ["a.txt?", "c.txt"], "f.txt, then b.txt, were left longest");
        crate::state::set_settings(crate::state::Settings { notifications: false, pr_status: false, ..Default::default() });
        // Pinned, a tab stays through Close others; a closed one comes back.
        open(cx, "b.txt", super::OpenAs::Kept);
        cx.update_window(handle.into(), |_, window, cx| {
            task.update(cx, |t, cx| {
                let id = |t: &TaskView, cx: &App, name: &str| t.tabs.iter().find(|tab| matches!(&tab.kind, TabKind::File(f) if f.read(cx).path.ends_with(name))).unwrap().id;
                let (a, b) = (id(t, cx, "a.txt"), id(t, cx, "b.txt"));
                t.toggle_pin(a, cx);
                t.close_many(false, |_, tab| tab.id != b, window, cx);
            })
        })
        .unwrap();
        assert_eq!(open_files(cx), ["a.txt", "b.txt"], "a.txt pinned, so no longer the preview either");
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.reopen_closed(window, cx))).unwrap();
        assert_eq!(open_files(cx), ["a.txt", "b.txt", "c.txt"]);

        task.update(cx, |t, cx| t.shut_down(cx));
        cx.update_window(handle.into(), |_, window, _| window.remove_window()).ok();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod diff_update_tests {
    use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
    use gpui_kit::{AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, Window, div, px};

    struct Host {
        state: Entity<DiffState>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(400.)).h(px(200.)).child(Diff::new(&self.state).size_full())
        }
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
    }

    /// A file of 200 lines, the first ones changed to `tag`.
    fn file(name: &str, tag: &str) -> (DiffFile, u64) {
        let lines: String = (0..200).map(|ix| format!(" line {ix}\n")).collect();
        let patch = format!("diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -1,201 +1,201 @@\n-old\n+{tag}\n{lines}");
        (DiffFile::parse(&patch).unwrap().remove(0), super::text_hash(&patch) | 1)
    }

    /// The Changes tab while an agent writes: a new patch leaves the view at
    /// the file it was reading, where taking it as another diff goes to the top.
    #[gpui_kit::test]
    fn a_diff_that_changes_stays_where_it_is_read(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_cn::init(cx);
            crate::theme::init(cx);
        });
        let state = cx.new(|cx| DiffState::new([], cx));
        let (_, cx) = cx.add_window_view({
            let state = state.clone();
            move |_, _| Host { state }
        });
        cx.update(|_, cx| state.update(cx, |state, cx| state.update_files([file("a.txt", "one"), file("b.txt", "one"), file("c.txt", "one")], cx)));
        for _ in 0..3 {
            draw(cx);
        }
        // To the third file: its header comes a third down the view, the end of the second above it.
        cx.update(|_, cx| state.update(cx, |state, cx| state.scroll_to_file("c.txt", cx)));
        draw(cx);
        let top = |cx: &mut VisualTestContext| cx.update(|_, cx| state.read(cx).top_file().map(|path| path.to_string()));
        assert_eq!(top(cx).as_deref(), Some("b.txt"));

        // b.txt changes again, the others are as they were, d.txt is new.
        cx.update(|_, cx| {
            state.update(cx, |state, cx| state.update_files([file("a.txt", "one"), file("b.txt", "two"), file("c.txt", "one"), file("d.txt", "one")], cx))
        });
        draw(cx);
        assert_eq!(top(cx).as_deref(), Some("b.txt"), "still at the lines being read");
        // And the file before it goes.
        cx.update(|_, cx| state.update(cx, |state, cx| state.update_files([file("b.txt", "two"), file("c.txt", "one"), file("d.txt", "one")], cx)));
        draw(cx);
        assert_eq!(top(cx).as_deref(), Some("b.txt"));

        // Another diff altogether starts at its top.
        cx.update(|_, cx| state.update(cx, |state, cx| state.set_files([file("a.txt", "one").0, file("b.txt", "one").0], cx)));
        draw(cx);
        assert_eq!(top(cx).as_deref(), Some("a.txt"));
    }
}
