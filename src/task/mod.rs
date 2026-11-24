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
    input::{CodeActionProvider, CompletionProvider, DefinitionProvider, DocumentRangeSemanticTokensProvider, Editor, EditorState, HoverProvider, Inlay, InlayHintsProvider, InlayStyle, InputEvent, Position, Rope, RopeExt as _},
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

mod activity;
mod agent_tab;
mod lookups;
mod branches;
mod refresh;
mod nav;
mod branch_bar;
mod open;
mod panels;
mod panes;
use panes::Pane;
mod git_ui;
mod helpers;
#[cfg(test)]
mod unicode_tests;
mod outline;
mod term;
mod chat;
mod file;
mod preview;
mod sql;
mod sql_dialog;
mod sql_guard;
mod sql_grid;
mod sql_schema;
pub(crate) use preview::{ink, rasterize as rasterize_svg};
mod changes;
mod session;
mod change_tree;
mod services;
mod db_panel;
mod auto;
mod pr;
mod replace;
mod annotate;
mod bookmarks;
mod change_popup;
mod file_tree;
mod strip;
mod tell;
use file_tree::*;
use git_ui::*;
pub use helpers::*;
// Named, to come before the one of the same name in gpui's own glob.
use outline::outline;
pub use term::*;
pub use chat::{ChatEvent, ChatView};
pub use file::*;
pub use changes::*;
pub use session::*;
pub use annotate::ToggleBlame;
pub use preview::{CycleLook, PREVIEW_CONTEXT};
pub use sql::{RunFile, RunStatement, SQL_INPUT_CONTEXT, SqlDefinition, StopStatement};
pub use sql_dialog::{bastion_dialog, connection_dialog, import_dialog};
pub use sql_guard::{lock_now, production_gate};
pub use db_panel::{forget_in_panel, reload_locked};
pub use git_ui::GitCommand;
pub use bookmarks::{NextBookmark, PrevBookmark, ShowBookmarks, ToggleBookmark};
use change_tree::*;
use pr::*;
pub use auto::AutoAsk;
pub use agent_tab::{AgentState, Asked, Question, Who};

// ---------------------------------------------------------------- task

enum TabKind {
    Terminal(Entity<TermView>),
    /// The place of an agent that is not running (`TabItem::agent`): opened,
    /// a question's goes on; the task's own waits to be started.
    Asleep,
    /// An agent that speaks ACP: its conversation and a prompt.
    Chat(Entity<ChatView>),
    Changes,
    History,
    File(Entity<FileView>),
    Browser(Entity<crate::browser::BrowserView>),
    /// A file that cannot be shown as text: binary, or unreadable.
    Binary(Entity<BinaryView>),
    /// The worktree against a commit, a branch or a stash: not kept over a restart.
    Diff(Entity<DiffTab>),
}

/// What the menu bar's tab items have to act on.
pub struct TabsState {
    pub closable: bool,
    pub file: bool,
    pub back: bool,
    pub forward: bool,
    pub reopen: bool,
    pub front: Option<FrontTab>,
    /// The side panels in view, by their place in the order of their keys:
    /// none in focus mode, or of a narrow window's hidden side.
    pub panels: [bool; 5],
}

/// The tabs the menu bar names, when one is in front.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum FrontTab {
    Agent,
    Changes,
    History,
    Shell,
}

pub struct BinaryView {
    path: PathBuf,
    reason: String,
    /// An image: shown as a picture, with what it is below.
    picture: Option<Entity<crate::picture::PictureView>>,
}

impl BinaryView {
    /// An image file, shown when its size is read off this thread.
    fn image(path: PathBuf, kind: &'static str, cx: &mut Context<Self>) -> Self {
        let picture = cx.new(crate::picture::PictureView::new);
        let read = path.clone();
        let image = ImageSource::from(path.clone());
        let caption = move |dims: Option<(u32, u32)>, bytes: Option<u64>| {
            let dims = dims.map(|(w, h)| format!(" · {w} × {h}")).unwrap_or_default();
            SharedString::from(format!("{kind}{dims}{}", bytes.map(|b| format!(" · {}", helpers::human_size(b))).unwrap_or_default()))
        };
        picture.update(cx, |v, cx| v.show(0, image.clone(), None, None, caption(None, None), cx));
        let shown = picture.downgrade();
        cx.spawn(async move |_, cx| {
            let (dims, bytes) = cx
                .background_spawn(async move {
                    let dims = image::ImageReader::open(&read).ok().and_then(|r| r.with_guessed_format().ok()).and_then(|r| r.into_dimensions().ok());
                    (dims, std::fs::metadata(&read).ok().map(|m| m.len()))
                })
                .await;
            let own = dims.map(|(w, h)| size(px(w as f32), px(h as f32)));
            // Fit, a picture grows no larger than its own size.
            let _ = shown.update(cx, |v, cx| v.show(0, image, own, own, caption(dims, bytes), cx));
        })
        .detach();
        BinaryView { path, reason: String::new(), picture: Some(picture) }
    }
}

impl Render for BinaryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (path, reveal) = (self.path.clone(), self.path.clone());
        if let Some(picture) = &self.picture {
            return v_flex()
                .size_full()
                .child(
                    h_flex()
                        .justify_end()
                        .gap_1()
                        .px_3()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(Button::new("open-system").xsmall().ghost().label("Open in the system app").on_click(move |_, _, cx| cx.open_with_system(&path)))
                        .child(Button::new("reveal").xsmall().ghost().label("Reveal in file manager").on_click(move |_, _, cx| file_tree::reveal(reveal.clone(), false, cx))),
                )
                .child(div().flex_1().min_h_0().child(picture.clone()));
        }
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
                h_flex()
                    .gap_2()
                    .child(Button::new("open-system").small().outline().label("Open in the system app").on_click(move |_, _, cx| cx.open_with_system(&path)))
                    .child(Button::new("reveal").small().outline().label("Reveal in file manager").on_click(move |_, _, cx| file_tree::reveal(reveal.clone(), false, cx))),
            )
    }
}

/// What is being dragged, set when the drag starts. Pane dividers drag too
/// (gpui's drag-and-drop does not say what is dragged), and those must not
/// show the drop zones.
#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Tab,
    Files,
}

static DRAG: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn start_drag(drag: Drag) {
    DRAG.store(drag as u8, std::sync::atomic::Ordering::Relaxed);
}

fn drag_active(cx: &App) -> Drag {
    if !cx.has_active_drag() {
        DRAG.store(0, std::sync::atomic::Ordering::Relaxed);
        return Drag::None;
    }
    match DRAG.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Drag::Tab,
        2 => Drag::Files,
        _ => Drag::None,
    }
}

/// A tab being dragged to the other pane; also its drag preview.
#[derive(Clone)]
struct DraggedTab {
    tab_id: usize,
    title: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_preview(self.title.clone(), cx)
    }
}

fn drag_preview(title: SharedString, cx: &App) -> impl IntoElement {
    div()
        .px_3()
        .py_1()
        .text_sm()
        .rounded_md()
        .bg(cx.theme().secondary)
        .border_1()
        .border_color(cx.theme().primary)
        .child(title)
}

/// Files dragged from the Files or the Changes panel: to a terminal, which
/// types their paths, or to a pane, which opens them.
#[derive(Clone)]
pub(crate) struct DraggedFiles {
    /// Absolute.
    pub paths: Vec<PathBuf>,
    /// Files on the disk; not a folder, nor a deletion in Changes, which
    /// have a path to type and nothing to open.
    pub opens: bool,
    /// The task's worktree: in its terminals the paths are typed from there.
    pub root: PathBuf,
}

impl DraggedFiles {
    fn new(paths: Vec<PathBuf>, opens: bool, root: PathBuf) -> Self {
        Self { paths, opens, root }
    }

    /// As typed in a shell or to an agent: relative to the worktree when in it.
    pub fn text(&self) -> String {
        self.paths
            .iter()
            .map(|p| term::shell_quote(&p.strip_prefix(&self.root).unwrap_or(p).display().to_string()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl Render for DraggedFiles {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = |p: &PathBuf| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
        let title = match self.paths.as_slice() {
            [one] => name(one),
            many => format!("{} files", many.len()),
        };
        drag_preview(title.into(), cx)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum RightPane {
    Changes,
    Files,
    /// The declarations of the file last in view.
    Structure,
    /// The task's services (`.ideae.toml`): start, stop, open.
    Run,
    /// The connections, their databases and tables.
    Database,
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
    pane: Pane,
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
    /// The agent it runs: its terminal or its chat.
    agent: Option<Who>,
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
            pane: Pane::MAIN,
            shown: SharedString::default(),
            dirty: false,
            service: None,
            preview: false,
            pinned: false,
            used: use_tick(),
            agent: None,
        }
    }
}

/// What a terminal is there for: its role, its session in the daemon, its title.
pub(super) enum Term {
    Agent(Who),
    Shell { title: String, seq: usize },
    Service { title: String, seq: usize, name: String, cwd: PathBuf },
}

struct ClosedTab {
    path: PathBuf,
    line: u32,
    col: u32,
    pane: Pane,
}

/// A window narrower than this has the side panel over the task, on asking.
const NARROW_WINDOW: Pixels = px(1100.);

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
    /// Something only the workspace does: the project's settings, or a
    /// setup or a check, which run as the project's trust allows.
    Auto(AutoAsk),
    /// What the agent does, as guessed from its terminal (activity.rs).
    Heard(crate::agent::HookEvent),
    /// The main thread stays on this branch: not to be told again.
    KeepBranch(String),
    /// A task's worktree is on another branch, which becomes the task's.
    OwnBranch(String),
    /// A worktree was added, or this one put on another branch.
    Worktrees,
    /// Of a question asked aside (agent_tab.rs).
    Asked(Asked),
}

impl EventEmitter<TaskEvent> for TaskView {}

pub struct TaskView {
    /// The project's own checkout, not a worktree of a task.
    pub main_thread: bool,
    /// The branches the main thread was asked to stay on, not the default.
    pub kept_branches: Vec<String>,
    pub root: PathBuf,
    pub branch: SharedString,
    /// The branch the task owns, and where its worktree is when not on it, in words.
    own_branch: String,
    off_own: Option<String>,
    /// The task's own agent.
    pub agent: AgentState,
    /// Agents asked a question aside, in the order they were.
    pub questions: Vec<Question>,
    /// It was ever asked one: its folder holds sessions other than its agent's.
    pub asked: bool,
    /// The file the agent is writing to now, and those it wrote to since
    /// the last prompt: marked in the Changes and Files panels.
    agent_editing: Option<SharedString>,
    agent_touched: Rc<HashSet<SharedString>>,
    /// What questions' agents wrote, by the question.
    question_touched: Rc<HashMap<SharedString, u32>>,
    /// The paths of `files`, for the tree's marks.
    changed_paths: Rc<HashSet<SharedString>>,
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
    /// The project's setup and check, as they went in this task.
    auto: auto::Auto,
    hook_socket: Option<String>,
    /// Numbers terminals so each has a unique element id and default title.
    terminal_seq: usize,
    /// Status changed to one that wants attention while the task was not in view.
    pub unseen: bool,
    tabs: Vec<TabItem>,
    /// The tab shown in each pane, by id, by [`Pane::ix`].
    active: [Option<usize>; 4],
    /// The pane you work in: new tabs open there.
    current: Pane,
    /// Focus mode: this pane fills the window alone.
    zoomed: Option<Pane>,
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
    /// The side: a panel in its upper half and one in its lower, either or
    /// neither; the one the arrows go to; and one come over the task from
    /// its mark in the strip, with what waits to bring or take it.
    side: [Option<RightPane>; 2],
    keyed: RightPane,
    side_peek: Option<RightPane>,
    side_peek_wait: Task<()>,
    peek_focus: FocusHandle,
    /// The file the Structure panel shows (tab id), and its outline cached by
    /// (tab id, edit version).
    structure_file: Option<usize>,
    /// Keyboard focus of the side panel (Alt+1/3), and the row the arrows are on.
    side_focus: FocusHandle,
    side_selected: usize,
    /// Where the Changes and Structure panels are scrolled to.
    changes_scroll: UniformListScrollHandle,
    structure_scroll: UniformListScrollHandle,
    run_scroll: ScrollHandle,
    /// Whether the side panel holds the keyboard, set each render.
    side_has_focus: std::cell::Cell<bool>,
    structure: std::cell::RefCell<Option<(usize, u64, Rc<Vec<crate::finder::OutlineItem>>)>>,
    /// The file the Files panel last went to by itself.
    tree_followed: Option<usize>,
    /// The outline as the Structure panel shows it, some of it left out or
    /// in another order, by what it was made of.
    structure_shown: std::cell::RefCell<Option<((usize, bool, bool, bool), Rc<Vec<crate::finder::OutlineItem>>)>>,
    /// The outline being made, by what it is of.
    structure_job: std::cell::RefCell<(Option<(usize, u64)>, Task<()>)>,
    /// The panel opened before its file's outline was made: the cursor's
    /// declaration is selected when it comes.
    structure_select: bool,
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
    /// Folders made from the tree that git does not list yet, being empty.
    made_folders: Rc<Vec<String>>,
    /// The files of the diff folded for being viewed, with the version of
    /// them that was; none for one unfolded when it changed after.
    folded_viewed: HashMap<String, Option<u64>>,
    /// Folders folded in the Changes tree, as "staged:dir" / "changes:dir".
    folded_changes: HashSet<String>,
    /// Out of view long enough (or never shown): no language servers run;
    /// they start when the task comes into view.
    pub lsp_asleep: bool,
    /// Its agent in a terminal was stopped for having nothing to do: it
    /// resumes when the task comes into view.
    agent_slept: bool,
    /// When its agent's processes were last looked at, before it is
    /// stopped: once a minute at most, for one that keeps a command running.
    agent_sleep_probe: Option<std::time::Instant>,
    /// Its project said no language servers.
    pub lsp_off: bool,
    /// When its agent last asked a language server through ideae's MCP
    /// server: that is use, as a file open is.
    lsp_asked: Option<std::time::Instant>,
    file_states: HashMap<String, git::FileState>,
    commit_input: Entity<gpui_kit::component::input::TextareaState>,
    /// The next commit replaces the last one, which is not pushed; with the
    /// message put in the box for it, if one was.
    amend: Option<String>,
    /// The git/gh action running, and the outcome of the last one.
    busy: Option<&'static str>,
    /// The file the last Discard threw away, for Undo.
    /// It is taken where the discard runs, off the UI thread.
    discard_undo: Option<Arc<std::sync::Mutex<Option<git::FileSnapshot>>>>,
    action: Option<(bool, String)>,
    /// The last result told by a notification, for when the Changes panel,
    /// which shows it, is not in view: a Fetch from the branch bar is not
    /// to go unanswered.
    /// In a narrow window the side panel is over the task, shown on asking.
    narrow: bool,
    side_over: bool,
    /// How full the agent's context is, from 0 to 1, as the workspace last read it.
    pub context_fill: f32,
    told_action: Option<(bool, String)>,
    /// Wakes the refresh loop: file events, actions, the fallback tick.
    refresh_tx: async_channel::Sender<()>,
    /// The branch "Branch vs …" compares with, when picked by hand; shared
    /// with the refresh loop.
    base_override: Arc<std::sync::Mutex<Option<String>>>,
    /// Kept for as long as the task: the refresh, off the UI thread, tells
    /// it what to watch.
    _watching: Arc<std::sync::Mutex<Watching>>,
    /// One server per (program, project root): its own, or its folder in
    /// the one the project's tasks share.
    lsp: Arc<crate::lsp_share::TaskServers>,
    /// Since when no open file uses a server.
    lsp_unused: HashMap<(&'static str, PathBuf), std::time::Instant>,
    /// Where you were before each jump, for Back and Forward.
    nav_back: Vec<NavPoint>,
    nav_forward: Vec<NavPoint>,
    navigating: bool,
    visible: bool,
    /// Pages of the task's Chrome no tab opened, to show as tabs.
    _new_pages: Option<Task<()>>,
    _poll: Task<()>,
}

impl TaskView {
    pub fn new(
        root: PathBuf,
        branch: String,
        launch: Option<crate::agents::Launch>,
        start_agent: bool,
        hook_socket: Option<String>,
        port_slot: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let services = crate::services::load(&root).unwrap_or_default();
        let mut task_env = crate::services::task_env(&services, port_slot);
        task_env.push(crate::browser::task_env(&root));
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
        // Not started (a question opened in a task not running): its tab
        // holds its place, first, until it is (`start_main_agent`).
        match &launch {
            Some(launch) if start_agent => tabs.push(Self::make_agent_tab(&root, hook_socket.as_ref(), Who::Main, launch, task_env.clone(), window, cx)),
            Some(launch) => tabs.push(TabItem { agent: Some(Who::Main), ..TabItem::new(launch.profile.name.clone(), TabKind::Asleep) }),
            _ => tabs.push(Self::make_terminal(&root, hook_socket.as_ref(), Term::Shell { title: "shell".into(), seq: 1 }, None, task_env.clone(), None, window, cx)),
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
        let ws_tx = refresh_tx.clone();
        cx.observe_global::<DiffWhitespace>(move |_, _| {
            let _ = ws_tx.try_send(());
        })
        .detach();
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
                                let staged: HashSet<u64> = git::hunks(&staged).iter().map(|hunk| changes::staged_key(&hunk.path, &hunk.changes)).collect();
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
            main_thread: false,
            kept_branches: Vec::new(),
            root,
            own_branch: branch.clone(),
            off_own: None,
            branch: branch.into(),
            agent: AgentState::new(launch),
            questions: Vec::new(),
            asked: false,
            agent_editing: None,
            agent_touched: Rc::default(),
            question_touched: Rc::default(),
            changed_paths: Rc::default(),
            recent: Vec::new(),
            pr: None,
            viewed: BTreeMap::new(),
            bookmarks: BTreeMap::new(),
            port_slot,
            services,
            services_error: None,
            services_found: Vec::new(),
            auto: Default::default(),
            listening: Vec::new(),
            _listening_poll: None,
            hook_socket,
            terminal_seq: 1,
            unseen: false,
            tabs,
            active: [None; 4],
            current: Pane::MAIN,
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
            side: [Some(RightPane::Changes), None],
            keyed: RightPane::Changes,
            side_peek: None,
            side_peek_wait: Task::ready(()),
            peek_focus: cx.focus_handle(),
            structure_file: None,
            side_focus: cx.focus_handle(),
            side_selected: 0,
            changes_scroll: UniformListScrollHandle::new(),
            structure_scroll: UniformListScrollHandle::new(),
            run_scroll: ScrollHandle::new(),
            side_has_focus: Default::default(),
            structure: Default::default(),
            structure_shown: Default::default(),
            tree_followed: None,
            structure_job: std::cell::RefCell::new((None, Task::ready(()))),
            structure_select: false,
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
            made_folders: Rc::default(),
            folded_viewed: HashMap::new(),
            folded_changes: HashSet::new(),
            lsp_asleep: false,
            agent_slept: false,
            agent_sleep_probe: None,
            lsp_off: false,
            file_states: HashMap::new(),
            commit_input,
            amend: None,
            busy: None,
            discard_undo: None,
            action: None,
            told_action: None,
            context_fill: 0.,
            narrow: false,
            side_over: false,
            refresh_tx,
            base_override,
            _watching: watching,
            lsp: Arc::default(),
            lsp_unused: HashMap::new(),
            lsp_asked: None,
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
            navigating: false,
            visible: false,
            _new_pages: None,
            _poll: poll,
        };
        // Debug aid for headless screenshots: start on a given tab.
        let first = std::env::var("IDEAE_TAB").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        this.active[0] = this.tabs.get(first).or(this.tabs.first()).map(|t| t.id);
        this.sync_visibility(cx);
        if start_agent {
            this.watch_activity(Who::Main, cx);
        }
        this
    }

    fn make_terminal(
        root: &PathBuf,
        hook_socket: Option<&String>,
        term: Term,
        input: Option<String>,
        extra_env: Vec<(String, String)>,
        cwd: Option<PathBuf>,
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
        let role = match &term {
            Term::Service { .. } => "service",
            Term::Agent(_) => "agent",
            Term::Shell { .. } => "terminal",
        };
        if let Term::Agent(who) = &term {
            env.extend(who.tag().map(|tag| (crate::agent::AGENT_ENV.to_string(), tag)));
        }
        let cwd = match &term {
            Term::Service { cwd, .. } => cwd.clone(),
            _ => cwd.unwrap_or_else(|| root.clone()),
        };
        env.push((crate::agent::ROLE_ENV.into(), role.into()));
        // Enter is a carriage return; PowerShell's line editor takes a bare
        // newline for a line break inside the command and waits for more.
        let enter = if cfg!(windows) { "\r" } else { "\n" };
        let input = input.map(|input| format!("{input}{enter}"));
        let config = TerminalConfig::default()
            .with_colors(crate::theme::terminal_colors(cx))
            .with_appearance(crate::theme::terminal_appearance(&crate::state::settings()));
        // In the daemon the program outlives the window; a restart attaches
        // to it again under the same session name.
        let session = format!(
            "{}#{}",
            root.display(),
            match &term {
                Term::Service { name, .. } => format!("service-{name}"),
                Term::Agent(Who::Main) => "agent".to_string(),
                Term::Agent(Who::Question(n)) => format!("q-{n}"),
                Term::Shell { seq, .. } => format!("shell-{seq}"),
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
                let spec = crate::daemon::proto::Spec { cwd: cwd.clone(), env, input, program: Vec::new() };
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
        state.update(cx, |state, _| state.urls_to_app = true);
        cx.subscribe_in(&state, window, |this: &mut Self, state, event: &TerminalEvent, window, cx| match event {
            // A shell that exits (Ctrl+D, `exit`) takes its tab with it, as in a
            // terminal app; the agent's tab stays and shows the exit.
            TerminalEvent::Exited(_) => {
                let ix = this.tabs.iter().position(|t| match &t.kind {
                    TabKind::Terminal(v) => v.read(cx).state == *state,
                    _ => false,
                });
                match ix.map(|ix| (ix, this.tabs[ix].agent)) {
                    Some((ix, None)) => {
                        // Its thread and what it kept of the screen go with the tab.
                        state.update(cx, |state, cx| state.close(cx));
                        this.remove_tab(ix, window, cx)
                    }
                    Some((_, Some(who))) => {
                        if let Some(agent) = this.agent_of_mut(who) {
                            agent.status = crate::agent::AgentStatus::Exited;
                        }
                    }
                    None => {}
                }
                cx.emit(TaskEvent::Changed);
                cx.notify();
            }
            // Claude Code that crashed or was killed sends no SessionEnd.
            TerminalEvent::ForegroundChanged => {
                let ended = this.tabs.iter().find_map(|t| match &t.kind {
                    TabKind::Terminal(v) if v.read(cx).state == *state && v.read(cx).agent_ended(cx) => t.agent,
                    _ => None,
                });
                if let Some(agent) = ended.and_then(|who| this.agent_of_mut(who)).filter(|a| a.status != crate::agent::AgentStatus::Exited) {
                    agent.status = crate::agent::AgentStatus::Exited;
                    cx.emit(TaskEvent::Changed);
                    cx.notify();
                }
            }
            TerminalEvent::TitleChanged | TerminalEvent::Bell => {
                let title = state.read(cx).title().clone();
                let mut of_agent = None;
                if let Some(tab) = this.tabs.iter_mut().find(|t| matches!(&t.kind, TabKind::Terminal(v) if v.read(cx).state == *state)) {
                    tab.shown = title;
                    of_agent = tab.agent;
                }
                if let (Some(who), TerminalEvent::Bell) = (of_agent, event) {
                    this.agent_signal(who, None, cx);
                }
                cx.emit(TaskEvent::Changed);
                cx.notify();
            }
            TerminalEvent::Notification { title, body } => {
                if let Some(who) = this.agent_of_terminal(state, cx) {
                    let words = if body.is_empty() { title.to_string() } else { body.to_string() };
                    this.agent_signal(who, Some(&words), cx);
                }
            }
            TerminalEvent::OpenPath(target) => {
                let cwd = state.read(cx).cwd().map(|p| p.to_path_buf());
                this.open_terminal_path(target, cwd, window, cx);
            }
            // A dev server's address opens in the task's Web tab, the rest in the system's browser.
            TerminalEvent::OpenUrl(url) if crate::browser::is_local(url) => this.show_url(url, window, cx),
            TerminalEvent::OpenUrl(url) => cx.open_url(url),
            _ => {}
        })
        .detach();
        let (title, seq, agent, service_name) = match term {
            Term::Agent(who) => (if who == Who::Main { "agent".to_string() } else { "?".to_string() }, 0, Some(who), None),
            Term::Shell { title, seq } => (title, seq, None, None),
            Term::Service { title, seq, name, .. } => (title, seq, None, Some(name)),
        };
        let id = SharedString::from(match agent {
            Some(Who::Question(n)) => format!("{}-term-q{n}", root.display()),
            _ => format!("{}-term-{seq}", root.display()),
        });
        let task = agent.is_some().then(|| cx.weak_entity());
        let shown = state.read(cx).title().clone();
        let view = cx.new(|cx| TermView::new(id, state, session, seq, task, service_name.clone(), cx));
        TabItem { shown, service: service_name, agent, ..TabItem::new(title, TabKind::Terminal(view)) }
    }

    fn ix_of(&self, id: usize) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// What Restart Agent asks: its program is not running in its tab.
    pub fn agent_restartable(&self, cx: &App) -> bool {
        self.has_agent() && (self.agent_ix(Who::Main).is_none() || self.agent_stopped(Who::Main, cx))
    }

    /// The agent's program is not running in its tab.
    pub fn agent_stopped(&self, who: Who, cx: &App) -> bool {
        self.agent_ix(who).is_some_and(|ix| match &self.tabs[ix].kind {
            TabKind::Terminal(v) => v.read(cx).agent_at_prompt(cx),
            TabKind::Chat(chat) => !chat.read(cx).thread.status.live(),
            _ => false,
        })
    }

    pub fn focus_mode(&self) -> bool {
        self.zoomed.is_some()
    }

    pub fn tabs_state(&self) -> TabsState {
        let focused = self.focused_ix();
        let front = focused.map(|ix| &self.tabs[ix]);
        let agent = |t: &TabItem| t.agent == Some(Who::Main);
        // The terminal Shell goes to: another one in front is not it.
        let shell = self.tabs.iter().position(|t| t.agent.is_none() && matches!(t.kind, TabKind::Terminal(_)));
        TabsState {
            closable: front.is_some_and(|t| !agent(t)),
            file: front.is_some_and(|t| matches!(t.kind, TabKind::File(_))),
            back: self.can_navigate(true),
            forward: self.can_navigate(false),
            reopen: !self.closed.is_empty(),
            front: front.and_then(|t| match &t.kind {
                TabKind::Terminal(_) if agent(t) => Some(FrontTab::Agent),
                TabKind::Changes => Some(FrontTab::Changes),
                TabKind::History => Some(FrontTab::History),
                TabKind::Terminal(_) if focused == shell => Some(FrontTab::Shell),
                _ => None,
            }),
            panels: strip::PANES.map(|pane| self.zoomed.is_none() && self.panel_shown(pane)),
        }
    }

    /// Out of focus mode, for what comes up in the task's own layout.
    pub fn leave_focus_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.zoomed.is_some() {
            self.toggle_zoom_focused(window, cx);
        }
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
            let symbols = symbol_source(self.root.clone(), self.lsp.clients(), files);
            return crate::finder::Finder::open_symbols(self.root.clone(), symbols, cx.weak_entity(), window, cx);
        }
        if kind == crate::finder::FinderKind::Grep {
            let query = self.selected_words(cx);
            return crate::finder::Finder::open_grep(self.root.clone(), files, query, cx.weak_entity(), window, cx);
        }
        crate::finder::Finder::open(kind, self.root.clone(), files, cx.weak_entity(), window, cx);
    }

    /// A browser tab in the pane you are in. Playwright MCP can drive the
    /// same page through the endpoint shown in its toolbar.
    pub fn new_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_browser("about:blank", window, cx);
    }

    pub fn open_browser(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.add_browser(crate::browser::Source::Url(url.to_string()), true, window, cx);
    }

    /// The pages of a task long out of view close, their addresses kept:
    /// its Chrome ends a minute after, and a tab opens its page again when shown.
    pub fn put_pages_to_sleep(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if let TabKind::Browser(view) = &tab.kind {
                view.update(cx, |v, cx| v.sleep(cx));
            }
        }
    }

    /// A tab for a page of the task's Chrome, opened in it or not: one a
    /// page opened comes up as a tab in front, one an agent's Playwright
    /// opened as a tab behind, not to take the user's eyes from their work.
    fn add_browser(&mut self, source: crate::browser::Source, activate: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self._new_pages.is_none() {
            let events = crate::browser::page_events(&self.root);
            self._new_pages = Some(cx.spawn_in(window, async move |this, cx| {
                while let Ok(event) = events.recv().await {
                    let done = this.update_in(cx, |t, window, cx| match event {
                        crate::browser::PageEvent::Opened { target, by_page } => {
                            t.add_browser(crate::browser::Source::Target(target), by_page, window, cx)
                        }
                        crate::browser::PageEvent::Downloaded(path) => {
                            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                            let told = format!("{name} is in {}", path.parent().map(|p| p.display().to_string()).unwrap_or_default());
                            window.push_notification(gpui_kit::component::notification::Notification::success(told).title("Downloaded"), cx);
                        }
                        crate::browser::PageEvent::DownloadFailed(name) => {
                            let told = format!("{name} did not download");
                            window.push_notification(gpui_kit::component::notification::Notification::warning(told).title("Download"), cx);
                        }
                    });
                    if done.is_err() {
                        break;
                    }
                }
            }));
        }
        let owner = crate::browser::Owner::Task(self.root.clone());
        let view = cx.new(|cx| crate::browser::BrowserView::new(source, owner, window, cx));
        view.update(cx, |page, _| page.set_places(self.own_places()));
        cx.subscribe_in(&view, window, |this, view, event: &crate::browser::BrowserEvent, window, cx| match event {
            crate::browser::BrowserEvent::Navigated => {
                let title = view.read(cx).title();
                if let Some(tab) = this.tabs.iter_mut().find(|t| matches!(&t.kind, TabKind::Browser(v) if v == view)) {
                    tab.shown = title;
                }
                cx.notify()
            }
            crate::browser::BrowserEvent::ToAgent(message) => this.paste_to_agent(message.clone(), window, cx),
            crate::browser::BrowserEvent::Closed => {
                if let Some(ix) = this.tabs.iter().position(|t| matches!(&t.kind, TabKind::Browser(v) if v == view)) {
                    this.remove_tab(ix, window, cx);
                    cx.emit(TaskEvent::Changed);
                }
            }
        })
        .detach();
        let shown = view.read(cx).title();
        let mut tab = TabItem { shown, ..TabItem::new("web", TabKind::Browser(view)) };
        tab.pane = self.current;
        self.tabs.push(tab);
        if activate {
            self.activate(self.tabs.len() - 1, window, cx);
        } else {
            self.sync_visibility(cx);
            cx.notify();
        }
    }

    /// The task's ports, and where its browser is, for what runs in it.
    fn terminal_env(&self) -> Vec<(String, String)> {
        let mut env = crate::services::task_env(&self.services, self.port_slot);
        env.push(crate::browser::task_env(&self.root));
        env
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
        self.tabs[at].pane = self.current;
        self.activate(at, window, cx);
    }

    /// A shell in `dir`, in a new tab shown in the pane worked in.
    pub(super) fn new_terminal_in(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_seq += 1;
        let at = self.terminal_tab_in(self.terminal_seq, Some(dir), window, cx);
        self.tabs[at].pane = self.current;
        self.activate(at, window, cx);
    }

    /// Adds a shell tab after the last terminal and returns its index.
    fn new_terminal_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> usize {
        self.terminal_seq += 1;
        self.terminal_tab(self.terminal_seq, window, cx)
    }

    /// A shell tab numbered `seq`: a restored one gets its old session back.
    fn terminal_tab(&mut self, seq: usize, window: &mut Window, cx: &mut Context<Self>) -> usize {
        self.terminal_tab_in(seq, None, window, cx)
    }

    fn terminal_tab_in(&mut self, seq: usize, cwd: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> usize {
        self.terminal_seq = self.terminal_seq.max(seq);
        let env = self.terminal_env();
        let tab = Self::make_terminal(&self.root, self.hook_socket.as_ref(), Term::Shell { title: format!("shell {seq}"), seq }, None, env, cwd, window, cx);
        // After the last terminal, so terminals stay together left of the views.
        let at = self.tabs.iter().rposition(|t| matches!(t.kind, TabKind::Terminal(_))).map_or(0, |ix| ix + 1);
        self.tabs.insert(at, tab);
        at
    }

    /// The agent terminal's title, which Claude Code uses as a status line.
    pub fn agent_title(&self, _: &App) -> Option<SharedString> {
        self.tabs.iter().find_map(|tab| match &tab.kind {
            TabKind::Terminal(_) if tab.agent == Some(Who::Main) => Some(tab.shown.clone()),
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
    /// Files with an open transaction or cell changes not saved: what ending this task would lose.
    pub fn sql_at_risk(&self, cx: &App) -> Vec<String> {
        self.all_tabs()
            .filter_map(|tab| match &tab.kind {
                TabKind::File(file) if file.read(cx).sql_holds(cx) => Some(tab.title.to_string()),
                _ => None,
            })
            .collect()
    }

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

    /// Every terminal of the task, those of questions put away too.
    pub fn set_terminal_colors(&mut self, colors: &gpui_cn::terminal::TerminalColors, cx: &mut Context<Self>) {
        for tab in self.all_tabs() {
            if let TabKind::Terminal(view) = &tab.kind {
                let state = view.read(cx).state.clone();
                state.update(cx, |s, cx| s.set_colors(colors.clone(), cx));
            }
        }
    }

    pub fn set_terminal_appearance(&mut self, appearance: &gpui_cn::terminal::TerminalAppearance, cx: &mut Context<Self>) {
        for tab in self.all_tabs() {
            if let TabKind::Terminal(view) = &tab.kind {
                let state = view.read(cx).state.clone();
                state.update(cx, |s, cx| s.set_appearance(appearance.clone(), cx));
            }
        }
    }

    /// What of the agent's work the task and its row show, to tell a report
    /// that changes it from one that does not.
    pub fn agent_shown(&self) -> (Option<SharedString>, Option<SharedString>, usize) {
        (self.agent.doing.clone().map(SharedString::from), self.agent_editing.clone(), self.agent_touched.len())
    }

    pub fn set_agent_status(&mut self, who: Who, status: crate::agent::AgentStatus, seen: bool, cx: &mut Context<Self>) {
        let Some(agent) = self.agent_of(who) else { return };
        // An agent at work reports every tool it uses: drawn again only for
        // what shows another way than before.
        let was = agent.status.clone();
        let before = (self.unseen, agent.doing.is_some(), self.agent_editing.is_some());
        if who == Who::Main {
            // The same again (a recap, the reminder a minute later) tells nothing new:
            // a task already looked at stays out of Needs you.
            let again = status == was && !self.unseen;
            self.unseen = status.is_attention() && !seen && !again;
        }
        if status != crate::agent::AgentStatus::Working {
            self.agent_editing = None;
        }
        if let Who::Question(n) = who
            && let Some(q) = self.questions.iter_mut().find(|q| q.id == n)
        {
            let busy = matches!(status, crate::agent::AgentStatus::Working | crate::agent::AgentStatus::NeedsInput(_));
            q.quiet_since = if busy { None } else { q.quiet_since.or(Some(std::time::Instant::now())) };
            if status == crate::agent::AgentStatus::Done {
                self.question_talked(n);
            }
        }
        let Some(agent) = self.agent_of_mut(who) else { return };
        if status != crate::agent::AgentStatus::Working {
            agent.doing = None;
        }
        let doing = agent.doing.is_some();
        agent.status = status.clone();
        let changed = status != was || before != (self.unseen, doing, self.agent_editing.is_some());
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

    /// A file a question's agent is about to write: marked as the task's
    /// agent's are, without starting their list over.
    pub fn mark_written(&mut self, n: u32, file: &str) -> Option<String> {
        let rel = path_in(&self.root, std::path::Path::new(file))?;
        Rc::make_mut(&mut self.question_touched).insert(SharedString::from(rel.clone()), n);
        Some(rel)
    }

    /// A new prompt starts a question's list over, as the task's agent's; so does its end.
    pub fn forget_written(&mut self, n: u32) {
        if self.question_touched.values().any(|of| *of == n) {
            Rc::make_mut(&mut self.question_touched).retain(|_, of| *of != n);
        }
    }

    /// "editing" by the file the agent writes to now, a dot by those it
    /// wrote to since the last prompt.
    fn agent_mark(&self, path: &str) -> Option<Div> {
        let editing = self.agent_editing.as_deref() == Some(path);
        // What a question's agent wrote, told from what the task's did.
        let asked = self.question_touched.get(path).is_some_and(|n| self.questions.iter().any(|q| q.id == *n));
        (editing || self.agent_touched.contains(path) || asked)
            .then(|| div().text_xs().text_color(gpui_kit::blue()).child(if editing { "editing" } else if asked { "?" } else { "●" }))
    }

    /// The open files look again at what they differ from: after the
    /// setting of what the gutter compares with changed.
    /// Hints shown or not, blame moved: each open file shows what it should.
    pub fn code_settings_changed(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if let TabKind::File(file) = &tab.kind {
                file.update(cx, |f, cx| {
                    f.show_line_blame(cx);
                    f.editor.update(cx, |state, cx| state.refresh_inlay_hints(cx));
                });
            }
        }
    }

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

    /// The agent's turn ended: what became of the review comments it was sent.
    pub fn recheck_review(&mut self, cx: &mut Context<Self>) {
        self.changes.update(cx, |c, cx| c.recheck_review(false, cx));
    }

    /// The branch work starts from, as compared with (`origin/main`).
    pub fn base_ref(&self) -> Option<&str> {
        self.branch_info.base.as_deref()
    }

    /// The base branch, as named for short (`main`), when it changed, as
    /// last fetched, the same lines as this branch.
    pub fn base_clash(&self) -> Option<&str> {
        let base = self.branch_info.base.as_deref().filter(|_| !self.branch_info.base_conflicts.is_empty())?;
        Some(base.strip_prefix("origin/").unwrap_or(base))
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
            // How the lists are shown may have changed in another task meanwhile.
            self.reorder_diffs(cx);
            // What it skipped while out of view.
            let _ = self.refresh_tx.try_send(());
            self.show_diff_if_seen(cx);
            self.fill_tree(cx);
            self.focus_active(window, cx);
            if self.panel_shown(RightPane::Run) && self._listening_poll.is_none() {
                self.refresh_services(cx);
                self.watch_listening(cx);
            }
        }
    }

    /// Hidden terminals keep parsing but stop producing frames.
    fn sync_visibility(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            let shown = self.is_shown(tab.id);
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
        self.shown_in(self.current).and_then(|id| self.ix_of(id))
    }

    /// Whether the keyboard is with the tab in front, for tests.
    #[cfg(test)]
    pub fn front_tab_has_focus(&self, window: &Window, cx: &App) -> bool {
        match self.focused_ix().and_then(|ix| self.tabs.get(ix)).map(|t| &t.kind) {
            Some(TabKind::Terminal(view)) => view.read(cx).state.read(cx).focus_handle().is_focused(window),
            Some(TabKind::Chat(chat)) => chat.read(cx).is_focused(window, cx),
            Some(TabKind::File(file)) => file.read(cx).editor.read(cx).focus_handle(cx).is_focused(window),
            Some(TabKind::Browser(view)) => view.focus_handle(cx).is_focused(window),
            Some(TabKind::Changes) => self.changes.read(cx).diff.read(cx).focus_handle(cx).is_focused(window),
            _ => self.focus.is_focused(window),
        }
    }

    pub(crate) fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
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
            Some(TabKind::Chat(chat)) => {
                let handle = chat.read(cx).focus_handle(cx);
                handle.focus(window, cx)
            }
            Some(TabKind::File(file)) => file.update(cx, |f, cx| f.focus(window, cx)),
            Some(TabKind::Binary(view)) => {
                if let Some(picture) = &view.read(cx).picture {
                    picture.read(cx).focus_handle().clone().focus(window, cx)
                } else {
                    self.focus.focus(window, cx)
                }
            }
            Some(TabKind::Browser(view)) => view.focus_handle(cx).focus(window, cx),
            // Its keys (J, K, N, V…) are the diff's.
            Some(TabKind::Changes) => self.changes.read(cx).diff.read(cx).focus_handle(cx).focus(window, cx),
            _ => self.focus.focus(window, cx),
        }
    }

    /// Shows tab `ix` in its own pane and focuses that pane.
    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        if let (TabKind::Asleep, Some(Who::Question(n))) = (&self.tabs[ix].kind, self.tabs[ix].agent) {
            return self.show_question(n, window, cx);
        }
        if let (Some(Who::Question(n)), true) = (self.tabs[ix].agent, self.visible) {
            cx.emit(TaskEvent::Asked(Asked::Seen(n)));
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
        self.active[tab.pane.ix()] = Some(tab.id);
        self.current = tab.pane;
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
            Some(TabKind::File(file)) if file.read(cx).dirty || file.read(cx).sql_holds(cx) => {
                let title = self.tabs[ix].title.clone();
                let description = match (file.read(cx).sql_holds(cx), file.read(cx).dirty) {
                    (true, true) => "The unsaved edits will be lost, and so will its uncommitted transaction or cell changes not applied.",
                    (true, false) => "Its uncommitted transaction or cell changes not applied will be lost.",
                    _ => "The unsaved edits will be lost.",
                };
                let task = cx.weak_entity();
                let file_id = file.entity_id();
                window.open_alert_dialog(cx, move |dialog, _, _| {
                    let task = task.clone();
                    dialog
                        .confirm()
                        .title(format!("Discard changes to {title}?"))
                        .description(description)
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
            Some(TabKind::File(_) | TabKind::Browser(_) | TabKind::Binary(_) | TabKind::Diff(_) | TabKind::Changes | TabKind::History) => {
                self.remember_closed(ix, cx);
                self.remove_tab(ix, window, cx)
            }
            Some(TabKind::Asleep) => {
                if matches!(self.tabs[ix].agent, Some(Who::Question(_))) {
                    self.remove_tab(ix, window, cx);
                }
            }
            Some(TabKind::Terminal(_) | TabKind::Chat(_)) if self.tabs[ix].agent.is_some() => {
                if let Some(Who::Question(n)) = self.tabs[ix].agent {
                    self.hide_question(n, window, cx);
                }
            }
            Some(TabKind::Chat(_)) => {}
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
        self.closed.push(ClosedTab { path, line, col, pane: tab.pane });
        if self.closed.len() > 30 {
            self.closed.remove(0);
        }
    }

    pub fn reopen_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(closed) = self.closed.pop() {
            if closed.path.is_file() {
                self.current = if self.has(closed.pane) { closed.pane } else { self.current };
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

    /// The tabs `which` picks in a pane that [`closed_in_bulk`] allows.
    fn close_many(&mut self, pane: Pane, which: impl Fn(usize, &TabItem) -> bool, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<usize> = self.closed_together(pane, which, cx);
        for id in ids {
            if let Some(ix) = self.ix_of(id) {
                self.remember_closed(ix, cx);
                self.remove_tab(ix, window, cx);
            }
        }
    }

    /// The ids of the tabs [`Self::close_many`] would close.
    fn closed_together(&self, pane: Pane, which: impl Fn(usize, &TabItem) -> bool, cx: &App) -> Vec<usize> {
        self.tabs.iter().enumerate().filter(|(ix, t)| t.pane == pane && which(*ix, t) && closed_in_bulk(t, cx)).map(|(_, t)| t.id).collect()
    }

    fn toggle_pin(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.pinned = !tab.pinned;
            tab.preview = false;
            cx.notify();
        }
    }

    fn remove_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.take_tab(ix, window, cx);
    }

    /// Out of the tabs, and given back: a question's, kept while hidden.
    fn take_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> TabItem {
        let removed = self.tabs.remove(ix);
        // The diff laid out and coloured goes with its tab; it is made
        // again from what the task keeps when the tab is next looked at.
        if matches!(removed.kind, TabKind::Changes) {
            let diff = self.changes.read(cx).diff.clone();
            diff.update(cx, |state, cx| state.set_files([], cx));
            self.changes.update(cx, |changes, cx| {
                changes.set_hunks(Rc::default(), cx);
                changes.first_lines = Rc::default();
            });
            self.diff_stale = true;
        }
        // The server keeps every text it was given until told otherwise.
        if let TabKind::File(file) = &removed.kind {
            self.keep_bookmarks(file, cx);
            let f = file.read(cx);
            // A scratch file left empty is nothing to keep; what is on disk decides.
            if f.sql.is_some() && f.editor.read(cx).text().len() == 0 && crate::db::scratch::is_scratch(&f.path) {
                crate::db::scratch::drop_if_empty(f.path.clone());
            }
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
            .find(|t| t.pane == removed.pane)
            .map(|t| t.id);
        if self.active[removed.pane.ix()] == Some(removed.id) {
            self.active[removed.pane.ix()] = neighbour;
        }
        self.normalize();
        self.sync_visibility(cx);
        // The tab that comes to the front may be Changes, with an older diff.
        self.show_diff_if_seen(cx);
        self.focus_active(window, cx);
        cx.notify();
        removed
    }

    pub fn show_view(&mut self, which: usize, window: &mut Window, cx: &mut Context<Self>) {
        match which {
            0 => {
                self.start_main_agent(window, cx);
                if let Some(ix) = self.agent_ix(Who::Main).or_else(|| self.tabs.iter().position(|t| t.title == "shell")) {
                    self.activate(ix, window, cx);
                }
            }
            1 => self.show_tab(|k| matches!(k, TabKind::Changes), window, cx),
            2 => self.show_tab(|k| matches!(k, TabKind::History), window, cx),
            _ => match self.tabs.iter().position(|t| t.agent.is_none() && matches!(t.kind, TabKind::Terminal(_))) {
                Some(ix) => self.activate(ix, window, cx),
                None => self.new_terminal(window, cx),
            },
        }
    }

    /// Alt+1/2/3: shows the panel and moves the keyboard there. Arrows move,
    /// Enter opens, Escape goes back to the tab.
    pub fn show_side_panel(&mut self, which: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_focus_mode(window, cx);
        self.reveal_side_panel(which, cx);
        match self.keyed {
            RightPane::Files => self.tree.update(cx, |t, cx| t.focus(window, cx)),
            _ => self.side_focus.focus(window, cx),
        }
    }

    /// The panel comes up and the keyboard stays where it is.
    pub fn reveal_side_panel(&mut self, which: usize, cx: &mut Context<Self>) {
        self.side_over = true;
        self.keyed = strip::PANES[which.min(strip::PANES.len() - 1)];
        self.open_panel(self.keyed, cx);
        self.side_selected = match self.keyed {
            // Start on the declaration the cursor is in.
            RightPane::Structure => at_cursor(&self.structure_items(cx), self.structure_cursor(cx)).unwrap_or(0),
            _ => 0,
        };
        self.structure_select = self.keyed == RightPane::Structure && !self.structure_ready();
        cx.notify();
    }

    fn side_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let len = match self.keyed {
            RightPane::Changes => self.files.len(),
            RightPane::Structure => self.structure_items(cx).len(),
            RightPane::Files | RightPane::Run | RightPane::Database => return,
        };
        match event.keystroke.key.as_str() {
            "down" if len > 0 => self.side_selected = (self.side_selected + 1).min(len - 1),
            "up" => self.side_selected = self.side_selected.saturating_sub(1),
            "enter" if len > 0 => {
                let ix = self.side_selected.min(len - 1);
                match self.keyed {
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

    /// The outline of the Structure file, parsed again only after edits;
    /// none until `refresh_structure` has made it (a long file takes tens of
    /// milliseconds).
    fn structure_items(&self, _: &App) -> Rc<Vec<crate::finder::OutlineItem>> {
        let Some(id) = self.structure_file else { return Rc::default() };
        // While you type, the outline of a moment ago: `refresh_structure`
        // brings the new one once the typing pauses.
        let all: Rc<Vec<crate::finder::OutlineItem>> = self.structure.borrow().as_ref().filter(|(i, _, _)| *i == id).map(|(_, _, items)| items.clone()).unwrap_or_default();
        let (public, fields, sorted) = crate::state::with_settings(|s| (s.structure_public, !s.structure_no_fields, s.structure_sorted));
        if !public && fields && !sorted {
            return all;
        }
        // What the panel's buttons leave out, with all that is declared in it, and the order asked: made once for an outline, not with every frame.
        let made_of = (Rc::as_ptr(&all) as usize, public, fields, sorted);
        if let Some((_, shown)) = self.structure_shown.borrow().as_ref().filter(|(of, _)| *of == made_of) {
            return shown.clone();
        }
        let mut shown = Vec::new();
        let mut under: Option<usize> = None;
        for item in all.iter() {
            if under.is_some_and(|depth| item.depth > depth) {
                continue;
            }
            let field = crate::finder::kind_mark(&item.kind).0 == "v";
            under = ((public && item.private) || (!fields && field)).then_some(item.depth);
            if under.is_none() {
                shown.push(item.clone());
            }
        }
        if sorted {
            shown = sorted_by_name(shown);
        }
        let shown = Rc::new(shown);
        *self.structure_shown.borrow_mut() = Some((made_of, shown.clone()));
        shown
    }

    /// What is selected in the file the keyboard is in, when it is a few
    /// words on one line: what a search started from there looks for.
    fn selected_words(&self, cx: &App) -> String {
        let Some(TabKind::File(file)) = self.focused_ix().map(|ix| &self.tabs[ix].kind) else { return String::new() };
        let editor = file.read(cx).editor.read(cx);
        let range = editor.selected_range();
        if range.is_empty() || range.len() > 200 {
            return String::new();
        }
        let text = editor.selected_text().to_string();
        if text.contains('\n') { String::new() } else { text }
    }

    /// The file whose structure is to show: the one in front where the
    /// keyboard is, or the one beside a terminal that has it.
    fn file_in_view(&self) -> Option<usize> {
        let file = |id: usize| self.tabs.iter().any(|t| t.id == id && matches!(t.kind, TabKind::File(_))).then_some(id);
        let focused = self.focused_ix().map(|ix| self.tabs[ix].id);
        let beside = Pane::ALL.into_iter().filter(|&pane| pane != self.current).find_map(|pane| self.shown_in(pane).and_then(file));
        focused.and_then(file).or(beside)
    }

    /// Whether the outline is that of the Structure file, if of an older text.
    fn structure_ready(&self) -> bool {
        self.structure_file.is_some_and(|id| self.structure.borrow().as_ref().is_some_and(|(i, _, _)| *i == id))
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
        // Another file's is made at once; the same file's once typing pauses.
        let typing = self.structure_ready();
        let job = cx.spawn(async move |this, cx| {
            if typing {
                cx.background_executor().timer(Duration::from_millis(200)).await;
            }
            let (version, text) = file.read_with(cx, |f, cx| (f.edit_version, f.editor.read(cx).text().to_string()));
            let items = cx.background_spawn(async move { outline(&path, &text) }).await;
            let _ = this.update(cx, |this, cx| {
                *this.structure.borrow_mut() = Some((id, version, Rc::new(items)));
                if this.structure_file == Some(id) && std::mem::take(&mut this.structure_select) && this.keyed == RightPane::Structure {
                    // In the outline as it is shown: some of it may be left out, or in another order.
                    this.side_selected = at_cursor(&this.structure_items(cx), this.structure_cursor(cx)).unwrap_or(0);
                }
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
                tab.pane = self.current;
                self.tabs.push(tab);
                self.tabs.len() - 1
            }
        };
        self.activate(ix, window, cx);
    }

    /// An agent's change of `path` (absolute or in the task) in the Changes
    /// tab, at its line, where it is staged or reverted by parts.
    pub(super) fn review_change(&mut self, path: &str, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        let rel = std::path::Path::new(path).strip_prefix(&self.root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string());
        if !self.changed_paths.contains(rel.as_str()) {
            window.push_notification(Notification::info(format!("{rel} has no uncommitted change now: committed or put back since.")), cx);
            return;
        }
        self.changes.update(cx, |c, cx| c.set_scope(DiffScope::Uncommitted, cx));
        // Beside the chat, as the files it follows, not over it.
        self.show_changed_file(rel.clone().into(), window, cx);
        if let (Some(ix), Some(chat)) = (self.tabs.iter().position(|t| matches!(t.kind, TabKind::Changes)), self.agent_chat(Who::Main)) {
            self.beside_chat(&chat, ix, window, cx);
            self.show_changed_file(rel.clone().into(), window, cx);
            if let Some(chat) = self.agent_ix(Who::Main) {
                self.activate(chat, window, cx);
            }
        }
        self.changes.update(cx, |c, cx| {
            c.show_change_at(&rel, line, cx);
        });
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

/// The declaration the cursor is in: the last to begin at or above its
/// line, whatever order the outline is shown in.
pub(super) fn at_cursor(items: &[crate::finder::OutlineItem], cursor: u32) -> Option<usize> {
    items.iter().enumerate().filter(|(_, item)| item.line <= cursor).max_by_key(|(_, item)| item.line).map(|(ix, _)| ix)
}

/// An outline with what is declared side by side in the order of the
/// names, each still followed by what is declared in it.
fn sorted_by_name(items: Vec<crate::finder::OutlineItem>) -> Vec<crate::finder::OutlineItem> {
    fn sort(items: &[crate::finder::OutlineItem], out: &mut Vec<crate::finder::OutlineItem>) {
        let Some(depth) = items.iter().map(|item| item.depth).min() else { return };
        // Each of this depth with all that follows it deeper.
        let starts: Vec<usize> = (0..items.len()).filter(|&ix| items[ix].depth == depth).collect();
        let mut groups: Vec<&[crate::finder::OutlineItem]> = starts.iter().enumerate().map(|(n, &at)| &items[at..starts.get(n + 1).copied().unwrap_or(items.len())]).collect();
        let name = |item: &crate::finder::OutlineItem| item.name.to_lowercase();
        groups.sort_by_cached_key(|group| name(&group[0]));
        // What comes before the first of this depth is deeper and has no one above it: it stays first.
        sort_rest(&items[..starts.first().copied().unwrap_or(0)], out);
        for group in groups {
            out.push(group[0].clone());
            sort(&group[1..], out);
        }
    }
    fn sort_rest(items: &[crate::finder::OutlineItem], out: &mut Vec<crate::finder::OutlineItem>) {
        out.extend(items.iter().cloned());
    }
    let mut out = Vec::with_capacity(items.len());
    sort(&items, &mut out);
    out
}

impl Render for TaskView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.side_has_focus.set(self.side_focus.is_focused(window) || self.peek_focus.is_focused(window));
        self.settle_side(cx);
        // Structure is of the file in view: with none there it shows none, not the one that was.
        let in_view = self.file_in_view();
        self.structure_file = in_view;
        // Told apart from the Structure file, which a tab coming to the front has set already.
        if std::mem::replace(&mut self.tree_followed, in_view) != in_view && in_view.is_some() && self.panel_shown(RightPane::Files) && crate::state::with_settings(|s| s.tree_follows) {
            // Not while drawing: the tree is another view.
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.locate_in_tree(cx));
            })
            .detach();
        }
        // Narrow, the side panel would leave the task too little: it goes over it.
        let narrow = window.viewport_size().width < NARROW_WINDOW;
        if narrow != self.narrow {
            self.narrow = narrow;
            self.side_over = false;
        }
        if self.action != self.told_action {
            self.told_action = self.action.clone();
            let seen = self.panel_shown(RightPane::Changes) && self.zoomed.is_none();
            if let Some((ok, message)) = self.action.clone().filter(|_| !seen && self.visible) {
                let note = if ok { Notification::info(message) } else { Notification::error(message) };
                window.push_notification(note.title("Git"), cx);
            }
        }
        let content = self.render_panes(cx);
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
            .on_action(cx.listener(|this, _: &CopyReference, _, cx| this.copy_reference(cx)))
            .on_action(cx.listener(|this, _: &CopyFilePath, _, cx| this.copy_file_path(cx)))
            .on_action(cx.listener(|this, _: &ShowFileInFiles, _, cx| this.show_file_in_files(cx)))
            .on_action(cx.listener(|this, _: &ShowBranches, window, cx| this.branches_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &CompareFileWith, window, cx| this.compare_open_file(window, cx)))
            .map(|view| {
                let main = v_flex().size_full().min_w_0().child(self.render_branch(cx)).children(self.render_off_default(cx)).children(self.render_off_own(cx)).child(div().flex_1().min_h_0().child(content));
                let held = self.side != [None, None];
                // Narrow, the side would leave the task too little: it is over it, on asking.
                let body: AnyElement = if narrow || !held {
                    main.into_any_element()
                } else {
                    h_resizable("task")
                        .on_resize(|state, _, cx| {
                            if let Some(width) = state.read(cx).sizes().get(1).copied() {
                                cx.set_global(SidePanelWidth(width));
                            }
                        })
                        .child(resizable_panel().child(main))
                        .child(
                            resizable_panel()
                                .size(cx.try_global::<SidePanelWidth>().map_or(px(300.), |w| w.0))
                                .size_range(px(200.)..px(640.))
                                .fixed()
                                .child(self.render_side(cx)),
                        )
                        .into_any_element()
                };
                let over = (narrow && held && self.side_over).then(|| {
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .h_full()
                        .w(px(320.))
                        .bg(cx.theme().background)
                        .shadow_lg()
                        .occlude()
                        .child(self.render_side(cx))
                });
                view.child(
                    h_flex()
                        .size_full()
                        .child(div().relative().flex_1().min_w_0().h_full().child(body).children(over).children(self.render_peek(cx)))
                        .child(self.render_strip(cx)),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod structure_tests {
    use super::{at_cursor, sorted_by_name};
    use crate::finder::OutlineItem;

    #[test]
    fn an_outline_by_name_keeps_what_is_declared_in_each() {
        let item = |depth: usize, name: &str, line: u32| OutlineItem { depth, kind: "fn".into(), name: name.into(), line, end: line, private: false };
        let file = vec![item(0, "Zeta", 0), item(1, "run", 1), item(1, "Add", 2), item(0, "alpha", 3), item(0, "Beta", 4), item(1, "x", 5)];
        let shown = sorted_by_name(file);
        let names: Vec<(usize, &str)> = shown.iter().map(|i| (i.depth, i.name.as_str())).collect();
        assert_eq!(names, [(0, "alpha"), (0, "Beta"), (1, "x"), (0, "Zeta"), (1, "Add"), (1, "run")]);
        // The cursor on line 2 is in Add, wherever that is in the list.
        assert_eq!(at_cursor(&shown, 2).map(|ix| shown[ix].line), Some(2));
        assert_eq!(at_cursor(&shown, 0).map(|ix| shown[ix].line), Some(0));
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
    use super::panes::Pane;
    use super::{FileView, TabKind, TaskView};
    use crate::state::{ProjectState, State};
    use crate::workspace::Workspace;
    use gpui_kit::{App, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds, WindowOptions, point, px, size};
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
                t.open_panel(super::RightPane::Changes, cx);
            });
            window.render_frame(cx);
            let row = window.find(("file", 0usize));
            assert!(row.visible() && row.bounds().size.height <= px(super::git_ui::CHANGE_ROW), "the changed file's row, whole in the room every row has: {:?}", row.bounds());
        })
        .unwrap();
        // The side: a strip of marks, a panel in each half, and one that comes over the task from its mark.
        use super::RightPane::{Changes, Structure};
        let frame = |cx: &mut TestAppContext, then: &mut dyn FnMut(&mut gpui_kit::Window, &mut gpui_kit::App)| {
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                then(window, cx)
            })
            .unwrap()
        };
        frame(cx, &mut |window, _| assert!(window.find(("side-mark", Changes as usize)).visible() && window.try_find("side-peek").is_none()));
        let whole = cx.update_window(handle.into(), |_, window, _| window.find(("file", 0usize)).bounds()).unwrap();
        cx.update_window(handle.into(), |_, _, cx| task.update(cx, |t, cx| t.move_panel(Structure, true, cx))).unwrap();
        task.update(cx, |t, cx| {
            assert_eq!(t.side, [Some(Changes), Some(Structure)], "moved to the lower half, it is shown there");
            let session = t.session(cx);
            assert_eq!((session.side_closed, session.side_panel, session.side_below), (false, 0, Some(2)));
        });
        frame(cx, &mut |window, _| assert_eq!(window.find(("file", 0usize)).bounds().origin, whole.origin, "and Changes is above it, where it was"));
        // Both put away, the strip is all there is of the side.
        cx.update_window(handle.into(), |_, _, cx| {
            task.update(cx, |t, cx| {
                t.close_panel(Changes, cx);
                t.close_panel(Structure, cx);
            })
        })
        .unwrap();
        frame(cx, &mut |window, _| assert!(window.try_find(("file", 0usize)).is_none() && window.find(("side-mark", Structure as usize)).visible()));
        task.update(cx, |t, cx| assert!(t.session(cx).side_closed && t.session(cx).side_below.is_none()));
        // Not while the side holds a panel: one put there by a click is not taken over by one the pointer passed.
        cx.update_window(handle.into(), |_, window, cx| {
            task.update(cx, |t, cx| {
                t.open_panel(Structure, cx);
                t.peek_panel(Some(Changes), window, cx);
            })
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_millis(800));
        cx.run_until_parked();
        task.update(cx, |t, cx| {
            assert_eq!((t.side, t.side_peek), ([None, Some(Structure)], None));
            t.close_panel(Structure, cx);
        });
        // The pointer rests on a mark: after a while its panel is over the task, and goes a while after the pointer has.
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.peek_panel(Some(Changes), window, cx))).unwrap();
        frame(cx, &mut |window, _| assert!(window.try_find("side-peek").is_none(), "not at once"));
        cx.executor().advance_clock(Duration::from_millis(800));
        cx.run_until_parked();
        frame(cx, &mut |window, _| assert!(window.find("side-peek").visible() && window.find(("file", 0usize)).visible()));
        task.update(cx, |t, _| assert_eq!((t.side, t.side_peek), ([None, None], Some(Changes)), "over the task, not held"));
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.peek_panel(None, window, cx))).unwrap();
        cx.executor().advance_clock(Duration::from_millis(400));
        cx.run_until_parked();
        frame(cx, &mut |window, _| assert!(window.try_find("side-peek").is_none()));
        // A click on the mark holds it, in the half it belongs to.
        cx.update_window(handle.into(), |_, _, cx| {
            task.update(cx, |t, cx| {
                t.move_panel(Structure, false, cx);
                t.close_panel(Structure, cx);
                t.open_panel(Changes, cx);
            })
        })
        .unwrap();
        task.update(cx, |t, _| assert_eq!(t.side, [Some(Changes), None]));
        // Structure is of the file in view: with a terminal in front it is of none, not of the file that was.
        let at = |t: &TaskView, file: bool| t.tabs.iter().position(|tab| matches!(tab.kind, TabKind::File(_)) == file).unwrap();
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.activate(at(t, true), window, cx))).unwrap();
        frame(cx, &mut |_, _| {});
        task.update(cx, |t, _| assert_eq!(t.structure_file, Some(t.tabs[at(t, true)].id)));
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.activate(at(t, false), window, cx))).unwrap();
        frame(cx, &mut |_, _| {});
        task.update(cx, |t, _| assert_eq!(t.structure_file, None));
        // The tree shows the file in the editor on asking, and closes every folder.
        cx.update_window(handle.into(), |_, window, cx| {
            task.update(cx, |t, cx| {
                t.activate(at(t, true), window, cx);
                t.open_panel(super::RightPane::Files, cx);
            })
        })
        .unwrap();
        frame(cx, &mut |window, _| assert!(window.find("tree-locate").visible() && window.find("tree-collapse").visible()));
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
        // The cursor was on a line the agent replaced: it stays in what replaced it.
        assert_eq!((shown(cx), on_disk()), ("mine agent\n".to_string(), "theirs\n".to_string()));

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
                t.close_many(Pane::MAIN, |_, tab| tab.id != b, window, cx);
            })
        })
        .unwrap();
        assert_eq!(open_files(cx), ["a.txt", "b.txt"], "a.txt pinned, so no longer the preview either");
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.reopen_closed(window, cx))).unwrap();
        assert_eq!(open_files(cx), ["a.txt", "b.txt", "c.txt"]);

        // Set to follow, the tree goes to each file that comes to the front, not to the first only.
        let before = crate::state::settings();
        crate::state::set_settings(crate::state::Settings { tree_follows: true, ..before.clone() });
        cx.update_window(handle.into(), |_, _, cx| task.update(cx, |t, cx| t.open_panel(super::RightPane::Files, cx))).unwrap();
        let chosen = |cx: &mut TestAppContext| task.update(cx, |t, cx| t.tree.read(cx).selected_item().map(|item| item.id.to_string()));
        refreshed(cx);
        for name in ["b.txt", "a.txt", "c.txt", "a.txt"] {
            let file = repo.join(name);
            cx.update_window(handle.into(), |_, window, cx| {
                task.update(cx, |t, cx| t.open_file(file.clone(), 0, window, cx));
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
            assert_eq!(chosen(cx).as_deref(), Some(name));
        }
        crate::state::set_settings(before);

        // Panes: a tab split down opens the left column's lower pane, the
        // right column comes beside, the session keeps them, and a pane's
        // last tab gone, its tabs' column or floor closes up.
        let pane_of = |cx: &mut TestAppContext, name: &str| {
            task.update(cx, |t, cx| t.tabs.iter().find(|tab| matches!(&tab.kind, TabKind::File(f) if f.read(cx).path.ends_with(name))).map(|tab| tab.pane))
        };
        let move_file = |cx: &mut TestAppContext, name: &str, label: &str| {
            cx.update_window(handle.into(), |_, window, cx| {
                task.update(cx, |t, cx| {
                    let ix = t.tabs.iter().position(|tab| matches!(&tab.kind, TabKind::File(f) if f.read(cx).path.ends_with(name))).unwrap();
                    let moves = t.moves_of(ix);
                    let (_, to) = moves.iter().find(|(l, _)| l == label).unwrap_or_else(|| panic!("{label} not in {moves:?}"));
                    t.move_to(ix, *to, window, cx);
                })
            })
            .unwrap();
        };
        let lower_left = Pane { right: false, below: true };
        let upper_right = Pane { right: true, below: false };
        move_file(cx, "a.txt", "Split down");
        assert_eq!(pane_of(cx, "a.txt"), Some(lower_left));
        task.update(cx, |t, _| assert_eq!(t.current, lower_left, "the pane it went to is worked in"));
        move_file(cx, "c.txt", "Split right");
        assert_eq!(pane_of(cx, "c.txt"), Some(upper_right));
        let saved = task.update(cx, |t, cx| t.session(cx));
        assert!(saved.tabs.iter().any(|tab| tab.below && !tab.right) && saved.left_below_active.is_some());
        // a.txt back up: the lower pane closes.
        move_file(cx, "a.txt", "Move to the upper left pane");
        assert_eq!(pane_of(cx, "a.txt"), Some(Pane::MAIN));
        task.update(cx, |t, _| assert!(!t.has(lower_left)));
        // The left column emptied of all but the agent's tab, the right one stays.
        assert_eq!(pane_of(cx, "c.txt"), Some(upper_right));

        task.update(cx, |t, cx| t.shut_down(cx));
        cx.update_window(handle.into(), |_, window, _| window.remove_window()).ok();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Markdown, an SVG and a CSV drawn beside their text and instead of
    /// it, the drawing following what is typed once typing pauses.
    #[gpui_kit::test]
    #[ignore]
    fn an_open_file_is_drawn_beside_its_text(cx: &mut TestAppContext) {
        use super::preview::{Drawn, Look};
        use gpui_kit::test::TestWindowExt as _;
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("ideae-drawn-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("repo");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.md"), "# Title\n\ntext\n\n```mermaid\nflowchart LR\n  A --> B\n```\n").unwrap();
        std::fs::write(base.join("f.mmd"), "flowchart LR\n  A --> B\n").unwrap();
        image::RgbaImage::new(300, 200).save(base.join("p.png")).unwrap();
        let long: String = (0..120).map(|i| format!("## Part {i}\n\nText of part {i}.\n\n")).collect();
        std::fs::write(base.join("long.md"), long).unwrap();
        std::fs::write(base.join("t.csv"), "n,v\n1,\"x, y\"\n2,3\n").unwrap();
        std::fs::write(base.join("i.svg"), r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12"/>"#).unwrap();
        let script = "cd repo; git init -q -b main; git config user.email t@t; git config user.name t; git add .; git commit -qm base";
        assert!(std::process::Command::new("sh").arg("-ec").arg(script).current_dir(&dir).status().unwrap().success());
        let repo = dir.join("repo").canonicalize().unwrap();
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
        let open = |cx: &mut TestAppContext, name: &str, look: Look| -> Entity<FileView> {
            let path = repo.join(name);
            cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.open_file(path.clone(), 0, window, cx))).unwrap();
            let file = task.update(cx, |t, _| {
                t.tabs.iter().find_map(|tab| match &tab.kind {
                    TabKind::File(f) => Some(f.clone()),
                    _ => None,
                })
            });
            let file = task.update(cx, |t, cx| {
                t.tabs.iter().find_map(|tab| match &tab.kind {
                    TabKind::File(f) if f.read(cx).path == path => Some(f.clone()),
                    _ => None,
                })
            }).or(file).expect("the file's tab");
            cx.update_window(handle.into(), |_, _, cx| file.update(cx, |f, cx| f.set_look(look, cx))).unwrap();
            cx.run_until_parked();
            file
        };
        let frame = |cx: &mut TestAppContext, check: &dyn Fn(&mut gpui_kit::Window)| {
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                check(window);
            })
            .unwrap();
        };

        let svg = open(cx, "i.svg", Look::Split);
        frame(cx, &|window| {
            let (editor, picture) = (window.find("file-editor"), window.find("svg-picture"));
            assert!(editor.visible() && picture.visible(), "the text and the picture");
            assert!(editor.bounds().right() <= picture.bounds().left(), "side by side: {:?} {:?}", editor.bounds(), picture.bounds());
        });
        svg.update(cx, |f, _| assert!(matches!(&f.preview.drawn, Some((0, Drawn::Svg(Some(p), None))) if p.size == (24., 12.))));
        // Broken while typing: the last picture stays, with why.
        cx.update_window(handle.into(), |_, window, cx| svg.update(cx, |f, cx| f.editor.update(cx, |e, cx| e.insert("<", window, cx)))).unwrap();
        cx.run_until_parked();
        svg.update(cx, |f, _| assert!(matches!(&f.preview.drawn, Some((0, _))), "not drawn again while typing"));
        cx.executor().advance_clock(Duration::from_millis(400));
        cx.run_until_parked();
        svg.update(cx, |f, _| assert!(matches!(&f.preview.drawn, Some((1, Drawn::Svg(Some(_), Some(_)))))));

        // The line between them dragged: the share kept for SVGs, a double click puts it back in the middle.
        cx.update_window(handle.into(), |_, window, cx| {
            let (split, line) = (window.find("file-split").bounds(), window.find("split-handle").bounds());
            let to = point(split.left() + split.size.width * 0.3, line.center().y);
            window.drag(line.center(), to, cx);
            window.render_frame(cx);
            let share = window.find("file-editor").bounds().size.width / split.size.width;
            assert!((share - 0.3).abs() < 0.02, "the text takes what it was dragged to: {share}");
            window.double_click("split-handle", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(crate::state::with_settings(|s| s.split_shares.get("svg").copied()), Some(0.5));

        // Zoomed and moved back to fit.
        frame(cx, &|window| assert!(window.find("picture-zoom").visible()));
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("picture-actual", cx);
            window.render_frame(cx);
            assert_eq!(window.find("picture").bounds().size.width, px(24.), "an SVG at its own size");
            window.click("picture-fit", cx);
            window.render_frame(cx);
            assert!(window.find("picture").bounds().size.width > px(100.), "fit, larger than it is");
        })
        .unwrap();

        // An image file: fit no larger than it is, and at its own size, once its size is read.
        cx.update_window(handle.into(), |_, window, cx| task.update(cx, |t, cx| t.open_file(repo.join("p.png"), 0, window, cx))).unwrap();
        cx.run_until_parked();
        frame(cx, &|window| assert!(window.find("picture").bounds().size.width <= px(300.)));
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("picture-actual", cx);
            window.render_frame(cx);
            assert_eq!(window.find("picture").bounds().size, size(px(300.), px(200.)));
            // Its keys are its zoom while it has the keyboard, over Fold and Unfold, which are the editor's.
            crate::keymap::bind_all(cx);
            window.click("picture", cx);
            window.press(if cfg!(target_os = "macos") { "cmd--" } else { "ctrl--" }, cx);
            window.render_frame(cx);
            assert!(window.find("picture").bounds().size.width < px(300.), "smaller by its key");
            window.press(if cfg!(target_os = "macos") { "cmd-9" } else { "ctrl-9" }, cx);
            window.render_frame(cx);
            assert_eq!(window.find("picture").bounds().size.width, px(300.), "fit by its key: no larger than it is, as the room is larger");
            // Larger than the room: its left edge can be reached, and its right one by scrolling.
            for _ in 0..30 {
                window.click("picture-larger", cx);
            }
            window.render_frame(cx);
            let (room, picture) = (window.find("picture-scroll").bounds(), window.find("picture").bounds());
            assert!(picture.size.width > room.size.width, "wider than its room");
            assert!(picture.left() >= room.left() && picture.top() >= room.top(), "nothing out of reach: {picture:?} in {room:?}");
            let left = picture.left();
            window.scroll("picture-scroll", gpui_kit::ScrollDelta::Pixels(point(px(-500.), px(0.))), cx);
            window.render_frame(cx);
            assert_eq!(window.find("picture").bounds().left(), left - px(500.), "scrolled sideways");
            // Shift and the wheel of a mouse, which only turns one way.
            window.dispatch_event(
                gpui_kit::PlatformInput::Scroll(gpui_kit::ScrollEvent {
                    position: room.center(),
                    delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-100.))),
                    modifiers: gpui_kit::Modifiers::shift(),
                    ..Default::default()
                }),
                cx,
            );
            window.render_frame(cx);
            let moved = window.find("picture").bounds();
            assert_eq!((moved.left(), moved.top()), (left - px(600.), picture.top()), "Shift+wheel goes sideways only");
            window.click("picture-fit", cx);
            window.render_frame(cx);
        })
        .unwrap();

        let csv = open(cx, "t.csv", Look::Preview);
        frame(cx, &|window| {
            assert!(window.try_find("file-editor").is_none_or(|e| !e.visible()), "the table only");
            assert!(window.find("sheet").visible());
        });
        csv.update(cx, |f, _| match &f.preview.drawn {
            Some((_, Drawn::Table(sheet))) => assert_eq!((sheet.rows(), sheet.cell(1, 1).as_ref()), (2, "x, y")),
            _ => panic!("a table"),
        });

        let mmd = open(cx, "f.mmd", Look::Split);
        frame(cx, &|window| assert!(window.find("svg-picture").visible(), "a Mermaid file drawn as a picture"));
        mmd.update(cx, |f, _| assert!(matches!(&f.preview.drawn, Some((_, Drawn::Svg(Some(_), None))))));

        let md = open(cx, "a.md", Look::Preview);
        frame(cx, &|window| assert!(window.find("file-preview").visible()));
        // Its diagram is drawn off this thread, then shown.
        cx.run_until_parked();
        frame(cx, &|_| {});
        frame(cx, &|window| assert!(window.find("diagram").visible(), "the Markdown's diagram as a picture"));
        let saved = task.update(cx, |t, cx| t.session(cx));
        let look_of = |name: &str| saved.tabs.iter().find(|tab| tab.path.as_deref() == Some(name)).and_then(|tab| tab.look.clone());
        assert_eq!((look_of("a.md").as_deref(), look_of("i.svg").as_deref()), (Some("preview"), Some("split")), "kept with the session");
        // Its keys work while the text is not shown: Ctrl+Shift+V goes on to the text.
        cx.update_window(handle.into(), |_, window, cx| {
            md.update(cx, |f, cx| f.pick_look(Look::Preview, window, cx));
            window.dispatch_action(Box::new(super::CycleLook), cx);
        })
        .unwrap();
        md.update(cx, |f, _| assert_eq!(f.preview.look, Look::Code));
        md.update(cx, |f, _| assert!(f.preview.drawn.is_none(), "nothing kept for the text alone"));
        let saved = task.update(cx, |t, cx| t.session(cx));
        assert!(saved.tabs.iter().any(|tab| tab.path.as_deref() == Some("a.md") && tab.look.is_none()));
        frame(cx, &|window| assert!(window.try_find("file-preview").is_none_or(|p| !p.visible())));

        // Markdown beside its text follows it as it scrolls: the line at its top, at the drawing's top.
        let long = open(cx, "long.md", Look::Split);
        frame(cx, &|_| {});
        cx.update_window(handle.into(), |_, window, cx| {
            window.scroll("file-editor", gpui_kit::ScrollDelta::Lines(point(0., -60.)), cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        frame(cx, &|_| {});
        let (top, block) = long.update(cx, |f, cx| {
            let top = f.preview.followed.expect("followed");
            let (state, _) = f.preview.markdown.clone().expect("Markdown's view");
            (top, state.read(cx).list_state().logical_scroll_top().item_ix)
        });
        // Four lines a part, two blocks a part (its heading, its text).
        assert!(top > 20, "the text scrolled: {top}");
        assert!(block.abs_diff(top / 2) <= 1, "the drawing at the text's line {top}: block {block}");

        // And the other way: the drawing scrolled by hand, the text follows it.
        cx.update_window(handle.into(), |_, window, cx| {
            window.scroll("file-preview", gpui_kit::ScrollDelta::Lines(point(0., -40.)), cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        frame(cx, &|_| {});
        let (block, line) = long.update(cx, |f, cx| {
            let (state, _) = f.preview.markdown.clone().expect("Markdown's view");
            let editor = f.editor.read(cx);
            let line = (-editor.scroll_offset().y / editor.line_height().unwrap()).round() as usize;
            (state.read(cx).list_state().logical_scroll_top().item_ix, line)
        });
        assert!(block > top / 2 + 3, "the drawing scrolled on: block {block}");
        assert!(line.abs_diff(block * 2) <= 2, "the text at the drawing's block {block}: line {line}");

        // By its scrollbar, which sends no wheel: the text follows too.
        long.update(cx, |f, cx| {
            let (state, _) = f.preview.markdown.clone().expect("Markdown's view");
            state.read(cx).list_state().scroll_to(gpui_kit::ListOffset { item_ix: 10, offset_in_item: px(0.) });
            cx.notify();
        });
        for _ in 0..3 {
            frame(cx, &|_| {});
            cx.run_until_parked();
        }
        let line = long.update(cx, |f, cx| {
            let editor = f.editor.read(cx);
            (-editor.scroll_offset().y / editor.line_height().unwrap()).round() as usize
        });
        assert!(line.abs_diff(20) <= 2, "the text at the drawing's block 10: line {line}");

        // The look chosen is what the next file of the kind opens in, and is not what a restored tab has.
        std::fs::write(repo.join("next.md"), "# next\n").unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            long.update(cx, |f, cx| f.pick_look(Look::Split, window, cx));
            long.update(cx, |f, cx| f.pick_look(Look::Preview, window, cx));
            task.update(cx, |t, cx| t.open_file(repo.join("next.md"), 0, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        let next = task.update(cx, |t, cx| t.tabs.iter().find_map(|tab| match &tab.kind {
            TabKind::File(f) if f.read(cx).path.ends_with("next.md") => Some(f.read(cx).preview.look),
            _ => None,
        }));
        assert_eq!(next, Some(Look::Preview));
        crate::state::set_settings(crate::state::Settings { file_looks: Default::default(), ..crate::state::settings() });

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
        // To a file: its header comes to the top of the view.
        cx.update(|_, cx| state.update(cx, |state, cx| state.scroll_to_file("b.txt", cx)));
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

/// A tab's ×, a middle click on it and its Close: every tab but the agent's
/// (a terminal or a chat).
fn closable(tab: &TabItem) -> bool {
    !matches!(tab.agent, Some(Who::Main))
}

/// Close others, to the right and all close without asking: files, pictures,
/// pages and the views that come back with a key (Changes, History). Pinned
/// tabs, files with unsaved edits and terminals stay.
fn closed_in_bulk(tab: &TabItem, cx: &App) -> bool {
    !tab.pinned
        && match &tab.kind {
            TabKind::File(f) => !f.read(cx).dirty && f.read(cx).saving.is_none() && !f.read(cx).sql_busy() && !f.read(cx).sql_holds(cx),
            TabKind::Binary(_) | TabKind::Browser(_) | TabKind::Diff(_) | TabKind::Changes | TabKind::History => true,
            TabKind::Terminal(_) | TabKind::Chat(_) | TabKind::Asleep => false,
        }
}
