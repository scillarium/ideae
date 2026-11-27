//! The window: projects and their tasks in the sidebar, the task in view, and
//! the state saved between runs.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui_kit::component::{
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    notification::Notification,
    h_flex, v_flex,
};
use gpui_kit::*;

use crate::{
    agent, git, naming,
    state::{self, ProjectState, State, TaskSession, TaskState},
    task::{SidePanelWidth, TaskEvent, TaskView},
    theme::{self, Appearance},
};

#[derive(Default)]
pub struct TaskEntry {
    pub path: PathBuf,
    pub branch: String,
    pub title: String,
    /// The project's own worktree: one per project, never deleted.
    pub main: bool,
    pub starred: bool,
    pub last_active: u64,
    /// The tabs to open when the task starts again.
    session: TaskSession,
    /// Running: its terminals, agent and editors. Kept while it is out of
    /// view, so coming back finds everything as it was.
    pub view: Option<Entity<TaskView>>,
    /// A git operation on it is running (creating, deleting).
    pub busy: Option<SharedString>,
    /// When it was last in view, this run; its language servers sleep after
    /// a while out of view.
    pub left_view: Option<std::time::Instant>,
    /// Its pull request, and when `gh` was last asked.
    pub pr: Option<crate::pr::Pr>,
    pub pr_checked: Option<std::time::Instant>,
    /// Ports of `.ideae.toml` services: base + 100 × this; 0 for main.
    pub port_slot: u32,
    /// Locked worktree (`git worktree lock`) whose locker is gone.
    pub locked: Option<String>,
    /// One of several tries at the same task (variants.rs): the group
    /// they share (empty for a task on its own), its number there, and
    /// what its agent alone was told on top of the prompt.
    pub group: String,
    pub variant: u32,
    pub note: String,
    /// What the agent was told to start with, and when: Unix seconds.
    pub prompt: String,
    pub created: u64,
    /// When its agent's latest turn ended. Not `last_active`, which a
    /// look at the task moves too.
    pub done_at: u64,
    /// When it was last looked at: in view in the active window. What its
    /// agent reported before stays out of Needs you after a restart.
    pub seen_at: u64,
    /// The commit a variant was made from: what its diff is counted against.
    pub base: String,
    /// Where its worktree is when not on its branch, which stays the task's
    /// (`branch`): read from git, not saved.
    pub head: Option<git::Head>,
    /// A worktree an agent made for itself (side.rs), not a task of the user's.
    pub side: Option<side::Side>,
    /// The agent chosen when it was made, by profile id; empty for its
    /// project's or the default.
    pub agent: String,
    /// A chat agent's choices for this task alone (a variant's model), by
    /// the agent's option ids.
    pub choices: Vec<(String, String)>,
    /// Its agents' sessions; the last is the one it runs.
    pub sessions: Vec<state::AgentSession>,
    /// Questions asked aside of its agent, open and closed, and how many
    /// it was ever asked (questions.rs).
    pub questions: Vec<state::QuestionState>,
    pub asked: u32,
    /// Started by the agent of this task (crew.rs).
    pub spawned_by: Option<PathBuf>,
    /// Made this run and not started yet, or its latest session recorded
    /// and not run: its agent starts a new session.
    pub fresh: bool,
    /// The archive it was restored from, and whether that one keeps work
    /// it did not get back (archive.rs).
    pub archive: String,
    pub archive_keep: bool,
    /// A deferred task come back on its own: when it is first opened, its
    /// project's setup runs and its agent is told this (empty: nothing).
    pub woke: Option<String>,
    /// Its tickets, request and addresses (state::TaskLink), and whether a
    /// task of before them had them read out of its prompt and branch.
    pub links: Vec<state::TaskLink>,
    pub links_read: bool,
    /// On a colleague's request, not one of yours: to review or go on with.
    pub review: bool,
}

impl TaskEntry {
    /// A task as it was saved (or as the store has it): not running.
    fn of_saved(t: &state::TaskState) -> Self {
        TaskEntry {
                path: t.path.clone(),
                branch: t.branch.clone(),
                title: t.title.clone(),
                main: t.main,
                starred: t.starred,
                last_active: t.last_active,
                session: t.session.clone(),
                view: None,
                busy: None,
                locked: None,
                left_view: None,
                pr: None,
                pr_checked: None,
                port_slot: t.port_slot,
                group: t.group.clone(),
                variant: t.variant,
                note: t.note.clone(),
                prompt: t.prompt.clone(),
                created: t.created,
                done_at: t.done_at,
                seen_at: t.seen_at,
                base: t.base.clone(),
                head: None,
                side: None,
                agent: t.agent.clone(),
                choices: t.choices.clone(),
                sessions: t.sessions.clone(),
                fresh: t.fresh,
                archive: t.archive.clone(),
                archive_keep: t.archive_keep,
                woke: t.woke.clone(),
                questions: t.questions.clone(),
                asked: t.asked,
                spawned_by: t.spawned_by.clone(),
                links: t.links.clone(),
                links_read: t.links_read,
                review: t.review,
            }
    }

    /// What a saved task (another window's, by way of the store) has of the
    /// machine's, over this one's; its tabs and what runs stay.
    fn take_saved(&mut self, t: &state::TaskState) {
        let mut again = Self::of_saved(t);
        let mut session = std::mem::take(&mut self.session);
        session.review = t.session.review.clone();
        session.viewed = t.session.viewed.clone();
        session.bookmarks = t.session.bookmarks.clone();
        again.session = session;
        again.view = self.view.take();
        again.busy = self.busy.take();
        again.left_view = self.left_view.take();
        again.pr = self.pr.take();
        again.pr_checked = self.pr_checked.take();
        again.locked = self.locked.take();
        again.head = self.head.take();
        again.side = self.side.take();
        *self = again;
    }

    /// Its agent, terminals and servers stopped (Pause), or not started in
    /// this run: no view, nothing under way.
    pub fn paused(&self) -> bool {
        self.view.is_none() && self.busy.is_none()
    }

    /// Its request as a link: added, or brought up to date.
    pub fn set_pr_link(&mut self, pr: &crate::pr::Pr) {
        let link = state::TaskLink { what: "pr".into(), key: format!("#{}", pr.number), url: pr.url.clone(), ..Default::default() };
        match self.links.iter_mut().find(|l| l.what == "pr") {
            Some(known) => *known = link,
            None => self.links.push(link),
        }
    }

    /// Its own ticket: the first.
    pub fn ticket(&self) -> Option<&state::TaskLink> {
        self.links.iter().find(|l| l.is_ticket())
    }
}

pub mod agents_ui;
pub mod archive_ui;
mod elsewhere;
mod hosts_ui;
pub(crate) use hosts_ui::HostState;
mod merged_ui;
pub mod automation;
pub mod layered;
mod links_ui;
mod review_ui;
mod tickets_ui;
pub mod present;
pub mod questions;
mod crew;
pub mod side;
pub mod spaces;
pub mod variants;
pub mod welcome;

impl TaskEntry {
    /// Made by the app (or restored) on a branch of its own, which stays its
    /// whatever its worktree is put on; one made elsewhere follows its worktree.
    pub fn owns_branch(&self) -> bool {
        !self.main && self.created > 0 && !self.branch.is_empty() && !self.branch.starts_with('(')
    }

    /// The agent's latest session, to find its file by. Unknown, it is
    /// the latest in the folder, unless questions asked there made others.
    pub fn session_ref(&self) -> Option<crate::agents::SessionRef> {
        let session = self.sessions.last().filter(|s| s.id.is_some() || self.asked == 0)?;
        // Of the machine it runs on.
        let profile = crate::agents::profiles_for(&self.path).0.into_iter().find(|p| p.id == session.profile)?;
        Some(crate::agents::SessionRef { profile, id: session.id.clone() })
    }

    pub fn label(&self) -> String {
        match (&self.title, self.side.as_ref().and_then(|s| s.about.as_ref())) {
            (title, _) if !title.is_empty() => title.clone(),
            // A subagent's branch is named after its id: what it was asked says more.
            (_, Some(about)) => about.clone(),
            _ => self.branch.clone(),
        }
    }
}

pub struct Project {
    pub path: PathBuf,
    pub name: String,
    /// The space it is sorted into, by id; none, and it is in every one.
    pub space: Option<String>,
    pub collapsed: bool,
    pub tasks: Vec<TaskEntry>,
    /// The command last run in every variant of a task: tests, a benchmark;
    /// and whether it runs by itself when the variants have finished.
    pub check: String,
    pub check_auto: bool,
    /// Whether the commands of `.ideae.toml` may run, by their trust key;
    /// and the hints not to show again. The settings kept for yourself are
    /// in project_config, read from any thread.
    pub trusted: std::collections::BTreeMap<String, bool>,
    pub hints_off: std::collections::BTreeSet<String>,
    /// Its own layer of the settings, and its team's: the `.ideae.toml` of
    /// its main checkout, as last read (`team_read`, the text's hash).
    pub settings: crate::layers::Overrides,
    pub team: crate::layers::Overrides,
    pub team_read: Option<u64>,
    /// Whether its recent commits have tickets' keys in their subjects: a
    /// task's commit box then starts with its ticket's key. Read once a run.
    pub keys_in_history: Option<bool>,
    /// What applies, all layers put together: worked out when one changes
    /// (`Workspace::resolve_projects`), never while drawing.
    pub resolved: std::sync::Arc<crate::layers::Resolved>,
    /// `resolved` was worked out at least once: a project added since the
    /// last time is not, and has only the defaults until it is.
    pub resolved_once: bool,
    /// Branches of origin whose PR was merged, and when they were looked for.
    pub merged_branches: Vec<crate::merged_branches::MergedBranch>,
    pub merged_checked: Option<std::time::Instant>,
    /// Worktrees its tasks' agents made where nothing else tells it, and
    /// the task of each ([`side`]).
    pub made_by_agents: std::collections::BTreeMap<PathBuf, PathBuf>,
}

#[cfg(all(test, unix))]
impl Project {
    /// Its tasks but the main thread, which every project has.
    pub fn work(&self) -> Vec<&TaskEntry> {
        self.tasks.iter().filter(|t| !t.main).collect()
    }
}

impl Project {
    /// What applies to it of the layered settings.
    pub fn values(&self) -> &crate::layers::Values {
        &self.resolved.values
    }

    fn new(path: PathBuf) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        Self {
            path,
            name,
            space: None,
            collapsed: false,
            tasks: Vec::new(),
            check: String::new(),
            check_auto: false,
            trusted: Default::default(),
            hints_off: Default::default(),
            settings: Default::default(),
            team: Default::default(),
            team_read: None,
            keys_in_history: None,
            resolved: Default::default(),
            resolved_once: false,
            merged_branches: Vec::new(),
            merged_checked: None,
            made_by_agents: Default::default(),
        }
    }
}

pub struct Workspace {
    pub projects: Vec<Project>,
    /// The task in view, by worktree path.
    pub active: Option<PathBuf>,
    pub active_collapsed: bool,
    pub starred_collapsed: bool,
    /// Set while the window is shown to others: what is left in it.
    pub presenting: Option<present::Presenting>,
    /// The spaces, the one in the window (none: all of them), and the
    /// task last looked at in each.
    pub spaces: Vec<spaces::Space>,
    pub space: Option<String>,
    last_in_space: std::collections::HashMap<Option<String>, PathBuf>,
    /// The agent given on the command line, over Settings.
    agent_override: Option<Option<String>>,
    hook_socket: Option<String>,
    /// Kept alive for the app's lifetime: dropping it removes the socket.
    _bridge: Option<agent::Bridge>,
    pub appearance: Appearance,
    pub sidebar_width: Pixels,
    /// The sidebar drawn as a narrow rail, dragged that narrow; and whether
    /// the whole of it is open over the task for the pointer on it.
    pub sidebar_rail: bool,
    pub sidebar_peek: bool,
    pub(crate) peek_wait: Task<()>,
    /// Whether the pointer is on the rail or the sidebar open from it, as last told.
    pub(crate) peek_hovered: bool,
    /// When the pointer last moved on the rail: the sidebar opens once it rests.
    pub(crate) peek_moved: std::time::Instant,
    /// Told that the battery runs low, this time it runs down.
    battery_told: bool,
    awake: crate::awake::Awake,
    /// When the window last lost the keyboard; None while it has it.
    window_left: Option<std::time::Instant>,
    /// In the GNOME dash; asked once, not while the menu opens.
    /// How the app stands with the desktop, and whether it was told once that it is not listed there.
    pub desk: crate::desktop::Standing,
    told_unlisted: bool,
    pub sysmon: Entity<crate::sysmon::SysMon>,
    /// Filters the sidebar's tasks.
    pub search: Entity<InputState>,
    /// The state as last written, to write only changes.
    /// The state as last written, and the number of the save that wrote it:
    /// a write that comes after a later one is dropped.
    last_saved: std::sync::Arc<std::sync::Mutex<(u64, String)>>,
    save_seq: u64,
    /// Tasks open in the last run and not loaded yet, the next one last.
    restoring: Vec<PathBuf>,
    /// Tasks open when the app last stopped and not loaded yet, the one in view too.
    last_run: std::collections::HashSet<PathBuf>,
    /// Started for a question alone: its own agent waits until it is opened.
    quiet_start: std::collections::HashSet<PathBuf>,
    /// Agents tasks' agents started (crew.rs).
    crew: crew::Crew,
    /// The machine's store this window opened (crate::machine): its saves go to that one alone.
    store: u64,
    /// Projects of each connected machine this window does not show (its
    /// sidebar offers them: "box: 3 more projects"), by host: path and name.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub more_elsewhere: std::collections::BTreeMap<String, Vec<(PathBuf, String)>>,
    /// The next task made sends its prompt: the dialog filled it, nobody
    /// has anything to add.
    send_next_prompt: bool,
    /// Tasks named by a ticket's key alone, named once from their agent.
    naming_tasks: std::collections::HashSet<PathBuf>,
    /// Conversations copied for a question to start from, until it has.
    fork_copies: std::collections::HashMap<(PathBuf, u32), Vec<PathBuf>>,
    /// What questions' sessions were last read at, for how full they are.
    question_usage: std::collections::HashMap<(PathBuf, u32), Option<std::time::SystemTime>>,
    /// An answer read from its row: the pointer stayed on it.
    answer_hover: Task<()>,
    _subscriptions: Vec<Subscription>,
    _tick: Task<()>,
    pub build: crate::build_watch::BuildWatch,
    /// Clicked notifications send their task here.
    /// A clicked notification: its task, and the question it was of.
    open_task_tx: async_channel::Sender<(PathBuf, Option<u32>)>,
    /// Where the window is, to open there next time.
    window_place: Option<state::WindowPlace>,
    /// Where the window was asked to open, and when: some window managers
    /// place it a fixed offset away (frame or shadow insets), which would
    /// otherwise add up with every restart.
    requested_origin: Option<(Point<Pixels>, std::time::Instant)>,
    origin_offset: Point<Pixels>,
    /// A newer release, when this is a release build; and whether it is
    /// being installed.
    pub update: Option<crate::update::Release>,
    pub updating: bool,
    /// A build of a checkout: the commits of its main branch it lacks, and
    /// whether they are being pulled and built.
    pub behind: Option<crate::update::Behind>,
    pub rebuilding: bool,
    /// Quitting while the agents go on: a restart, or the window's ×.
    keep_terminals: bool,
    /// A restart under way: a second click starts no second instance.
    pub restarting: bool,
    /// What each task changed since its fork, by worktree path: tasks of
    /// a project that touch the same files are marked before they clash.
    pub touched: std::collections::HashMap<PathBuf, std::collections::BTreeSet<String>>,
    /// Of those, the ones two tasks of a project both changed, by task.
    shared: std::collections::HashMap<PathBuf, Vec<(PathBuf, Vec<String>)>>,
    /// When each project's base branch was last fetched in the background.
    fetched: std::collections::HashMap<PathBuf, std::time::Instant>,
    /// Integrations whose reading went wrong this run: told once, and left
    /// alone a while when the tracker asked to slow down.
    pub(crate) ticket_trouble: std::collections::HashMap<String, (crate::integrations::IntError, std::time::Instant)>,
    /// The group of variants whose screen is in view, in place of a task.
    pub active_group: Option<String>,
    /// On the group's screen, the variant the menu bar's task items act on: the one clicked last.
    pub group_card: Option<PathBuf>,
    /// Groups folded in the sidebar.
    pub collapsed_groups: std::collections::HashSet<String>,
    /// The agents' worktrees shown under a task (or a project's path, for
    /// those in temporary places): folded until opened.
    pub open_sides: std::collections::HashSet<PathBuf>,
    /// The projects whose Paused line is open, by their path.
    pub open_paused: std::collections::HashSet<PathBuf>,
    /// The task whose menu a right click opened: its tooltip would cover
    /// the menu, so it is not shown till the pointer leaves the row.
    pub menu_on: Option<PathBuf>,
    /// What the group's screen shows of each variant, by worktree path.
    pub variant_info: std::collections::HashMap<PathBuf, variants::VariantInfo>,
    /// What each task's agent has used, by worktree, with when the
    /// transcript it is read from was last written to.
    pub usage: std::collections::HashMap<PathBuf, (Option<std::time::SystemTime>, crate::usage::Usage)>,
    reading_usage: bool,
    /// Sessions already told of as over the limit set.
    usage_warned: std::collections::HashSet<PathBuf>,
    /// Variants whose agent's answer is shown whole on the group's screen.
    whole_answers: std::collections::HashSet<PathBuf>,
    /// The command run in every variant of the group in view, how it went
    /// in each, and what runs it.
    pub check_command: Entity<InputState>,
    checks: std::collections::HashMap<PathBuf, variants::Run>,
    check: Option<variants::Check>,
    /// Groups whose check waits for the one that runs: (group, command,
    /// whether the judge follows).
    check_queue: Vec<(String, String, bool)>,
    /// What the judge said of each group; the judges that run; the groups
    /// it was asked about by itself already; and those that changed since
    /// it spoke.
    judges: std::collections::HashMap<String, variants::Run>,
    judging: std::collections::HashMap<String, (variants::RunSlot, Task<()>)>,
    judged: std::collections::HashSet<String>,
    stale_verdicts: std::collections::HashSet<String>,
    read_variants_again: bool,
    /// A picture of each variant's site, taken when the judge looked: by
    /// worktree path.
    shots: std::collections::HashMap<PathBuf, PathBuf>,
    reading_variants: bool,
    /// What to tell every agent of the group in view.
    pub group_message: Entity<InputState>,
    /// Setups, teardowns, checks and what they wait for.
    auto: automation::AutoState,
    /// Where tasks are archived (archive.rs).
    pub archive_dir: PathBuf,
    /// Deferred tasks coming back now, and those that could not: told once.
    waking: std::collections::HashSet<PathBuf>,
    wake_failed: std::collections::HashSet<PathBuf>,
    /// Tasks of hosts being connected, by host ([`hosts_ui`]), and the one
    /// to show when its host is.
    host_waiting: std::collections::BTreeMap<String, Vec<PathBuf>>,
    /// What waits for a host to be connected to be done ([`hosts_ui`]).
    host_then: std::collections::BTreeMap<String, Vec<hosts_ui::Then>>,
    /// Hosts the last try to connect failed, and why (in words): told by
    /// their projects until tried again.
    host_failed: std::collections::BTreeMap<String, String>,
    /// Settings asked for and being opened (the language servers looked for first): asked again, nothing more.
    pub(crate) settings_opening: bool,
    host_shown: Option<PathBuf>,
    /// Where agents' reports go, from this machine's daemon and others'.
    #[cfg_attr(not(unix), allow(dead_code))]
    hook_tx: async_channel::Sender<agent::HookEvent>,
}

impl Workspace {
    #[cfg(test)]
    pub fn new(
        repos: Vec<PathBuf>,
        agent: Option<Option<String>>,
        appearance: Option<Appearance>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_state(state::load(), repos, agent, appearance, window, cx)
    }

    /// As `new`, with the state already read.
    pub fn with_state(
        mut saved: State,
        repos: Vec<PathBuf>,
        agent: Option<Option<String>>,
        appearance: Option<Appearance>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        saved.normalize_layers();
        // Keys an older version kept in the state go to the sealed store.
        let mut settings = saved.settings.clone();
        if settings.agents.iter_mut().fold(false, |any, p| p.seal_secrets() || any) {
            state::set_settings(settings);
        } else {
            state::init_settings(&saved.settings);
        }
        cx.background_spawn(async { crate::lsp::look_for_servers() }).detach();
        let appearance = appearance
            .or_else(|| saved.appearance.as_deref().and_then(Appearance::parse))
            .unwrap_or(Appearance::Auto);
        theme::choose(&saved.settings.theme, saved.settings.dark_theme(), cx);
        theme::set_code_font(&saved.settings.code_font, cx);
        theme::set_ligatures(saved.settings.ligatures, cx);
        theme::apply(appearance, window, cx);
        crate::lsp::POWER_SAVING.store(saved.power_saving, std::sync::atomic::Ordering::Relaxed);
        crate::learn::restore(saved.learn_counts.clone());
        // A global: tests running alongside would flip it under edit.rs's test of it.
        if !cfg!(test) {
            gpui_kit::base::input::HUMP_WORDS.store(saved.settings.hump_words, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(width) = saved.side_panel_width {
            cx.set_global(SidePanelWidth(px(width)));
        }
        #[cfg(target_os = "macos")]
        crate::mac_notify::of_the_window();
        // `IDEAE_NOTIFY="title\nbody"`: a notification at the start, for a run nobody can make one in.
        if let Ok(text) = std::env::var("IDEAE_NOTIFY") {
            let (title, body) = text.split_once('\n').unwrap_or((text.as_str(), ""));
            #[cfg(target_os = "macos")]
            eprintln!("notifications: bundle {:?}, as the app {}", crate::mac_notify::bundle(), crate::mac_notify::as_the_app());
            crate::desktop::notify_with(title, body, || eprintln!("notifications: clicked"));
        }
        // A task's Chrome can be started by its agent before any web tab is drawn.
        crate::browser::note_screen(window.scale_factor());
        let side = crate::task::SideLayout::default();
        cx.set_global(crate::task::SideLayout { below: saved.side_lower.unwrap_or(side.below), split: saved.side_split.map(px) });
        crate::zoom::init(saved.zoom, cx);

        let (hook_tx, hook_rx) = async_channel::unbounded::<agent::HookEvent>();
        let bridge_tx = hook_tx.clone();
        let bridge = agent::Bridge::start(move |event| {
            let _ = bridge_tx.send_blocking(event);
        })
        .inspect_err(|e| eprintln!("agent hooks off: {e}"))
        .ok();
        let agent_override = agent;
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut hook_socket = bridge.as_ref().map(|b| b.socket.clone());
        // Terminals run in the daemon, which also takes the hooks. Without
        // one (Windows, or it would not start) they run in the window.
        #[cfg(unix)]
        if std::env::var_os("IDEAE_NO_DAEMON").is_none() {
            let live = hook_tx.clone();
            let to_event = |h: crate::daemon::proto::Hook, replayed: bool| agent::HookEvent {
                replayed: replayed.then_some(h.at),
                task: h.task.into(),
                agent: h.agent,
                event: h.event,
                message: h.message,
                kind: h.kind,
                file: h.file,
                said: h.said,
                session: h.session,
                guessed: false,
                worktree: h.worktree,
                call: h.call,
            };
            match crate::daemon::client::connect(move |h| {
                let _ = live.send_blocking(to_event(h, false));
            }) {
                Some((_, earlier)) => {
                    hook_socket = Some(crate::daemon::proto::socket_path().display().to_string());
                    for h in earlier {
                        let _ = hook_tx.send_blocking(to_event(h, true));
                    }
                }
                None => eprintln!("daemon unavailable: terminals run in the window"),
            }
        }
        // This machine's projects and tasks are its store's: its daemon's, or this window's where there is none.
        let store = crate::machine::open(&mut saved);
        Self::follow_store(window, cx);
        // What ssh asks (a password, a passphrase) for a project on another machine.
        #[cfg(unix)]
        {
            let (ask_tx, ask_rx) = async_channel::unbounded::<crate::askpass::Ask>();
            if crate::askpass::socket().is_none() && !cfg!(test) {
                let _ = crate::askpass::listen(move |ask| {
                    let _ = ask_tx.send_blocking(ask);
                })
                .inspect_err(|e| eprintln!("ssh's questions not asked here: {e}"));
            }
            cx.spawn_in(window, async move |this, cx| {
                while let Ok(ask) = ask_rx.recv().await {
                    if this.update_in(cx, |this, window, cx| this.ask_for_ssh(ask, window, cx)).is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        // Chat agents' questions through ideae's MCP server, each about its
        // own worktree. Tests leave the socket to the app, but one that asks through it.
        let (mcp_tx, mcp_rx) = async_channel::unbounded::<(crate::mcp::Request, async_channel::Sender<crate::mcp::Answer>)>();
        if !cfg!(test) || std::env::var_os("IDEAE_TEST_MCP").is_some() {
            let _ = crate::mcp::listen(move |request, answer| {
                let _ = mcp_tx.send_blocking((request, answer));
            })
            .inspect_err(|e| eprintln!("ideae's MCP server off: {e}"));
        }
        cx.spawn_in(window, async move |this, cx| {
            while let Ok((request, answer)) = mcp_rx.recv().await {
                if this.update_in(cx, |this, window, cx| this.on_mcp(request, answer, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Reports wait until the tasks of the last run are back: one for a
        // task not loaded yet would be lost.
        let (restored_tx, restored_rx) = async_channel::bounded::<()>(1);
        cx.spawn_in(window, async move |this, cx| {
            let _ = restored_rx.recv().await;
            while let Ok(event) = hook_rx.recv().await {
                if this.update_in(cx, |this, window, cx| this.on_hook(event, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let subscriptions = vec![
            cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
                if this.appearance == Appearance::Auto {
                    theme::apply(Appearance::Auto, window, cx);
                }
            }),
            // Leaving the window saves edited files, as JetBrains IDEs do.
            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                this.looked_at_in_view(window, cx);
                if !window.is_window_active() {
                    this.window_left.get_or_insert_with(std::time::Instant::now);
                    for view in this.views() {
                        view.update(cx, |t, cx| t.autosave_all(cx));
                    }
                } else {
                    this.window_left = None;
                    // Back at the window: the task in view has its servers again.
                    if let Some(view) = this.active_view() {
                        view.update(cx, |t, cx| t.wake_language_servers(cx));
                    }
                }
            }),
            // Every theme change, from the switcher or the OS, recolors the terminals.
            cx.observe_global::<gpui_kit::component::Theme>(|this: &mut Self, cx| {
                let colors = theme::terminal_colors(cx);
                for view in this.views() {
                    view.update(cx, |t, cx| t.set_terminal_colors(&colors, cx));
                }
            }),
            // Terminals end their whole session (the agent and whatever it
            // started in the background); nothing of a task outlives the app.
            // A restart leaves the daemon's terminals running for the next
            // window; any other quit ends them with everything else.
            cx.on_app_quit(|this: &mut Self, cx| {
                this.save(cx);
                // Its changes to other machines' projects there before their links close.
                crate::machine::flush_remote(std::time::Duration::from_secs(3));
                if this.keep_terminals {
                    for view in this.views() {
                        view.update(cx, |t, cx| t.save_all_now(cx));
                    }
                    #[cfg(unix)]
                    for daemon in crate::daemon::client::get().into_iter().chain(crate::daemon::client::remotes()) {
                        daemon.leaving();
                    }
                } else {
                    let seen = this.hook_socket.as_ref().map(|socket| crate::procs::task_processes(None, socket)).unwrap_or_default();
                    for view in this.views() {
                        view.update(cx, |t, cx| t.shut_down_at_quit(cx));
                    }
                    this.teardown_at_quit();
                    #[cfg(unix)]
                    if let Some(daemon) = crate::daemon::client::get() {
                        daemon.shutdown();
                    }
                    // Exit ends the agents of other machines too, and ssh's links to
                    // them; the window's × leaves them.
                    // Each host at once (each waits a moment at most), what lies beyond a
                    // first host before any link to one is closed.
                    #[cfg(unix)]
                    {
                        let remotes = crate::daemon::client::remotes();
                        let ending: Vec<_> = remotes
                            .iter()
                            .cloned()
                            .map(|daemon| {
                                std::thread::spawn(move || {
                                    daemon.shutdown();
                                    daemon.close_beyond();
                                })
                            })
                            .collect();
                        for done in ending {
                            let _ = done.join();
                        }
                        for daemon in remotes {
                            daemon.close_master();
                        }
                    }
                    // Here, not on the tasks' threads: the app exits next.
                    if let Some(socket) = this.hook_socket.clone() {
                        crate::procs::end_task_strays(None, &socket, seen);
                    }
                }
                crate::procs::end_child_sessions();
                crate::browser::quit();
                crate::predict::quit();
                crate::db::quit();
                crate::procs::end_if_stuck();
                async {}
            }),
        ];

        cx.observe_window_bounds(window, |this: &mut Self, window, _| {
            let (bounds, maximized, fullscreen) = match window.window_bounds() {
                WindowBounds::Windowed(b) => (b, false, false),
                WindowBounds::Maximized(b) => (b, true, false),
                WindowBounds::Fullscreen(b) => (b, false, true),
            };
            if let Some((asked, at)) = this.requested_origin {
                let offset = bounds.origin - asked;
                let small = offset.x.abs() < px(150.) && offset.y.abs() < px(150.);
                if at.elapsed() < std::time::Duration::from_secs(3) && !maximized && small {
                    this.origin_offset = offset;
                }
            }
            let origin = bounds.origin - this.origin_offset;
            this.window_place = Some(state::WindowPlace {
                x: f32::from(origin.x),
                y: f32::from(origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
                maximized,
                fullscreen,
            });
        })
        .detach();

        // A clicked notification brings the window up on its task.
        let (open_task_tx, open_task_rx) = async_channel::unbounded::<(PathBuf, Option<u32>)>();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok((path, question)) = open_task_rx.recv().await {
                if this.update_in(cx, |this, window, cx| {
                    window.activate_window();
                    match question {
                        Some(n) => this.show_question(&path, n, window, cx),
                        None => this.focus_task(&path, window, cx),
                    }
                }).is_err() {
                    break;
                }
            }
        })
        .detach();

        // A ticket asked for from outside (deep_link): its task, or a new one.
        let (asked_tx, asked_rx) = async_channel::unbounded::<String>();
        // Not in the tests: one process's windows would take each other's.
        if !cfg!(test) {
            crate::deep_link::connect(asked_tx);
            crate::deep_link::listen();
        }
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(line) = asked_rx.recv().await {
                if this.update_in(cx, |this, window, cx| this.open_asked(&line, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Closing the window the window manager's way is the same as ×.
        let closing = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            // Open transactions end with the window: asked first, the window stays meanwhile.
            let asked = closing.update(cx, |w, cx| w.ask_about_sql(window, cx, |w, _, cx| w.close_window(cx))).unwrap_or(false);
            if !asked {
                let _ = closing.update(cx, |w, cx| w.close_window(cx));
            }
            !asked
        });

        // Saves now and then, and picks up worktrees made outside the app.
        let tick = cx.spawn_in(window, async move |this, cx| {
            let mut n = 0u32;
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                n += 1;
                let alive = this.update_in(cx, |this, window, cx| {
                    this.save_later(cx);
                    this.check_build(window, cx);
                    // While an agent of the group in view works; a turn's end reads it too.
                    if this.active_group.clone().is_some_and(|group| !this.group_settled(&group, cx)) {
                        this.refresh_variant_info(window, cx);
                    }
                    if n == 1 {
                        this.welcome_agents(window, cx);
                        this.offer_agent_hooks(window, cx);
                    }
                    // Every six hours, the first time soon after start; a
                    // checkout's fetch is cheap, so every hour.
                    if n % 7200 == 3 || (n % 1200 == 3 && crate::update::source().is_some()) {
                        this.check_update(false, window, cx);
                    }
                    // Saving power, what only reads the local repositories
                    // runs every five minutes, not every half: the list
                    // of tasks still follows worktrees made elsewhere.
                    let saving = crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed);
                    let often = if saving { 100 } else { 10 };
                    if n % often == 0 {
                        this.sync_worktrees(window, cx);
                        // Tasks come and go with their worktrees.
                        this.tell_what_is_hidden();
                    }
                    // Every two minutes: a battery runs down slowly.
                    if n % 40 == 5 {
                        this.look_at_battery(window, cx);
                    }
                    this.keep_awake(cx);
                    if n % 2 == 0 {
                        this.restart_lost_language_servers(cx);
                    }
                    if n % 10 == 0 {
                        this.sleep_idle_language_servers(cx);
                        this.tend_questions(window, cx);
                        this.refresh_prs(false, window, cx);
                        this.refresh_merged_branches(window, cx);
                    }
                    if n % often == 1 {
                        this.refresh_touched(false, window, cx);
                    }
                    if n % often == 4 {
                        this.refresh_usage(window, cx);
                    }
                    if n % 5 == 2 {
                        this.fetch_bases(cx);
                    }
                    // The team's settings: a minute after its file changed, five for a project elsewhere.
                    if n % 20 == 11 {
                        this.read_team_layers_of(n % 100 == 11, cx);
                        this.read_key_history(window, cx);
                    }
                    // Once a day, the first soon after start.
                    if n % 28_800 == 5 {
                        this.clean_up_archives(cx);
                    }
                    // Hourly, the first soon after start: deferred tasks whose day came.
                    if n % 1_200 == 7 {
                        this.wake_deferred(window, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        let mut this = Self {
            projects: Vec::new(),
            active: None,
            active_collapsed: saved.active_collapsed,
            starred_collapsed: saved.starred_collapsed,
            presenting: saved.presenting.as_ref().map(|p| present::Presenting::new(p.kept.iter().cloned().collect(), p.since)),
            spaces: saved.spaces.clone(),
            space: saved.space_id.clone(),
            last_in_space: Default::default(),
            agent_override,
            hook_socket,
            _bridge: bridge,
            appearance,
            sidebar_width: px(saved.sidebar_width.unwrap_or(260.)),
            sidebar_rail: saved.sidebar_rail,
            sidebar_peek: false,
            peek_wait: Task::ready(()),
            peek_hovered: false,
            peek_moved: std::time::Instant::now(),
            battery_told: false,
            awake: Default::default(),
            window_left: None,
            desk: Default::default(),
            told_unlisted: saved.told_unlisted,
            search: {
                let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search tasks…"));
                cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()).detach();
                search
            },
            group_message: {
                let message = cx.new(|cx| InputState::new(window, cx).placeholder("To every variant: \"also cover it with a test\""));
                cx.subscribe_in(&message, window, |this, message, event: &InputEvent, window, cx| {
                    let (InputEvent::PressEnter { .. }, Some(group)) = (event, this.active_group.clone()) else { return };
                    let text = message.read(cx).value().trim().to_string();
                    if !text.is_empty() {
                        message.update(cx, |input, cx| input.set_value("", window, cx));
                        this.message_group(&group, text, window, cx);
                    }
                })
                .detach();
                message
            },
            active_group: None,
            group_card: None,
            collapsed_groups: Default::default(),
            open_sides: Default::default(),
            open_paused: Default::default(),
            menu_on: None,
            variant_info: Default::default(),
            usage: Default::default(),
            reading_usage: false,
            usage_warned: Default::default(),
            whole_answers: Default::default(),
            check_command: {
                let command = cx.new(|cx| InputState::new(window, cx).placeholder("Run in every variant: cargo test, npm run bench…"));
                cx.subscribe_in(&command, window, |this, command, event: &InputEvent, window, cx| {
                    let (InputEvent::PressEnter { .. }, Some(group)) = (event, this.active_group.clone()) else { return };
                    let line = command.read(cx).value().trim().to_string();
                    if !line.is_empty() {
                        if let Some(&(pi, _)) = this.members(&group).first() {
                            this.projects[pi].check = line.clone();
                        }
                        this.run_in_group(&group, line, false, window, cx);
                    }
                })
                .detach();
                command
            },
            checks: Default::default(),
            check: None,
            check_queue: Vec::new(),
            judges: Default::default(),
            judging: Default::default(),
            judged: Default::default(),
            stale_verdicts: Default::default(),
            read_variants_again: false,
            shots: Default::default(),
            reading_variants: false,
            sysmon: {
                cx.new(crate::sysmon::SysMon::new)
            },
            last_saved: Default::default(),
            save_seq: 0,
            restoring: Vec::new(),
            last_run: Default::default(),
            quiet_start: Default::default(),
            crew: crew::Crew::from_records(&saved.crew),
            store,
            more_elsewhere: Default::default(),
            send_next_prompt: false,
            naming_tasks: Default::default(),
            fork_copies: Default::default(),
            question_usage: Default::default(),
            answer_hover: Task::ready(()),
            _subscriptions: subscriptions,
            _tick: tick,
            requested_origin: None,
            origin_offset: Point::default(),
            open_task_tx,
            window_place: saved.window.clone(),
            update: None,
            restarting: false,
            updating: false,
            behind: None,
            rebuilding: false,
            keep_terminals: false,
            touched: Default::default(),
            shared: Default::default(),
            fetched: Default::default(),
            ticket_trouble: Default::default(),
            build: crate::build_watch::BuildWatch::new(),
            auto: Default::default(),
            archive_dir: crate::archive::root(),
            waking: Default::default(),
            wake_failed: Default::default(),
            host_waiting: Default::default(),
            host_then: Default::default(),
            host_failed: Default::default(),
            settings_opening: false,
            host_shown: None,
            hook_tx: hook_tx.clone(),
        };
        this.restore(&saved, window, cx);
        this.resolve_projects(cx);
        // Tasks of before get their links once the teams' layers are read.
        this.read_team_layers(cx);
        this.read_key_history(window, cx);
        this.tell_what_is_hidden();
        // A presentation that was on is on again, with all that goes with it.
        if this.presenting.is_some() {
            this.apply_presenting(cx);
        }

        // Repositories named on the command line join the list; the first one
        // comes into view when nothing else is.
        let mut first_cli = None;
        let repos = if repos.is_empty() && this.projects.is_empty() {
            std::env::current_dir().ok().filter(|d| git::main_worktree(d).is_some()).into_iter().collect()
        } else {
            repos
        };
        for repo in repos {
            let added = match repo.to_str().filter(|r| r.starts_with("ssh://")) {
                Some(url) => this.add_remote_project(url),
                None => this.add_project(&repo),
            };
            if let Some(ix) = added {
                first_cli.get_or_insert(ix);
            }
        }
        if this.active.is_none() {
            if let Some(ix) = first_cli {
                this.open_main(ix, window, cx);
            }
        }
        this.sync_worktrees(window, cx);
        if crate::desktop::can_list() {
            cx.spawn_in(window, async move |this, cx| {
                let desk = cx.background_spawn(async { crate::desktop::standing() }).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.desk = desk;
                    this.tell_unlisted(window, cx);
                    cx.notify();
                });
            })
            .detach();
        }
        // What the start found of this machine's store, said once: once the window can show it.
        if let Some(note) = crate::machine::take_note() {
            cx.spawn_in(window, async move |_, cx| {
                let _ = cx.update(|window, cx| window.push_notification(Notification::warning(note).title("Your tasks"), cx));
            })
            .detach();
        }
        // A panic of the run before, said once.
        cx.spawn_in(window, async move |_, cx| {
            let Some((said, log)) = cx.background_spawn(async { crate::crash::take_last() }).await else { return };
            let _ = cx.update(|window, cx| {
                window.push_notification(Notification::error(format!("{said}\n{}", log.display())).title("ideae ended unexpectedly last time"), cx);
            });
        })
        .detach();
        // The other tasks of the last run come back one at a time, the
        // window drawn in between: it is there with the task in view.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                yield_now().await;
                let more = this.update_in(cx, |this, window, cx| {
                    let Some(path) = this.restoring.pop() else { return false };
                    this.load(&path, window, cx);
                    cx.notify();
                    true
                });
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
            // Sessions no task shows any more (closed while the window was gone).
            #[cfg(unix)]
            let _ = this.update(cx, |this, cx| {
                if let Some(daemon) = crate::daemon::client::get() {
                    let keep = this.views().iter().flat_map(|v| v.read(cx).sessions(cx)).collect();
                    daemon.prune(keep);
                }
            });
            drop(restored_tx);
        })
        .detach();

        // Debug aid for runs nobody types in: IDEAE_KEYS="wait:8 ctrl-n g r e"
        // presses those keys in the window, a pause where one is asked.
        if let Ok(keys) = std::env::var("IDEAE_KEYS") {
            // The parser keeps what it is given: once per run, it may.
            let keys: &'static str = keys.leak();
            cx.spawn_in(window, async move |_, cx| {
                for key in keys.split_whitespace() {
                    let pause = key.strip_prefix("wait:").and_then(|secs| secs.parse::<f32>().ok());
                    cx.background_executor().timer(std::time::Duration::from_secs_f32(pause.unwrap_or(0.08))).await;
                    if let (None, Ok(mut stroke)) = (pause, Keystroke::parse(key)) {
                        // A key alone is also the character it types.
                        if key.chars().count() == 1 {
                            stroke.key_char = Some(key.to_string());
                        } else if key == "space" {
                            stroke.key_char = Some(" ".to_string());
                        }
                        let handled = cx.update(|window, cx| window.dispatch_keystroke(stroke, cx));
                        if std::env::var_os("IDEAE_DEBUG").is_some() {
                            eprintln!("key {key} -> {handled:?}");
                        }
                    }
                }
            })
            .detach();
        }

        if let Some(view) = this.active_view() {
            view.update(cx, |t, cx| {
                // Debug aid for headless screenshots: IDEAE_OPEN=path:line.
                if let Ok(spec) = std::env::var("IDEAE_OPEN") {
                    let (path, line) = spec.rsplit_once(':').unwrap_or((spec.as_str(), "1"));
                    let line = line.parse::<u32>().unwrap_or(1).saturating_sub(1);
                    t.open_file_as(path.into(), line, 0, crate::task::OpenAs::Kept, window, cx);
                }
                // Debug aid for headless screenshots: IDEAE_URL opens a browser tab.
                if let Ok(url) = std::env::var("IDEAE_URL") {
                    t.open_browser(&url, window, cx);
                }
            });
        }
        // Debug aid for runs nobody clicks in: IDEAE_NEW_TASK="title\nprompt"
        // starts a task in the first project, in IDEAE_VARIANTS variants.
        // Several are parted by a line "---"; "project: title" puts one in
        // the project of that name.
        if let (Ok(tasks), false) = (std::env::var("IDEAE_NEW_TASK"), this.projects.is_empty()) {
            for task in tasks.split("\n---\n") {
                let (title, prompt) = task.split_once('\n').unwrap_or((task, ""));
                let (project, title) = match title.split_once(": ") {
                    Some((name, rest)) if let Some(pi) = this.projects.iter().position(|p| p.name == name.trim()) => (pi, rest),
                    _ => (0, title),
                };
                let variants = std::env::var("IDEAE_VARIANTS").ok().and_then(|n| n.parse::<usize>().ok()).unwrap_or(1).clamp(1, variants::MOST);
                // IDEAE_AGENTS="claude,codex": the agent of each, by profile id.
                let agents = std::env::var("IDEAE_AGENTS").map(|a| a.split(',').map(|id| id.trim().to_string()).collect()).unwrap_or_default();
                this.create_task(project, title.trim().to_string(), prompt.trim().to_string(), vec![String::new(); variants], agents, window, cx);
            }
        }
        this
    }

    /// A project as it was saved (or as the store has it), its judges'
    /// verdicts and its tasks' checks taken in.
    fn project_of_saved(&mut self, p: &state::ProjectState) -> Project {
        let mut project = Project::new(p.path.clone());
        project.space = p.space_id.clone();
        project.collapsed = p.collapsed;
        project.check = p.check.clone();
        project.check_auto = p.check_auto;
        project.settings = p.settings.clone();
        project.trusted = p.trusted.clone();
        project.hints_off = p.hints_off.iter().cloned().collect();
        project.made_by_agents = p.made_by_agents.clone();
        crate::project_config::set_own(&p.path, p.own.clone());
        // What the judge said and how the checks went: paid for, kept.
        for (group, verdict) in &p.verdicts {
            self.judges.insert(group.clone(), verdict.clone());
            self.judged.insert(group.clone());
        }
        self.stale_verdicts.extend(p.stale_verdicts.iter().cloned());
        for t in &p.tasks {
            if let Some(check) = &t.check {
                self.checks.insert(t.path.clone(), check.clone());
            }
        }
        project.tasks = p.tasks.iter().map(TaskEntry::of_saved).collect();
        project
    }

    fn restore(&mut self, saved: &State, window: &mut Window, cx: &mut Context<Self>) {
        for p in &saved.projects {
            let project = self.project_of_saved(p);
            self.projects.push(project);
        }
        self.tell_user_browsers();
        let open: Vec<PathBuf> = saved
            .projects
            .iter()
            .flat_map(|p| &p.tasks)
            // A remote task's path is on its host, not here.
            .filter(|t| t.open && (crate::hosts::is_remote(&t.path) || t.path.exists()))
            .map(|t| t.path.clone())
            .collect();
        // The one in view now; the rest after the window is up (`restoring`).
        self.last_run = open.iter().cloned().collect();
        self.restoring = open.into_iter().rev().collect();
        if let Some(active) = saved.active_task.as_ref().filter(|p| self.find(p).is_some()) {
            self.restoring.retain(|p| p != active);
            self.switch(active, window, cx);
        }
    }

    // ------------------------------------------------------------ lookup

    /// Why a task of this title cannot be made in the project: one is on its
    /// branch already. Asked by New task before it closes, not to lose what was typed.
    fn title_in_the_way(&self, project: usize, title: &str) -> Option<String> {
        let p = self.projects.get(project)?;
        let branch = naming::branch_name_as(title, &p.values().branch);
        self.task_in_the_way(project, &git::task_worktree_dir(&p.path, &branch), &branch)
    }

    /// Why a new task cannot go to `path` on `branch`: a task is there
    /// already. Two rows of one path would be one task twice.
    pub(super) fn task_in_the_way(&self, project: usize, path: &Path, branch: &str) -> Option<String> {
        let there = self.projects.iter().flat_map(|p| &p.tasks).find(|t| t.path == path);
        let on_it = || self.projects.get(project)?.tasks.iter().find(|t| !t.main && t.branch == branch);
        let t = there.or_else(on_it)?;
        Some(format!("A task is on {branch} already: {}. Switch to it, or give the new one words of its own.", t.label()))
    }

    /// The row of a task that could not be made, and none other of its path.
    pub(super) fn drop_unmade(&mut self, path: &Path) {
        for p in &mut self.projects {
            if let Some(ti) = p.tasks.iter().rposition(|t| t.path == path && t.busy.is_some()) {
                p.tasks.remove(ti);
                return;
            }
        }
    }

    pub fn find(&self, path: &Path) -> Option<(usize, usize)> {
        self.projects.iter().enumerate().find_map(|(pi, p)| {
            p.tasks.iter().position(|t| t.path == path).map(|ti| (pi, ti))
        })
    }

    fn entry_mut(&mut self, path: &Path) -> Option<&mut TaskEntry> {
        let (pi, ti) = self.find(path)?;
        Some(&mut self.projects[pi].tasks[ti])
    }

    pub fn views(&self) -> Vec<Entity<TaskView>> {
        self.projects.iter().flat_map(|p| &p.tasks).filter_map(|t| t.view.clone()).collect()
    }

    pub fn active_view(&self) -> Option<Entity<TaskView>> {
        let (pi, ti) = self.find(self.active.as_ref()?)?;
        self.projects[pi].tasks[ti].view.clone()
    }

    pub fn active_project(&self) -> Option<usize> {
        self.active.as_ref().and_then(|a| self.find(a)).map(|(pi, _)| pi)
    }

    // ------------------------------------------------------------ state

    /// A task as it is saved: what it has, its tabs from its view when it runs.
    fn task_state(&self, t: &TaskEntry, cx: &App) -> TaskState {
        TaskState {
            path: t.path.clone(),
            branch: t.branch.clone(),
            title: t.title.clone(),
            main: t.main,
            starred: t.starred,
            last_active: t.last_active,
            open: t.view.is_some() || self.restoring.contains(&t.path),
            port_slot: t.port_slot,
            session: match &t.view {
                Some(view) => view.read(cx).session(cx),
                None => t.session.clone(),
            },
            group: t.group.clone(),
            variant: t.variant,
            note: t.note.clone(),
            prompt: t.prompt.clone(),
            created: t.created,
            done_at: t.done_at,
            seen_at: t.seen_at,
            base: t.base.clone(),
            check: self.checks.get(&t.path).filter(|run| run.is_over()).cloned(),
            agent: t.agent.clone(),
            choices: t.choices.clone(),
            sessions: t.sessions.clone(),
            fresh: t.fresh,
            archive: t.archive.clone(),
            archive_keep: t.archive_keep,
            woke: t.woke.clone(),
            questions: t.questions.clone(),
            asked: t.asked,
            spawned_by: t.spawned_by.clone(),
            links: t.links.clone(),
            links_read: t.links_read,
            review: t.review,
        }
    }

    fn snapshot(&self, cx: &App) -> State {
        State {
            appearance: Some(self.appearance.label().to_lowercase()),
            sidebar_width: Some(f32::from(self.sidebar_width)),
            sidebar_rail: self.sidebar_rail,
            side_panel_width: cx.try_global::<SidePanelWidth>().map(|w| f32::from(w.0)),
            side_lower: cx.try_global::<crate::task::SideLayout>().map(|layout| layout.below),
            side_split: cx.try_global::<crate::task::SideLayout>().and_then(|layout| layout.split).map(f32::from),
            told_unlisted: self.told_unlisted,
            active_collapsed: self.active_collapsed,
            starred_collapsed: self.starred_collapsed,
            presenting: self.presenting.as_ref().map(|p| state::PresentingState { kept: p.kept.iter().cloned().collect(), since: p.since }),
            spaces: self.spaces.clone(),
            space: self.space_name(self.space.as_deref()),
            space_id: self.space.clone(),
            power_saving: crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed),
            window: self.window_place.clone(),
            settings: state::settings(),
            active_task: self.active.clone(),
            zoom: crate::zoom::current(cx),
            crew: self.crew.records(),
            learn_counts: crate::learn::counts(),
            // Set as it is written (crate::machine::sync).
            machine_rev: 0,
            projects: self
                .projects
                .iter()
                .map(|p| {
                let old = crate::layers::OldFields::from_overrides(&p.settings);
                ProjectState {
                    path: p.path.clone(),
                    space: self.space_name(p.space.as_deref()),
                    space_id: p.space.clone(),
                    settings: p.settings.clone(),
                    collapsed: p.collapsed,
                    check: p.check.clone(),
                    check_auto: p.check_auto,
                    base: old.base,
                    worktrees: old.worktrees,
                    lsp_off: old.lsp_off,
                    pr_off: old.pr_off,
                    check_to_agent: old.check_to_agent,
                    own: crate::project_config::own(&p.path),
                    trusted: p.trusted.clone(),
                    hints_off: p.hints_off.iter().cloned().collect(),
                    agent: old.agent,
                    made_by_agents: p.made_by_agents.clone(),
                    verdicts: self
                        .judges
                        .iter()
                        .filter(|(group, run)| run.is_over() && p.tasks.iter().any(|t| &t.group == *group))
                        .map(|(group, run)| (group.clone(), run.clone()))
                        .collect(),
                    // Sorted: a set lists itself in each process's order, which would look like a change to another window.
                    stale_verdicts: {
                        let mut stale: Vec<String> = self.stale_verdicts.iter().filter(|group| p.tasks.iter().any(|t| &t.group == *group)).cloned().collect();
                        stale.sort();
                        stale
                    },
                    tasks: p
                        .tasks
                        .iter()
                        // One being archived is kept till it is: the app may quit before.
                        .filter(|t| t.busy.is_none() || t.view.is_some() || !t.group.is_empty() || t.busy.as_deref() == Some(archive_ui::ARCHIVING))
                        .map(|t| self.task_state(t, cx))
                        .collect(),
                }
                })
                .collect(),
        }
    }

    /// Tasks the snapshot leaves out as they are busy (a remote one connecting,
    /// one being made): not gone, so the store keeps them as they were.
    fn kept_aside(&self) -> Vec<PathBuf> {
        self.projects.iter().flat_map(|p| &p.tasks).filter(|t| t.busy.is_some() && t.view.is_none() && t.group.is_empty() && t.busy.as_deref() != Some(archive_ui::ARCHIVING)).map(|t| t.path.clone()).collect()
    }

    /// On disk when it returns: before the window or the app goes.
    pub fn save(&mut self, cx: &App) {
        let snapshot = (self.snapshot(cx), crate::machine::taken_in(self.store), self.kept_aside());
        self.save_seq += 1;
        write_state(&self.last_saved, self.save_seq, snapshot);
        crate::secrets::flush();
    }

    /// The now-and-then save: what it takes from the UI thread is the copy
    /// of the state; turning it into text and writing it happen off it.
    pub(crate) fn save_later(&mut self, cx: &mut Context<Self>) {
        let snapshot = (self.snapshot(cx), crate::machine::taken_in(self.store), self.kept_aside());
        self.save_seq += 1;
        let (seq, last) = (self.save_seq, self.last_saved.clone());
        cx.background_spawn(async move { write_state(&last, seq, snapshot) }).detach();
    }

    // ------------------------------------------------------------ tasks

    /// Starts a task: its terminals, agent and watchers.
    fn load(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TaskView>> {
        self.restoring.retain(|p| p != path);
        if self.entry_mut(path).is_some_and(|e| e.view.is_none()) && self.waits_for_host(path, false, window, cx) {
            return None;
        }
        let socket = self.hook_socket.clone();
        let entry = self.entry_mut(path)?;
        if let Some(view) = &entry.view {
            return Some(view.clone());
        }
        if entry.busy.is_some() {
            return None;
        }
        let launch = self.launch_for(path);
        let slot = self.port_slot_for(path);
        if let Some((pi, _)) = self.find(path) {
            crate::project_config::register(path, &self.projects[pi].path);
            // Added since the layers were last put together: what applies to it, before its task reads it.
            if !self.projects[pi].resolved_once {
                self.resolve_projects(cx);
            }
        }
        let entry = self.entry_mut(path)?;
        let (root, branch, session, pr, links, review) = (entry.path.clone(), entry.branch.clone(), entry.session.clone(), entry.pr.clone(), entry.links.clone(), entry.review);
        let commit_key = self.find(path).and_then(|(pi, ti)| self.commit_key(pi, ti));
        let fill = self.usage.get(path).map_or(0., |(_, usage)| usage.fill());
        let main = self.entry_mut(path).is_some_and(|entry| entry.main);
        let kept: Vec<String> = self.find(path).map(|(pi, _)| self.projects[pi].hints_off.iter().filter_map(|key| key.strip_prefix(automation::KEPT_BRANCH)).map(str::to_string).collect()).unwrap_or_default();
        let lsp_off = self.find(path).is_some_and(|(pi, _)| !self.projects[pi].values().language_servers);
        // Its own agent was not running when the app last stopped.
        let slept = session.tabs.iter().any(|tab| tab.kind == "slept-agent");
        let quiet = slept || self.quiet_start.contains(path) || (self.last_run.remove(path) && session.tabs.iter().any(|tab| tab.kind == "paused-agent"));
        let view = cx.new(|cx| {
            let mut t = TaskView::new(root, branch, launch, !quiet, socket, slot, links, window, cx);
            // Put to sleep in the last run: it resumes when the task is shown.
            t.set_agent_slept(slept);
            t.context_fill = fill;
            // Servers start when the task comes into view (switch).
            t.lsp_asleep = true;
            // Before the tabs: a question's is restored by what is known of it.
            if let Some(entry) = self.find(path).map(|(pi, ti)| &self.projects[pi].tasks[ti]) {
                self.give_questions(entry, &mut t);
            }
            t.set_commit_key(commit_key, window, cx);
            t.restore(&session, window, cx);
            t.pr = pr;
            t.review = review;
            t.main_thread = main;
            t.kept_branches = kept;
            t.lsp_off = lsp_off;
            t
        });
        cx.subscribe_in(&view, window, |this, view, event: &TaskEvent, window, cx| this.on_task_event(view, event, window, cx)).detach();
        let entry = self.entry_mut(path)?;
        entry.view = Some(view.clone());
        // Come back while nobody looked: what a restore does on the spot.
        if let Some(told) = entry.woke.take() {
            if !told.is_empty() {
                view.update(cx, |t, cx| t.queue_prompt(told, true, window, cx));
            }
            self.task_made(path, window, cx);
            self.save_later(cx);
        }
        Some(view)
    }

    /// The task's port slot, given now if it has none: 0 for the main
    /// checkout, else the lowest one no other task of the project holds.
    fn port_slot_for(&mut self, path: &Path) -> u32 {
        let Some((pi, ti)) = self.find(path) else { return 0 };
        let tasks = &self.projects[pi].tasks;
        if tasks[ti].main || tasks[ti].port_slot > 0 {
            return tasks[ti].port_slot;
        }
        let taken: std::collections::HashSet<u32> = tasks.iter().map(|t| t.port_slot).collect();
        let slot = (1..).find(|s| !taken.contains(s)).unwrap_or(1);
        self.projects[pi].tasks[ti].port_slot = slot;
        slot
    }

    /// No task in the window: the one in hand goes out of view, and goes on.
    pub fn leave_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_group = None;
        if let Some(old) = self.active_view() {
            old.update(cx, |t, cx| {
                t.autosave_all(cx);
                t.set_visible(false, window, cx)
            });
        }
        if let Some(old) = self.active.take().and_then(|a| self.entry_mut(&a)) {
            old.left_view = Some(std::time::Instant::now());
        }
        cx.notify();
    }

    /// The window came to the front or left it: what is in view was looked at
    /// until now, and on coming back is looked at again.
    fn looked_at_in_view(&mut self, window: &Window, cx: &mut Context<Self>) {
        let now = state::now();
        let looked: Vec<PathBuf> = match &self.active_group {
            Some(group) => self.members(group).into_iter().map(|(pi, ti)| self.projects[pi].tasks[ti].path.clone()).collect(),
            None => self.active.iter().cloned().collect(),
        };
        for path in looked {
            let Some(entry) = self.entry_mut(&path) else { continue };
            entry.seen_at = now;
            if let Some(view) = entry.view.clone().filter(|_| window.is_window_active()) {
                view.update(cx, |t, cx| {
                    if t.unseen {
                        t.unseen = false;
                        cx.notify();
                    }
                });
            }
        }
    }

    pub fn switch(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        // Another shown meanwhile: the one whose host is connecting stays out of view.
        self.host_shown = None;
        if self.entry_mut(path).is_some_and(|e| e.view.is_none()) && self.waits_for_host(path, true, window, cx) {
            return;
        }
        self.active_group = None;
        if self.active.as_deref() != Some(path) {
            if let Some(old) = self.active_view() {
                old.update(cx, |t, cx| {
                    t.autosave_all(cx);
                    t.set_visible(false, window, cx)
                });
            }
            if let Some(old) = self.active.clone().and_then(|a| self.entry_mut(&a)) {
                old.left_view = Some(std::time::Instant::now());
                if window.is_window_active() {
                    old.seen_at = state::now();
                }
            }
        }
        let Some(view) = self.load(path, window, cx) else { return };
        self.active = Some(path.to_path_buf());
        if let Some(entry) = self.entry_mut(path) {
            entry.last_active = state::now();
            entry.seen_at = state::now();
        }
        view.update(cx, |t, cx| {
            t.unseen = false;
            t.wake_language_servers(cx);
            t.wake_agent(window, cx);
            t.set_visible(true, window, cx)
        });
        // A question in front is read by coming into view.
        let front = view.read(cx).questions_in_front();
        if let Some(entry) = self.entry_mut(path) {
            for q in entry.questions.iter_mut().filter(|q| front.contains(&q.id)) {
                q.unread = false;
            }
        }
        self.refresh_prs(true, window, cx);
        cx.notify();
    }

    /// Asks `gh` about the PRs of tasks: running ones every two minutes,
    /// the rest every ten, merged and closed ones hourly; `now` for the
    /// task just shown.
    fn refresh_prs(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::pr::PrState;
        // Not while saving power: each asks GitHub. What is known stays on the rows.
        if crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        use std::time::Duration;
        let mut due = Vec::new();
        let mut reviewed = Vec::new();
        // A remote project's too, once its host is connected: gh is asked here,
        // of the repository its origin there names.
        for p in self.projects.iter_mut().filter(|p| p.values().pull_requests && crate::hostfs::reachable(&p.path)) {
            for t in &mut p.tasks {
                if t.main || t.busy.is_some() {
                    continue;
                }
                let every = match &t.pr {
                    Some(pr) if matches!(pr.state, PrState::Merged | PrState::Closed) => Duration::from_secs(3600),
                    _ if t.view.is_some() => Duration::from_secs(120),
                    _ => Duration::from_secs(600),
                };
                let active = self.active.as_deref() == Some(t.path.as_path());
                let stale = t.pr_checked.is_none_or(|at| at.elapsed() >= every || (now && active && at.elapsed() > Duration::from_secs(20)));
                if stale && (!now || active) {
                    t.pr_checked = Some(std::time::Instant::now());
                    due.push((p.path.clone(), t.path.clone(), t.branch.clone()));
                    // A review's branch is a colleague's: what they push since, fetched with it.
                    if t.review {
                        reviewed.push(t.path.clone());
                    }
                }
            }
        }
        if due.is_empty() {
            return;
        }
        let reviewed_now = reviewed.clone();
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(crate::procs::on_thread(move || {
                    for task in &reviewed {
                        let _ = crate::git::fetch_upstream(task);
                    }
                    // One question to GitHub for a repository's branches, not
                    // two for each; each on its own where that is not answered.
                    let mut found = Vec::with_capacity(due.len());
                    let mut by_repo: Vec<(PathBuf, Vec<(PathBuf, String)>)> = Vec::new();
                    for (repo, path, branch) in due {
                        match by_repo.iter_mut().find(|(known, _)| *known == repo) {
                            Some((_, tasks)) => tasks.push((path, branch)),
                            None => by_repo.push((repo, vec![(path, branch)])),
                        }
                    }
                    for (repo, tasks) in by_repo {
                        let branches: Vec<String> = tasks.iter().map(|(_, branch)| branch.clone()).collect();
                        match crate::pr::lookup_all(&repo, &branches) {
                            Some(prs) => found.extend(tasks.into_iter().map(|(path, _)| path).zip(prs)),
                            None => found.extend(tasks.into_iter().map(|(path, branch)| {
                                let pr = crate::pr::lookup(&path, &branch);
                                (path, pr)
                            })),
                        }
                    }
                    found
                }))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // Fetched: the branch bar tells what came (Review again).
                for task in &reviewed_now {
                    if let Some(view) = this.entry_mut(task).and_then(|e| e.view.clone()) {
                        view.read(cx).wake_refresh();
                    }
                }
                for (path, pr) in found {
                    if let Some(old) = this.find(&path).and_then(|(pi, ti)| this.projects[pi].tasks[ti].pr.clone()) {
                        this.request_changed(&path, &old, pr.as_ref(), window, cx);
                    }
                    // A PR merged since: its branch may be left on origin.
                    let merged = |pr: &Option<crate::pr::Pr>| pr.as_ref().is_some_and(|pr| pr.state == PrState::Merged);
                    if merged(&pr) && this.entry_mut(&path).is_some_and(|entry| !merged(&entry.pr)) {
                        if let Some((pi, _)) = this.find(&path) {
                            this.projects[pi].merged_checked = None;
                        }
                    }
                    if let Some(entry) = this.entry_mut(&path) {
                        if let Some(pr) = &pr {
                            entry.set_pr_link(pr);
                        }
                        if let Some(view) = &entry.view {
                            let links = entry.links.clone();
                            view.update(cx, |t, cx| {
                                t.pr = pr.clone();
                                t.links = links;
                                cx.notify();
                            });
                        }
                        entry.pr = pr;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetches the base branch of projects with a remote, each as often as
    /// Settings say; the task views see the new refs on their next poll.
    fn fetch_bases(&mut self, cx: &mut Context<Self>) {
        let minutes = state::settings().fetch_minutes;
        // Not while saving power: a fetch wakes the network for every project.
        if minutes == 0 || crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let every = std::time::Duration::from_secs(u64::from(minutes) * 60);
        let now = std::time::Instant::now();
        let due: Vec<PathBuf> = self
            .projects
            .iter()
            .map(|p| p.path.clone())
            .filter(|path| !crate::hosts::is_remote(path))
            .filter(|path| self.fetched.get(path).is_none_or(|at| now.duration_since(*at) >= every))
            .collect();
        if due.is_empty() {
            return;
        }
        for path in &due {
            self.fetched.insert(path.clone(), now);
        }
        cx.background_spawn(crate::procs::on_thread(move || {
            for repo in due {
                if let Err(e) = git::fetch_base(&repo) {
                    if std::env::var_os("IDEAE_DEBUG").is_some() {
                        eprintln!("fetch {}: {e}", repo.display());
                    }
                }
            }
        }))
        .detach();
    }

    /// Asks git what each task changed, in projects with more than one task.
    /// `now`: a turn just ended or a group came into view, and what the
    /// tasks have read may be of before: git is asked for all of them.
    fn refresh_touched(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        let due: Vec<PathBuf> = self
            .projects
            .iter()
            .filter(|p| !crate::hosts::is_remote(&p.path) && p.tasks.iter().filter(|t| !t.main).count() > 1)
            .flat_map(|p| p.tasks.iter().filter(|t| !t.main && t.busy.is_none()).map(|t| t.path.clone()))
            .collect();
        // A loaded task has read its changes already: git is asked for the others.
        let mut known = Vec::new();
        let mut ask = Vec::new();
        for path in due {
            match self.find(&path).and_then(|(pi, ti)| self.projects[pi].tasks[ti].view.clone()).filter(|_| !now) {
                Some(view) => known.extend(view.read(cx).touched(cx).map(|files| (files, path))),
                None => ask.push(path),
            }
        }
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    ask.into_iter()
                        .filter_map(|path| {
                            let base = git::base_ref(&path)?;
                            Some((git::touched_files(&path, &base)?, path))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let touched: std::collections::HashMap<_, _> = found.into_iter().chain(known).map(|(files, path)| (path, files)).collect();
                if touched != this.touched {
                    this.touched = touched;
                    this.find_shared();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Other tasks of the project that changed files this one changed too,
    /// with those files.
    pub fn overlaps(&self, project: &Project, path: &Path) -> Vec<(String, Vec<String>)> {
        let Some(shared) = self.shared.get(path) else { return Vec::new() };
        // Variants of one task change the same files by design.
        let group = project.tasks.iter().find(|t| t.path == path).map(|t| t.group.as_str()).filter(|g| !g.is_empty());
        project
            .tasks
            .iter()
            .filter(|t| t.path != path && Some(t.group.as_str()) != group)
            .filter_map(|t| Some((t.label(), shared.iter().find(|(other, _)| *other == t.path)?.1.clone())))
            .collect()
    }

    /// The files each task changed that another of its project changed
    /// too: found when what they changed is read, not as the list is drawn.
    fn find_shared(&mut self) {
        let mut shared: std::collections::HashMap<PathBuf, Vec<(PathBuf, Vec<String>)>> = Default::default();
        for project in &self.projects {
            for (ix, a) in project.tasks.iter().enumerate() {
                let Some(mine) = self.touched.get(&a.path).filter(|mine| !mine.is_empty()) else { continue };
                for b in &project.tasks[ix + 1..] {
                    let Some(theirs) = self.touched.get(&b.path) else { continue };
                    let both: Vec<String> = mine.intersection(theirs).cloned().collect();
                    if !both.is_empty() {
                        shared.entry(a.path.clone()).or_default().push((b.path.clone(), both.clone()));
                        shared.entry(b.path.clone()).or_default().push((a.path.clone(), both));
                    }
                }
            }
        }
        self.shared = shared;
    }

    /// Language servers of tasks out of view this long go to sleep;
    /// IDEAE_LSP_IDLE_SECS shortens it for tests.
    fn lsp_idle() -> std::time::Duration {
        let secs = std::env::var("IDEAE_LSP_IDLE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(u64::from(state::settings().lsp_idle_minutes.max(1)) * 60);
        std::time::Duration::from_secs(secs)
    }

    /// Chat agents of tasks out of view this long stop; IDEAE_AGENT_IDLE_SECS
    /// shortens it for tests. Zero: they run on.
    fn agent_idle() -> std::time::Duration {
        let secs = std::env::var("IDEAE_AGENT_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(u64::from(state::settings().agent_idle_minutes) * 60);
        std::time::Duration::from_secs(secs)
    }

    /// A server that ended by itself starts again, as the files that had it ask.
    fn restart_lost_language_servers(&mut self, cx: &mut Context<Self>) {
        for view in self.views() {
            if view.read(cx).lost_a_language_server() {
                view.update(cx, |t, cx| t.restart_lost_language_servers(cx));
            }
        }
    }

    fn sleep_idle_language_servers(&mut self, cx: &mut Context<Self>) {
        for view in self.views() {
            view.update(cx, |t, cx| t.drop_unused_language_servers(Self::lsp_idle(), cx));
        }
        // The window left as long: the task in view sleeps too, a rust-analyzer
        // holds gigabytes for nobody. Not while its agent works: it may ask them of problems.
        if self.window_left.is_some_and(|left| left.elapsed() >= Self::lsp_idle())
            && let Some(view) = self.active_view()
            && !view.read(cx).agent_busy()
            && !view.read(cx).question_busy()
            && !view.read(cx).asked_of_lately(Self::lsp_idle())
            && view.read(cx).has_language_servers()
        {
            view.update(cx, |t, cx| t.put_language_servers_to_sleep(cx));
        }
        for p in &self.projects {
            for t in &p.tasks {
                let (Some(view), Some(left)) = (&t.view, t.left_view) else { continue };
                if self.active.as_deref() != Some(t.path.as_path()) && left.elapsed() >= Self::lsp_idle() {
                    if view.read(cx).has_language_servers() && !view.read(cx).asked_of_lately(Self::lsp_idle()) {
                        view.update(cx, |t, cx| t.put_language_servers_to_sleep(cx));
                    }
                    // Its browser too, unless its agent may be driving a page right now.
                    if !view.read(cx).agent_busy() && !view.read(cx).question_busy() {
                        view.update(cx, |t, cx| t.put_pages_to_sleep(cx));
                    }
                }
                let agent_idle = Self::agent_idle();
                if !agent_idle.is_zero() && self.active.as_deref() != Some(t.path.as_path()) && left.elapsed() >= agent_idle {
                    view.update(cx, |t, cx| t.put_agent_to_sleep(cx));
                }
            }
        }
    }

    /// Pauses a task: its agent, terminals and servers stop, its teardown
    /// runs; its worktree and tabs stay for the next start, a click away.
    pub fn close_task(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.find(path).and_then(|(pi, ti)| self.projects[pi].tasks[ti].view.clone()) else {
            return;
        };
        let files = view.read(cx).sql_at_risk(cx);
        let again = path.to_path_buf();
        if self.ask_losing_sql(files, window, cx, move |w, window, cx| w.stop(&again, window, cx)) {
            return;
        }
        let (own, note) = (view.read(cx).agent_busy(), view.read(cx).question_pause_note());
        if own || note.is_some() {
            let mut told = String::new();
            if own {
                told.push_str("It is in the middle of a turn. ");
            }
            told.push_str("The task's terminals are stopped too.");
            if let Some(note) = note {
                told.push(' ');
                told.push_str(note);
            }
            let told = SharedString::from(told);
            let this = cx.weak_entity();
            let path = path.to_path_buf();
            window.open_alert_dialog(cx, move |dialog, _, _| {
                let (this, path) = (this.clone(), path.clone());
                dialog
                    .confirm()
                    .title("Stop the agent?")
                    .description(told.clone())
                    .ok_text("Stop")
                    .on_ok(move |_, window, cx| {
                        let _ = this.update(cx, |this, cx| this.stop(&path, window, cx));
                        true
                    })
            });
            return;
        }
        self.stop(path, window, cx);
    }

    fn stop(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pi, ti)) = self.find(path) else { return };
        let entry = &mut self.projects[pi].tasks[ti];
        let Some(view) = entry.view.take() else { return };
        entry.session = view.read(cx).session(cx);
        // What runs for its group ends with it: a check in it, a judge reading it.
        let group = entry.group.clone();
        if !group.is_empty() {
            self.stop_runs(&group, window, cx);
        }
        let entry = &mut self.projects[pi].tasks[ti];
        view.update(cx, |t, cx| t.shut_down(cx));
        // Their agents end with it; each goes on when its question is opened.
        for q in entry.questions.iter_mut().filter(|q| q.closed == 0) {
            q.asleep = true;
        }
        // Paused as any task is, the main thread too: the branch work starts
        // from is one click away, with its tabs.
        if !entry.main {
            self.teardown(path, window, cx);
        }
        if self.active.as_deref() == Some(path) {
            self.active = None;
            // Not one out of the window: another space's, or left out of a presentation.
            let next = self
                .projects
                .iter()
                .enumerate()
                .flat_map(|(pi, p)| p.tasks.iter().enumerate().map(move |(ti, t)| (pi, ti, t)))
                .filter(|(pi, ti, t)| t.view.is_some() && self.shows(*pi, *ti))
                .map(|(_, _, t)| t)
                .max_by_key(|t| t.last_active);
            if let Some(next) = next.map(|t| t.path.clone()) {
                self.switch(&next, window, cx);
            }
        }
        self.save_later(cx);
        cx.notify();
    }

    /// A new title for a task; its branch and worktree stay as they are.
    pub fn rename_task_dialog(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry_mut(path) else { return };
        // The main thread unnamed shows its agent's name or its branch: kept as a title, it would not follow them.
        let current = if entry.main { entry.title.clone() } else { entry.label() };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Task title"));
        input.update(cx, |i, cx| {
            i.set_value(current, window, cx);
            i.select_all(window, cx);
        });
        let path = path.to_path_buf();
        let apply = {
            let (input, path) = (input.clone(), path.clone());
            move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                // Empty: named as by default again (by its agent, its branch).
                let title = input.read(cx).value().trim().to_string();
                // The variants of a task are one task by name.
                let group = this.entry_mut(&path).map(|entry| entry.group.clone()).unwrap_or_default();
                for entry in this.projects.iter_mut().flat_map(|p| &mut p.tasks) {
                    if entry.path == path || (!group.is_empty() && entry.group == group) {
                        entry.title = title.clone();
                    }
                }
                this.save_later(cx);
                window.close_dialog(cx);
                cx.notify();
            }
        };
        let this = cx.weak_entity();
        let focused = std::rc::Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, focused, this, apply) = (input.clone(), focused.clone(), this.clone(), apply.clone());
            dialog
                .title("Rename task")
                .w(px(480.))
                // Enter in the input reaches the dialog.
                .on_ok({
                    let (this, apply) = (this.clone(), apply.clone());
                    move |_, window, cx| {
                        let _ = this.update(cx, |this, cx| apply(this, window, cx));
                        false
                    }
                })
                .content(move |content, window, cx| {
                    if !focused.replace(true) {
                        let input = input.clone();
                        window.defer(cx, move |window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
                    }
                    content.child(
                        v_flex()
                            .gap_1()
                            .child(Input::new(&input))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("The branch keeps its name. Empty: the name its agent gives it.")),
                    )
                })
                .footer(h_flex().justify_end().child(Button::new("rename-task").primary().label("Rename").on_click(
                    move |_, window, cx| {
                        let apply = apply.clone();
                        let _ = this.update(cx, |w, cx| apply(w, window, cx));
                    },
                )))
        });
    }

    /// One of the tasks of the last run, its view still to be made.
    pub(crate) fn coming_back(&self, path: &Path) -> bool {
        self.restoring.iter().any(|p| p == path)
    }

    pub fn toggle_star(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(entry) = self.entry_mut(path) {
            entry.starred = !entry.starred;
        }
        self.save_later(cx);
        cx.notify();
    }

    /// Opens the project's own worktree as a task, adding it to the list.
    pub fn open_main(&mut self, project: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.projects.get_mut(project) else { return };
        if !p.tasks.iter().any(|t| t.main) {
            p.tasks.insert(
                0,
                TaskEntry {
                    path: p.path.clone(),
                    // The task tells its branch once it has read it.
                    branch: String::new(),
                    title: String::new(),
                    main: true,
                    starred: false,
                    last_active: 0,
                    session: TaskSession::default(),
                    view: None,
                    busy: None,
                    locked: None,
                    left_view: None,
                    pr: None,
                    pr_checked: None,
                    port_slot: 0,
                    ..Default::default()
                },
            );
        }
        p.collapsed = false;
        let path = p.path.clone();
        self.switch(&path, window, cx);
        if let Some(view) = self.active_view() {
            view.update(cx, |t, cx| t.show_view(0, window, cx));
        }
        // Its branch, for the list.
        self.sync_worktrees(window, cx);
    }

    /// Asks for a title and, if you like, what to tell the agent; then makes
    /// the branch and worktree.
    pub fn new_task_dialog(&mut self, project: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.new_task_dialog_with(project, None, window, cx)
    }

    /// New task, its title and prompt filled in: a follow-up of a ticket's task.
    pub fn new_task_dialog_with(&mut self, project: usize, filled: Option<(String, String)>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::{Textarea, TextareaState};
        let prefill = filled;
        let Some(p) = self.projects.get(project) else { return };
        let name = p.name.clone();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("What is it about? DEV-123 Fix login, or plain words"));
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 14)
                .placeholder(if state::settings().send_new_prompt {
                    "For the agent, optional: sent once it is up"
                } else {
                    "For the agent, optional: put into its prompt for you to finish and send"
                })
        });
        // How many variants of the task, and what each one's agent alone is told.
        let count = Rc::new(std::cell::Cell::new(1usize));
        let notes: Vec<Entity<InputState>> = (1..=variants::MOST as u32)
            .map(|v| {
                let hint = match v {
                    1 => "optional, for this agent alone: \"the smallest patch that works\"",
                    2 => "optional: \"refactor what is in the way\"",
                    _ => "optional",
                };
                cx.new(|cx| InputState::new(window, cx).placeholder(format!("{}: {hint}", variants::letter(v))))
            })
            .collect();
        // The agent of each: the project's to start with.
        let first_agent = self.project_agent(project);
        let agents: Vec<Rc<RefCell<String>>> = (0..variants::MOST).map(|_| Rc::new(RefCell::new(first_agent.clone()))).collect();
        // The title, the prompt, the variants' notes and agents, when it can be made.
        let filled_in = {
            let (input, prompt, count, notes, agents) = (input.clone(), prompt.clone(), count.clone(), notes.clone(), agents.clone());
            move |cx: &App| {
                let title = input.read(cx).value().trim().to_string();
                let text = prompt.read(cx).value().trim().to_string();
                let notes: Vec<String> = notes.iter().take(count.get()).map(|n| n.read(cx).value().trim().to_string()).collect();
                let agents: Vec<String> = agents.iter().take(count.get()).map(|a| a.borrow().clone()).collect();
                // Variants are sent their prompt: there has to be one.
                (!title.is_empty() && (notes.len() < 2 || !text.is_empty())).then_some((title, text, notes, agents))
            }
        };
        let this = cx.weak_entity();
        // A ticket in the title fills the prompt, unless you wrote one yourself.
        // Left as it was filled, it is sent: there is nothing to finish.
        let filled: Rc<RefCell<String>> = Rc::default();
        let filled_by_dialog = filled.clone();
        let as_filled = {
            let filled = filled.clone();
            move |text: &str| !text.is_empty() && text == filled.borrow().trim()
        };
        let submit = {
            let (this, filled_in, as_filled) = (this.clone(), filled_in.clone(), as_filled.clone());
            move |window: &mut Window, cx: &mut App| {
                let Some((title, text, notes, agents)) = filled_in(cx) else { return };
                if let Some(why) = this.upgrade().and_then(|w| w.read(cx).title_in_the_way(project, &title)).filter(|_| notes.len() < 2) {
                    window.push_notification(Notification::error(why).title("Could not create the task"), cx);
                    return;
                }
                window.close_dialog(cx);
                let send = as_filled(&text);
                let _ = this.update(cx, |this, cx| {
                    this.send_next_prompt = send;
                    this.create_task(project, title, text, notes, agents, window, cx)
                });
            }
        };
        // Runs inside this entity's update, so it cannot go through `submit`.
        let on_enter = {
            let filled_in = filled_in.clone();
            move |this: &mut Self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| {
                // Ctrl+Enter in the prompt; Enter in the title is the dialog's.
                let InputEvent::PressEnter { secondary: true, .. } = event else { return };
                if let Some((title, text, notes, agents)) = filled_in(cx) {
                    if let Some(why) = this.title_in_the_way(project, &title).filter(|_| notes.len() < 2) {
                        window.push_notification(Notification::error(why).title("Could not create the task"), cx);
                        return;
                    }
                    window.close_dialog(cx);
                    this.send_next_prompt = as_filled(&text);
                    this.create_task(project, title, text, notes, agents, window, cx);
                }
            }
        };
        // Kept by the dialog and gone with it: detached, they would hold the inputs they listen to for good.
        let mut listening = Vec::new();
        // A key that came from a link (Jira, Linear), and the prompt made from
        // it: the title set to the key afterwards gives the same.
        let from_link: Rc<RefCell<Option<(String, String)>>> = Rc::default();
        let fetch: Rc<RefCell<Option<Task<()>>>> = Rc::default();
        let repo = p.path.clone();
        let integrations = crate::integrations::of_resolved(&p.resolved);
        let template = p.values().branch.clone();
        let keyed_history = p.keys_in_history == Some(true);
        // origin's branches as last fetched, read once as the dialog opens, no
        // network: what Create will start from is said before it. Create asks origin itself.
        let on_origin: Rc<RefCell<OnOrigin>> = Rc::default();
        {
            let (repo, on_origin) = (repo.clone(), on_origin.clone());
            cx.spawn_in(window, async move |_, cx| {
                // On the app's executor: a thread of its own would wake the dialog
                // from outside it, which a test's scheduler takes for a race.
                let read = cx.background_spawn(async move { OnOrigin::read(&repo) }).await;
                *on_origin.borrow_mut() = read;
                let _ = cx.update(|window, _| window.refresh());
            })
            .detach();
        }
        // The same ticket's task, open or archived: one ticket, one task.
        let existing: Rc<RefCell<Option<Existing>>> = Rc::default();
        let read_texts: Rc<RefCell<std::collections::HashMap<String, String>>> = Rc::default();
        let archived: Rc<RefCell<Vec<(String, crate::archive::Archived)>>> = Rc::default();
        {
            // Read once, off the UI thread, as the dialog opens: no network, the archives here.
            let (root, project_path, archived) = (self.archive_dir.clone(), repo.clone(), archived.clone());
            cx.spawn(async move |_, cx| {
                let found = cx
                    .background_spawn(async move {
                        crate::archive::list(&root)
                            .into_iter()
                            .filter(|a| a.meta.project == project_path)
                            .flat_map(|a| {
                                let mut keys: Vec<String> = a.meta.links.iter().filter(|l| l.is_ticket()).map(|l| l.key.clone()).collect();
                                keys.extend(crate::integrations::tickets::key_at_start(&a.meta.title).map(str::to_string));
                                keys.extend(crate::integrations::tickets::keys_in(&a.meta.branch));
                                keys.dedup();
                                keys.into_iter().map(move |k| (k, a.clone()))
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                *archived.borrow_mut() = found;
            })
            .detach();
        }
        {
            let prompt = prompt.clone();
            let (existing, archived, integrations, read_texts) = (existing.clone(), archived.clone(), integrations.clone(), read_texts.clone());
            listening.push(cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let title = input.read(cx).value().trim().to_string();
                let key = crate::integrations::tickets::ticket_in(&title, &integrations).filter(|f| f.own_issue.is_none()).map(|f| f.ticket.key);
                *existing.borrow_mut() = key.and_then(|key| {
                    let open = this.projects.iter().flat_map(|p| &p.tasks).find(|t| {
                        !t.main && (t.links.iter().any(|l| l.is_ticket() && l.key == key) || crate::integrations::tickets::key_at_start(&t.title) == Some(key.as_str()))
                    });
                    match open {
                        Some(t) => Some(Existing::Open(key, t.path.clone(), t.label())),
                        None => archived.borrow().iter().find(|(k, _)| *k == key).map(|(_, a)| Existing::Archived(key, a.clone())),
                    }
                });
                window.refresh();
                let current = prompt.read(cx).value().to_string();
                if !current.trim().is_empty() && current != *filled.borrow() {
                    return;
                }
                let set = |text: String, window: &mut Window, cx: &mut App| {
                    *filled.borrow_mut() = text.clone();
                    prompt.update(cx, |p, cx| p.set_value(text, window, cx));
                };
                // A link pasted: the key names the task, the link goes to the agent.
                // A Linear link has the issue's words too: the branch is named at once.
                // Filled now: the title set from here is no change the input tells of.
                let found = crate::integrations::tickets::ticket_in(&title, &integrations);
                if let Some(found) = found.as_ref().filter(|f| f.pasted.is_some() && f.ticket.kind.is_some_and(|k| k.keyed())) {
                    *fetch.borrow_mut() = None;
                    let (key, url) = (found.ticket.key.clone(), found.pasted.clone().unwrap_or_default());
                    let told = told_of(found, keyed_history);
                    *from_link.borrow_mut() = Some((key.clone(), told.clone()));
                    set(told, window, cx);
                    let named = if found.words.is_empty() { key } else { format!("{key} {}", found.words) };
                    let short = title.replace(&url, &named).split_whitespace().collect::<Vec<_>>().join(" ");
                    let input = input.clone();
                    window.defer(cx, move |window, cx| input.update(cx, |i, cx| i.set_value(short, window, cx)));
                    return;
                }
                // An issue of the project's own repository: `#12`, or its link.
                let own = found.as_ref().and_then(|f| f.own_issue.or_else(|| f.ticket.kind.filter(|k| !k.keyed()).and_then(|_| f.ticket.key.rsplit_once('#')?.1.parse().ok())));
                if let Some(key) = found.as_ref().filter(|f| f.ticket.kind.is_none_or(|k| k.keyed()) && f.own_issue.is_none()).map(|f| f.ticket.key.clone()) {
                    *fetch.borrow_mut() = None;
                    let linked = from_link.borrow().clone().filter(|(k, _)| *k == key).map(|(_, told)| told);
                    // A ticket read already in this dialog: its words again, not asked twice.
                    let read_before = read_texts.borrow().get(&key).cloned();
                    set(read_before.clone().or(linked).unwrap_or_else(|| told_of(found.as_ref().expect("found"), keyed_history)), window, cx);
                    // Read with your token, once typing pauses: the ticket's title names the task and its
                    // branch at once, its words go into the prompt you see.
                    let reader = found.as_ref().and_then(|f| f.ticket.integration.clone()).and_then(|id| integrations.iter().find(|i| i.id == id && i.connected).cloned());
                    if let Some(reader) = reader.filter(|_| read_before.is_none()) {
                        let (input, prompt, filled, this, read_texts) = (input.clone(), prompt.clone(), filled.clone(), cx.weak_entity(), read_texts.clone());
                        let task = window.spawn(cx, async move |cx| {
                            cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
                            let Ok(read) = this.update_in(cx, |w, window, cx| w.read_ticket(reader.clone(), key.clone(), window, cx)) else { return };
                            let Ok(body) = read.await else { return };
                            let _ = cx.update(|window, cx| {
                                let typed = input.read(cx).value().trim().to_string();
                                let current = prompt.read(cx).value().to_string();
                                if current.trim().is_empty() || current == *filled.borrow() {
                                    let url = reader.tracker().map(|t| t.url_for(&body.key)).unwrap_or_default();
                                    let text = crate::integrations::read::told(&body, reader.kind, &url);
                                    read_texts.borrow_mut().insert(key.clone(), text.clone());
                                    read_texts.borrow_mut().insert(body.key.clone(), text.clone());
                                    *filled.borrow_mut() = text.clone();
                                    prompt.update(cx, |p, cx| p.set_value(text, window, cx));
                                }
                                if typed == key && !body.title.is_empty() {
                                    input.update(cx, |i, cx| i.set_value(format!("{} {}", body.key, body.title), window, cx));
                                }
                            });
                        });
                        *fetch.borrow_mut() = Some(task);
                    }
                } else if let Some(number) = own {
                    let (repo, input, prompt, filled) = (repo.clone(), input.clone(), prompt.clone(), filled.clone());
                    let task = window.spawn(cx, async move |cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
                        let issue = cx.background_spawn(crate::procs::on_thread(move || repo_issue(&repo, number))).await;
                        let Some((issue_title, body)) = issue else { return };
                        let _ = cx.update(|window, cx| {
                            let current = prompt.read(cx).value().to_string();
                            if !current.trim().is_empty() && current != *filled.borrow() {
                                return;
                            }
                            let text = format!("Issue #{number}: {issue_title}\n\n{}\n", body.trim());
                            *filled.borrow_mut() = text.clone();
                            prompt.update(cx, |p, cx| p.set_value(text, window, cx));
                            // Only the reference typed: the issue's title names the task.
                            let typed = input.read(cx).value().trim().to_string();
                            if typed == format!("#{number}") {
                                input.update(cx, |i, cx| i.set_value(format!("#{number} {issue_title}"), window, cx));
                            }
                        });
                    });
                    *fetch.borrow_mut() = Some(task);
                } else if !filled.borrow().is_empty() {
                    set(String::new(), window, cx);
                }
            }));
        }
        // Ctrl+Enter creates it from the title too, as from the prompt.
        let on_title_enter = on_enter.clone();
        listening.push(cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| on_title_enter(this, event, window, cx)));
        listening.push(cx.subscribe_in(&prompt, window, move |this, _, event: &InputEvent, window, cx| on_enter(this, event, window, cx)));
        let listening = Rc::new(listening);
        let focused = std::rc::Rc::new(std::cell::Cell::new(false));
        let this_for_existing = cx.weak_entity();
        // A ticket's link copied before New task: offered, not put in unasked.
        let from_clipboard = if prefill.is_none() && state::with_settings(|s| s.tickets) {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .map(|text| text.trim().to_string())
                .filter(|text| text.len() < 2048 && text.starts_with("http") && !text.contains(char::is_whitespace))
                .and_then(|url| crate::integrations::tickets::ticket_of_url(&url, &integrations).map(|f| (f.ticket.key, url)))
        } else {
            None
        };
        // Filled in: the title as typed (its ticket read from it), then the prompt over what that gave.
        if let Some((title, mut text)) = prefill {
            // Set here, the title tells no change: its ticket's prompt is made here too.
            if text.is_empty()
                && let Some(found) = crate::integrations::tickets::ticket_in(&title, &integrations).filter(|f| f.own_issue.is_none())
            {
                text = told_of(&found, keyed_history);
                *filled_by_dialog.borrow_mut() = text.clone();
            }
            input.update(cx, |i, cx| i.set_value(title, window, cx));
            let prompt = prompt.clone();
            if !text.is_empty() {
                window.defer(cx, move |window, cx| prompt.update(cx, |p, cx| p.set_value(text, window, cx)));
            }
        }
        window.open_dialog(cx, move |dialog, _, cx| {
            let (input, prompt, submit, focused) = (input.clone(), prompt.clone(), submit.clone(), focused.clone());
            let (same, go) = (existing.borrow().clone(), this_for_existing.clone());
            let (template, on_origin) = (template.clone(), on_origin.clone());
            let clipboard = from_clipboard.clone().filter(|_| input.read(cx).value().trim().is_empty());
            let (count, notes, agents) = (count.clone(), notes.clone(), agents.clone());
            let _ = &listening;
            let many = count.get();
            let several = state::with_settings(|s| s.several_agents());
            let muted = cx.theme().muted_foreground;
            dialog
                .title(format!("New task in {name}"))
                .w(px(640.))
                // Enter in the title reaches the dialog, which closes on it, its listeners with it.
                .on_ok({
                    let submit = submit.clone();
                    move |_, window, cx| {
                        submit(window, cx);
                        false
                    }
                })
                .content(move |content, window, cx| {
                    if !focused.replace(true) {
                        let input = input.clone();
                        window.defer(cx, move |window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
                    }
                    content.child(
                        v_flex()
                            .gap_2()
                            .child(Input::new(&input))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(branch_hint(&input.read(cx).value(), &template)),
                            )
                            // Not to be missed: someone's work may be on it.
                            .when_some(origin_note(&input.read(cx).value(), &template, &on_origin.borrow()), |form, (said, find)| {
                                let go = go.clone();
                                form.child(
                                    h_flex()
                                        .id("on-origin")
                                        .test_support()
                                        .gap_2()
                                        .items_center()
                                        // Wrapped, the button kept in the dialog.
                                        .child(div().flex_1().min_w_0().text_xs().text_color(cx.theme().warning).child(said))
                                        .child(Button::new("review-instead").flex_none().xsmall().outline().label("Review instead…").tooltip("A colleague's work: review it in a task marked a review, not go on with it").on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let find = find.clone();
                                            let _ = go.update(cx, |w, cx| w.review_dialog_with(project, Some(find), window, cx));
                                        })),
                                )
                            })
                            .when_some(same.clone(), |form, same| form.child(existing_row(same, go.clone(), cx)))
                            .when_some(clipboard.clone(), |form, (key, url)| {
                                let input = input.clone();
                                form.child(
                                    h_flex()
                                        .gap_2()
                                        .child(div().flex_1().text_xs().text_color(cx.theme().muted_foreground).child(format!("Copied: {key}")))
                                        .child(Button::new("clipboard-ticket").xsmall().outline().label(format!("Create from {key}")).on_click(move |_, window, cx| {
                                            input.update(cx, |i, cx| i.set_value(url.clone(), window, cx));
                                        })),
                                )
                            })
                            .child(Textarea::new(&prompt))
                            .when(many == 1 && (several || agents_ui::model_picker("probe", agents[0].clone()).is_some()), |form| {
                                form.child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(div().text_sm().child("Agent"))
                                        .child(agents_ui::picker("new-task-agent", agents[0].clone()))
                                        .children(agents_ui::model_picker("new-task-model", agents[0].clone())),
                                )
                            })
                            .child(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .child(div().mr_1().text_sm().child("Variants"))
                                    .children((1..=variants::MOST).map(|n| {
                                        let count = count.clone();
                                        Button::new(("variants", n)).xsmall().ghost().label(n.to_string()).selected(many == n).on_click(
                                            move |_, window, _| {
                                                count.set(n);
                                                window.refresh();
                                            },
                                        )
                                    }))
                                    .child(div().ml_2().min_w_0().text_xs().text_color(cx.theme().muted_foreground).child(if many > 1 {
                                        format!("{many} agents get the prompt right away and try it at once, each in a worktree of its own; you keep one.")
                                    } else {
                                        "More than one: several agents try it at once, and you keep the best.".to_string()
                                    })),
                            )
                            .when(many > 1, |form| {
                                // Ways to tell them apart, a click each: into the first line that is empty.
                                let ways = [
                                    ("Smallest patch", "Do it with the smallest patch that works."),
                                    ("Failing test first", "Write a failing test first, then make it pass."),
                                    ("Refactor on the way", "Refactor what is in the way before making the change."),
                                    ("Question the premise", "First question whether the task is the right fix; say so if another is better, and do that."),
                                ];
                                form.children(notes.iter().zip(&agents).take(many).enumerate().map(|(ix, (note, agent))| {
                                    h_flex()
                                        .gap_2()
                                        .child(div().flex_1().child(Input::new(note).small()))
                                        .when(several, |row| row.child(agents_ui::picker(("variant-agent", ix), agent.clone())))
                                        .children(agents_ui::model_picker(("variant-model", ix), agent.clone()))
                                })).child(
                                    h_flex().gap_1().children(ways.into_iter().enumerate().map(|(ix, (name, text))| {
                                        let notes: Vec<Entity<InputState>> = notes.iter().take(many).cloned().collect();
                                        Button::new(("way", ix)).xsmall().ghost().label(name).tooltip(text).on_click(move |_, window, cx| {
                                            if let Some(empty) = notes.iter().find(|note| note.read(cx).value().trim().is_empty()) {
                                                empty.update(cx, |input, cx| input.set_value(text, window, cx));
                                            }
                                        })
                                    })),
                                ).when(
                                    prompt.read(cx).value().trim().is_empty(),
                                    |form| {
                                        form.child(
                                            div()
                                                .text_xs()
                                                .text_color(hsla(0.08, 0.9, 0.55, 1.))
                                                .child("Variants need a prompt: it is sent to every agent as it is."),
                                        )
                                    },
                                )
                            }),
                    )
                })
                .footer({
                    let submit = submit.clone();
                    h_flex()
                        .justify_end()
                        .items_center()
                        .gap_2()
                        .child(div().text_xs().text_color(muted).child(crate::keys::label("Ctrl+Enter")))
                        .child(
                            Button::new("create-task")
                                .primary()
                                .label(if many > 1 { format!("Create {many} variants") } else { "Create".to_string() })
                                .on_click(move |_, window, cx| submit(window, cx)),
                        )
                })
        });
    }

    /// `notes` has what each variant's agent alone is told: two or more
    /// make as many variants of the task (variants.rs). `agents`: the
    /// profile each runs; none given, the project's.
    /// A task has a worktree of its own. A folder without git becomes a
    /// repository for it; while there is no commit to start from, or it
    /// cannot be one (no git, another version control), the work is done in
    /// the folder, by the project's main task.
    fn create_task(&mut self, project: usize, title: String, prompt: String, notes: Vec<String>, agents: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        // Taken now, for this task alone, whatever way it is made.
        let given = std::mem::take(&mut self.send_next_prompt);
        let Some(repo) = self.projects.get(project).map(|p| p.path.clone()) else { return };
        // A remote project's: made there as here, once its host is connected (git, its
        // files and its .ideae.toml are read and written there).
        if crate::hosts::is_remote(&repo) {
            let key = repo.clone();
            return self.with_host(&repo, window, cx, Box::new(move |this, window, cx| {
                // By its path: the list may have changed while its host was connected.
                if let Some(project) = this.projects.iter().position(|p| p.path == key) {
                    this.create_worktree_task(project, title, prompt, notes, agents, given, window, cx)
                }
            }));
        }
        cx.spawn_in(window, async move |this, cx| {
            let folder = repo.clone();
            let has_commit = cx.background_spawn(async move { git::adopt(&folder) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(project) = this.projects.iter().position(|p| p.path == repo) else { return };
                if has_commit {
                    return this.create_worktree_task(project, title, prompt, notes, agents, given, window, cx);
                }
                this.open_main(project, window, cx);
                let view = this.entry_mut(&repo).and_then(|entry| entry.view.clone());
                if let (false, Some(view)) = (prompt.is_empty(), view) {
                    view.update(cx, |t, cx| t.queue_prompt(prompt, given || state::settings().send_new_prompt, window, cx));
                }
                let told = "Tasks get a worktree of their own from a git commit. There is none here yet, so this one works right in the folder.";
                window.push_notification(Notification::info(told).title("Working in the folder"), cx);
            });
        })
        .detach();
    }

    /// `given`: its prompt is sent, not left for you to finish.
    pub(super) fn create_worktree_task(&mut self, project: usize, title: String, prompt: String, notes: Vec<String>, agents: Vec<String>, given: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Variants are sent their prompt anyway.
        if notes.len() > 1 {
            return self.create_variants(project, title, prompt, notes, agents, window, cx);
        }
        // A colleague's request pasted: a task on its branch.
        if let Some((kind, number, url)) = title.split_whitespace().find_map(|w| crate::integrations::request_of_url(w).map(|(k, n)| (k, n, w.to_string()))) {
            return self.create_request_task(project, kind, number, url, prompt, agents, given, window, cx);
        }
        let Some(p) = self.projects.get(project) else { return };
        let branch = naming::branch_name_as(&title, &p.values().branch);
        self.create_task_on_branch(project, branch, title, prompt, agents, given, false, window, cx);
    }

    /// A task on `branch`: origin's of that name when it has one (git), else
    /// a new one from the base. `review`: on a colleague's branch, to review.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_task_on_branch(
        &mut self,
        project: usize,
        branch: String,
        title: String,
        prompt: String,
        agents: Vec<String>,
        given: bool,
        review: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.projects.get(project) else { return };
        let (repo, base) = (p.path.clone(), p.values().base.clone());
        let path = git::task_worktree_dir(&repo, &branch);
        if let Some(why) = self.task_in_the_way(project, &path, &branch) {
            window.push_notification(Notification::error(why).title("Could not create the task"), cx);
            return;
        }
        let p = &mut self.projects[project];
        p.collapsed = false;
        let links = crate::integrations::tickets::links_for_task(&title, &prompt, &crate::integrations::of_resolved(&p.resolved));
        p.tasks.push(TaskEntry {
            path: path.clone(),
            branch: branch.clone(),
            links,
            links_read: true,
            title,
            last_active: state::now(),
            busy: Some("creating…".into()),
            prompt: prompt.clone(),
            created: state::now(),
            agent: agents_ui::pick_profile(agents.first().map_or("", String::as_str)),
            choices: agents_ui::pick_choices(agents.first().map_or("", String::as_str)),
            fresh: true,
            review,
            ..Default::default()
        });
        if let Some(link) = self.projects[project].tasks.last().and_then(|t| t.ticket().cloned()) {
            self.ask_for_integration(project, &link, window, cx);
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let copy_env = state::settings().copy_env;
            let result = cx
                .background_spawn(async move {
                    let copy = crate::project_config::resolve(&repo).config.task.copy;
                    let copy_env = copy_env && copy.is_none();
                    // A review is of a branch there is, never a new one from the base.
                    let made = if review { git::review_worktree(&repo, &branch, copy_env) } else { git::new_task_worktree(&repo, &branch, &base, copy_env) };
                    if let (Ok((dir, _)), Some(copy)) = (&made, copy) {
                        crate::project_config::copy_ignored(&repo, dir, &copy);
                    }
                    made
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok((_, told)) => {
                        if let Some(entry) = this.entry_mut(&path) {
                            entry.busy = None;
                        }
                        // Not from the base: on origin's branch of its name, or one here.
                        if let Some(told) = told {
                            window.push_notification(Notification::info(told).title("On a branch there was already").autohide(false), cx);
                        }
                        // One an agent started runs out of view: what you look at stays.
                        let spawned = this.entry_mut(&path).is_some_and(|entry| entry.spawned_by.is_some());
                        if spawned {
                            this.load(&path, window, cx);
                        } else {
                            this.switch(&path, window, cx);
                        }
                        this.task_made(&path, window, cx);
                        this.check_ticket(&path, window, cx);
                        // To its own agent: the task in view is another when the switch did not happen.
                        let view = this.entry_mut(&path).and_then(|entry| entry.view.clone());
                        if let (false, Some(view)) = (prompt.is_empty(), view) {
                            // Sent when Settings say so, and always when nobody is there to press Enter (IDEAE_NEW_TASK,
                            // an agent's task) or there is nothing to finish (a prompt the dialog filled).
                            let send = state::settings().send_new_prompt || spawned || given || std::env::var_os("IDEAE_NEW_TASK").is_some();
                            view.update(cx, |t, cx| t.queue_prompt(prompt, send, window, cx));
                        }
                    }
                    Err(e) => {
                        this.drop_unmade(&path);
                        window.push_notification(Notification::error(e).title("Could not create the task"), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A binary downloaded and run is in no list of applications, and
    /// cannot be found there or pinned: said once, with the way to add it.
    /// Not written by itself: files in the home folder are the user's to ask for.
    fn tell_unlisted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A run on a scratch state is a test: the launcher is the user's own.
        if self.desk.listed || self.told_unlisted || cfg!(test) || std::env::var_os("IDEAE_STATE").is_some() {
            return;
        }
        self.told_unlisted = true;
        self.save_later(cx);
        let this = cx.weak_entity();
        let note = Notification::info("It is not in the list of applications yet: it cannot be found there or pinned. Window → Add to Applications does the same later.")
            .title("Add ideae to your applications?")
            .autohide(false)
            .action(move |_, _, cx| {
                let (this, me) = (this.clone(), cx.entity().downgrade());
                gpui_kit::component::button::Button::new("list-app").xsmall().outline().label("Add to Applications").on_click(move |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.list_app(window, cx));
                    let _ = me.update(cx, |note, cx| note.dismiss(window, cx));
                })
            });
        window.push_notification(note, cx);
    }

    /// Lists the app with the applications: a launcher entry and its icon,
    /// or a Start menu shortcut. Off the UI thread, and told how it went.
    pub fn list_app(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let (done, desk) = cx.background_spawn(async { (crate::desktop::install(), crate::desktop::standing()) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.desk = desk;
                let note = match done {
                    Ok(told) => Notification::info(told),
                    Err(why) => Notification::error(why),
                };
                window.push_notification(note.title("Add to Applications"), cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Pins the app to the GNOME dash, or takes it off; the entry is
    /// written first when there is none.
    pub fn toggle_pin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pinned = self.desk.pinned;
        cx.spawn_in(window, async move |this, cx| {
            let (done, desk) = cx
                .background_spawn(async move {
                    let done = if pinned { Ok(crate::desktop::unpin()) } else { crate::desktop::pin() };
                    (done, crate::desktop::standing())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.desk = desk;
                if let Err(why) = done {
                    window.push_notification(Notification::error(why).title("Pin to dash"), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ projects

    /// Adds a repository (by any path inside it) and returns its index.
    pub fn add_project(&mut self, path: &Path) -> Option<usize> {
        Some(self.add_project_root(project_root(path)?))
    }

    fn add_project_root(&mut self, root: PathBuf) -> usize {
        if let Some(ix) = self.projects.iter().position(|p| p.path == root) {
            return ix;
        }
        // A project added while a space is in the window is one of that space.
        let mut project = Project::new(root);
        project.space = self.space.clone();
        self.projects.push(project);
        self.projects.len() - 1
    }

    pub fn add_project_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let paths = match picked.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                // No folder picker (no desktop portal): say how else to add one.
                _ => {
                    let _ = this.update_in(cx, |_, window, cx| {
                        let told = "The folder picker is not available here. Start ideae with the repository instead: ideae /path/to/repo";
                        window.push_notification(Notification::error(told).title("Add project"), cx);
                    });
                    return;
                }
            };
            let picked_any = !paths.is_empty();
            let roots = cx.background_spawn(async move { paths.iter().filter_map(|path| project_root(path)).inspect(|root| { git::adopt(root); }).collect::<Vec<_>>() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if picked_any && roots.is_empty() {
                    window.push_notification(Notification::error("Not in a git repository: a project is a git repository's folder.").title("Add project"), cx);
                }
                for root in roots {
                    this.add_project_root(root);
                }
                this.sync_worktrees(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn remove_project(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.projects.get(ix) else { return };
        let running: Vec<PathBuf> = p.tasks.iter().filter(|t| t.view.is_some()).map(|t| t.path.clone()).collect();
        let name = p.name.clone();
        let path = p.path.clone();
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (this, running, path) = (this.clone(), running.clone(), path.clone());
            dialog
                .confirm()
                .title(format!("Remove {name} from the list?"))
                .description(if crate::hosts::is_remote(&path) {
                    "It stays on its machine, with its tasks, for its own window and others; running tasks here are stopped. Shown again from its machine's row below the projects."
                } else {
                    "Its files and worktrees stay on disk; running tasks are stopped."
                })
                .ok_text("Remove")
                .on_ok(move |_, window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        for task in &running {
                            this.stop(task, window, cx);
                        }
                        this.projects.retain(|p| p.path != path);
                        // Another machine's stays there, with its deferred tasks: shown no more here, offered again.
                        if let Some((host, _)) = crate::hosts::split(&path) {
                            #[cfg(unix)]
                            this.find_more_elsewhere(&host);
                            let _ = host;
                        } else {
                            // Its deferred tasks no longer come back: archives like the others.
                            let (root, project) = (this.archive_dir.clone(), path.clone());
                            cx.background_spawn(async move { crate::archive::undefer_project(&root, &project) }).detach();
                        }
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// Brings the task list in line with `git worktree list`: worktrees made
    /// outside the app appear, removed ones go.
    pub fn sync_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A remote project's worktrees are listed on its host, once it is connected.
        let repos: Vec<(PathBuf, std::collections::BTreeMap<PathBuf, PathBuf>)> = self
            .projects
            .iter()
            .filter(|p| !crate::hosts::is_remote(&p.path) || crate::hostfs::reachable(&p.path))
            .map(|p| (p.path.clone(), p.made_by_agents.clone()))
            .collect();
        // The branch each task owns, and since when.
        let owned: std::collections::HashMap<PathBuf, (String, u64)> =
            self.projects.iter().flat_map(|p| &p.tasks).filter(|t| t.owns_branch()).map(|t| (t.path.clone(), (t.branch.clone(), t.created))).collect();
        cx.spawn_in(window, async move |this, cx| {
            let lists = cx
                .background_spawn(async move {
                    repos
                        .into_iter()
                        .map(|(r, made)| {
                            let worktrees = git::worktrees(&r);
                            // Where each task's worktree is, and whether a branch it is on is its own work renamed.
                            let heads: std::collections::HashMap<PathBuf, (git::Head, bool)> = worktrees
                                .iter()
                                .skip(1)
                                .filter_map(|w| {
                                    let (own, since) = owned.get(&w.path)?;
                                    let head = git::head_of(&w.path, &w.branch);
                                    let grew = head.doing.is_none() && !head.branch.is_empty() && git::grew_from(&w.path, own, &head.branch, *since);
                                    Some((w.path.clone(), (head, grew)))
                                })
                                .collect();
                            let all: Vec<PathBuf> = worktrees.iter().map(|w| w.path.clone()).collect();
                            // An agent's worktrees, with what they hold beyond the branch they came from.
                            let sides: std::collections::HashMap<PathBuf, side::Side> = worktrees
                                .iter()
                                .skip(1)
                                .filter(|w| !w.in_use())
                                .filter_map(|w| {
                                    // Whose it is, as its agent's hook told, before what its place says.
                                    let parent = made
                                        .get(&w.path)
                                        .map(|task| all.contains(task).then(|| task.clone()))
                                        .or_else(|| side::made_by_agent(&w.path, &all))?;
                                    let from = parent.as_ref().unwrap_or(&all[0]);
                                    let base = worktrees.iter().find(|o| &o.path == from).map(|o| o.branch.clone()).unwrap_or_default();
                                    let (ahead, dirty) = git::work_beyond(&w.path, &base);
                                    // Claude Code's notes of it are on its machine.
                                    let about = if crate::hosts::is_remote(&w.path) { None } else { side::about(&w.path, from) };
                                    Some((w.path.clone(), side::Side { parent, ahead, dirty, about }))
                                })
                                .collect();
                            // Removed since: forgotten. One not made yet (its hook comes
                            // before the command runs) is not on disk either way.
                            let gone: Vec<PathBuf> = made.keys().filter(|path| !all.contains(path) && crate::hostfs::stat(path).is_err()).cloned().collect();
                            (worktrees, sides, r, gone, heads)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let restoring = this.restoring.clone();
                for (worktrees, mut sides, repo, gone, mut heads) in lists {
                    let Some(p) = this.projects.iter_mut().find(|p| p.path == repo) else { continue };
                    for path in gone {
                        p.made_by_agents.remove(&path);
                    }
                    // Not readable now (unmounted, git missing): leave it be.
                    if worktrees.first().is_none_or(|w| w.path != repo) {
                        continue;
                    }
                    // Not a repository, or git cannot read it now: its tasks stay.
                    if worktrees[0].branch == git::NO_GIT {
                        match p.tasks.iter_mut().find(|t| t.main) {
                            Some(main) => main.branch.clear(),
                            None => p.tasks.insert(0, TaskEntry { path: p.path.clone(), main: true, ..Default::default() }),
                        }
                        continue;
                    }
                    // Worktrees locked by a live process (Claude Code subagents)
                    // are that process's for now: not tasks.
                    let worktrees: Vec<_> = worktrees.into_iter().enumerate().filter(|(i, w)| *i == 0 || !w.in_use()).map(|(_, w)| w).collect();
                    for wt in worktrees.iter().skip(1) {
                        // One the user made a task of keeps being theirs.
                        let side = sides.remove(&wt.path);
                        match p.tasks.iter_mut().find(|t| t.path == wt.path) {
                            Some(t) => {
                                match heads.remove(&wt.path).filter(|_| t.owns_branch()) {
                                    Some((head, grew)) => {
                                        if grew {
                                            t.branch = head.branch.clone();
                                        }
                                        t.head = head.off(&t.branch).is_some().then_some(head);
                                    }
                                    None => {
                                        t.branch = wt.branch.clone();
                                        t.head = None;
                                    }
                                }
                                if let Some(view) = t.view.clone() {
                                    let (own, off) = (t.branch.clone(), t.head.as_ref().and_then(|h| h.off(&t.branch)));
                                    view.update(cx, |v, cx| v.set_own_branch(own, off, cx));
                                }
                                t.locked = wt.locked.clone();
                                t.side = side.filter(|_| t.title.is_empty() && t.group.is_empty() && t.prompt.is_empty());
                            }
                            None => p.tasks.push(TaskEntry {
                                path: wt.path.clone(),
                                branch: wt.branch.clone(),
                                title: String::new(),
                                main: false,
                                starred: false,
                                last_active: 0,
                                session: TaskSession::default(),
                                view: None,
                                busy: None,
                                locked: wt.locked.clone(),
                                left_view: None,
                                pr: None,
                                pr_checked: None,
                                port_slot: 0,
                                side,
                                ..Default::default()
                            }),
                        }
                    }
                    // Every project has its main thread, paused until opened.
                    if !p.tasks.iter().any(|t| t.main) {
                        p.tasks.insert(0, TaskEntry { path: p.path.clone(), main: true, ..Default::default() });
                    }
                    if let Some(main) = p.tasks.iter_mut().find(|t| t.main) {
                        main.branch = worktrees[0].branch.clone();
                    }
                    // One of the last run not loaded yet is as good as loaded:
                    // dropped here, its terminals would be ended as nobody's.
                    p.tasks.retain(|t| {
                        t.main || t.busy.is_some() || t.view.is_some() || restoring.contains(&t.path) || worktrees.iter().any(|w| w.path == t.path)
                    });
                }
                this.dissolve_lonely_groups();
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ order

    pub fn move_project(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to || from >= self.projects.len() || to > self.projects.len() {
            return;
        }
        let p = self.projects.remove(from);
        let to = if to > from { to - 1 } else { to };
        self.projects.insert(to.min(self.projects.len()), p);
        cx.notify();
    }

    /// Moves a task within its project to just before `to` (or the end).
    pub fn move_task(&mut self, path: &Path, project: usize, to: Option<usize>, cx: &mut Context<Self>) {
        let Some((pi, from)) = self.find(path) else { return };
        if pi != project {
            return;
        }
        let tasks = &mut self.projects[pi].tasks;
        let task = tasks.remove(from);
        let to = to.map_or(tasks.len(), |to| if to > from { to - 1 } else { to }).min(tasks.len());
        tasks.insert(to, task);
        cx.notify();
    }

    // ------------------------------------------------------------ hooks

    fn on_hook(&mut self, event: agent::HookEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let crate::task::Who::Question(n) = crate::task::Who::of_tag(event.agent.as_deref()) {
            return self.question_heard(n, event, window, cx);
        }
        // Which session the agent runs: what a restart, or the next run, resumes.
        if let Some(id) = event.session.as_deref().filter(|id| !id.is_empty()) {
            self.learn_session(&event.task, id, cx);
        }
        // A worktree its agent is about to make: folded under the task once it is there.
        if let Some(made) = event.worktree.as_deref().map(PathBuf::from)
            && let Some((pi, _)) = self.find(&event.task)
            && made != event.task
        {
            self.projects[pi].made_by_agents.insert(made, event.task.clone());
        }
        let Some(status) = event.status() else { return };
        let Some((pi, ti)) = self.find(&event.task) else { return };
        let entry = &self.projects[pi].tasks[ti];
        let Some(view) = entry.view.clone() else { return };
        // Its end, of an agent put to sleep: it is not gone.
        if event.event == "SessionEnd" && view.read(cx).agent_slept() {
            return;
        }
        let group = (!entry.group.is_empty()).then(|| entry.group.clone());
        let mut label = format!("{} · {}", self.projects[pi].name, entry.label());
        let variant = variants::letter(entry.variant);
        // A group on its screen is in view with all its variants.
        let in_view = self.active.as_deref() == Some(event.task.as_path()) || (group.is_some() && self.active_group == group);
        let looked = in_view && window.is_window_active();
        // Heard again after a restart: new only when it came after the last look.
        let seen = looked || event.replayed.is_some_and(|at| at <= entry.seen_at);
        let was = view.read(cx).agent.status.clone();
        // The Notification about a question its PreToolUse told already: no second notice, the question kept.
        let status = if agent::asked_again(&was, &event) { was.clone() } else { status };
        if looked {
            self.projects[pi].tasks[ti].seen_at = state::now();
        }
        // A turn's end, not the reminder a minute later or a session's start.
        let turn_over = status == agent::AgentStatus::Done && matches!(was, agent::AgentStatus::Working | agent::AgentStatus::NeedsInput(_));
        // A message that waited for this goes out now: the agent is not done yet.
        let more_coming = turn_over && view.read(cx).has_queued_message();
        let (heard, looping) = view.update(cx, |t, cx| {
            let said = (t.agent.said.clone(), t.agent.asks.clone());
            let looping = t.agent.hear(&event, &status).then(|| t.agent.looping.clone()).flatten();
            if looping.is_some() {
                cx.emit(TaskEvent::Changed);
            }
            t.agent.guessed = event.guessed;
            t.set_agent_status(crate::task::Who::Main, status.clone(), seen, cx);
            let before = t.agent_shown();
            t.follow_agent(&event.event, event.file.as_deref());
            match event.event.as_str() {
                "PreToolUse" if status == agent::AgentStatus::Working => t.agent.doing = event.message.clone(),
                "UserPromptSubmit" => t.agent.doing = Some("Thinking".into()),
                _ => {}
            }
            if t.agent_shown() != before {
                cx.emit(TaskEvent::Changed);
                cx.notify();
            }
            if turn_over && event.replayed.is_none() {
                t.deliver_queued(crate::task::Who::Main, window, cx);
                t.reload_own_pages(cx);
                t.recheck_review(cx);
            }
            (said != (t.agent.said.clone(), t.agent.asks.clone()), looping)
        });
        // Not stopped: a run of the same call may be meant. Told, as a question would be.
        if let Some(doing) = looping.filter(|_| !seen && event.replayed.is_none() && !window.is_window_active() && state::settings().notifications) {
            let (tx, path) = (self.open_task_tx.clone(), event.task.clone());
            crate::desktop::notify_with(&label, &format!("Goes round in circles: {doing}"), move || {
                let _ = tx.send_blocking((path, None));
            });
        }
        // A variant at work again: what the judge said is of before.
        if let (Some(group), "UserPromptSubmit") = (&group, event.event.as_str()) {
            if self.judges.contains_key(group) {
                self.group_changed(group);
            }
        }
        // To the minute: an agent reports every tool it uses, and each new
        // second here would have the state written again.
        let last = &mut self.projects[pi].tasks[ti].last_active;
        if state::now().saturating_sub(*last) >= 60 || turn_over {
            *last = state::now();
        }
        if turn_over && event.replayed.is_none() && !more_coming {
            self.after_turn(&event.task, window, cx);
            self.name_ticket_task(&event.task, window, cx);
        }
        if turn_over {
            self.projects[pi].tasks[ti].done_at = state::now();
            self.refresh_variant_info(window, cx);
            self.refresh_usage(window, cx);
            if let Some(group) = &group {
                self.refresh_touched(true, window, cx);
                if event.replayed.is_none() && !more_coming && self.group_settled(group, cx) {
                    self.group_came_through(group, window, cx);
                }
            }
        }
        // Out of sight while presenting: nothing is told now, all of it at the end.
        let hidden = !self.shows(pi, ti);
        if hidden && status.is_attention() && status != was && event.replayed.is_none() {
            self.hold_notice(&event.task);
        }
        // Not while the window is in front: the sidebar says it there, in Needs you.
        if status.is_attention() && !seen && !hidden && !window.is_window_active() && status != was && event.replayed.is_none() && state::settings().notifications {
            let said = self.said_shown(&view, cx);
            let words = state::with_settings(|s| s.notify_said && s.show_said) && !self.is_presenting();
            let body = match (&status, &group) {
                (agent::AgentStatus::NeedsInput(_), Some(_)) => {
                    label = format!("{label} · {variant}");
                    Some(crate::said::notice(&status, said.as_deref(), words))
                }
                // A group tells when it is ready to look at: after the judge (variants.rs).
                (_, Some(_)) => None,
                (_, None) => Some(crate::said::notice(&status, said.as_deref(), words)),
            };
            if let Some(body) = body {
                let (tx, path) = (self.open_task_tx.clone(), event.task.clone());
                crate::desktop::notify_with(&label, &body, move || {
                    let _ = tx.send_blocking((path, None));
                });
            }
        }
        // The rest reaches the list through the task, when it shows another way.
        if status != was || turn_over || heard {
            cx.notify();
        }
    }

    /// A task titled by its ticket's key alone (a link pasted, nothing else)
    /// gets a title from what its agent said after reading the ticket. Once,
    /// after the first turn that says something. Its branch is left as it
    /// is: renamed under its agent, it would surprise it, and how branches
    /// are named is the project's rule, which its agent knows, not ideae.
    fn name_ticket_task(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry_mut(path) else { return };
        let key = entry.title.trim().to_string();
        if entry.main || !entry.group.is_empty() || crate::integrations::tickets::key_at_start(&key) != Some(key.as_str()) {
            return;
        }
        let said = entry.view.as_ref().and_then(|v| v.read(cx).agent.said.as_ref().map(|s| s.whole.to_string()));
        let Some(said) = said.filter(|_| self.naming_tasks.insert(path.to_path_buf())) else { return };
        let path = path.to_path_buf();
        cx.spawn_in(window, async move |this, cx| {
            let job_key = key.clone();
            let named = cx
                .background_spawn(crate::procs::on_thread(move || questions::name_ticket(&job_key, &said).map(|name| format!("{job_key} {name}"))))
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(title) = named else { return };
                if let Some(entry) = this.entry_mut(&path).filter(|e| e.title.trim() == key) {
                    entry.title = title;
                }
                this.save_later(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The agent reported its session id: kept as the task's latest
    /// session's, the one resumed. Claude Code's `/clear` starts another.
    fn learn_session(&mut self, task: &Path, id: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.entry_mut(task) else { return };
        let Some(session) = entry.sessions.last_mut() else { return };
        if session.id.as_deref() == Some(id) {
            return;
        }
        session.id = Some(id.to_string());
        if let Some(view) = entry.view.clone() {
            view.update(cx, |t, _| t.agent.learned_session(id));
        }
    }

    /// What the agent of a task said, as far as it is to be shown: not
    /// while presenting, not when turned off. Whole, for a notification.
    pub fn said_shown(&self, view: &Entity<crate::task::TaskView>, cx: &App) -> Option<SharedString> {
        if self.is_presenting() || !state::with_settings(|s| s.show_said) {
            return None;
        }
        view.read(cx).agent.shown_said().map(|said| said.whole.clone())
    }

    /// Power saving: stops every language server and starts none until
    /// turned off, when the open files get theirs again.
    /// What the agents of the tasks that run have used: each transcript
    /// read again only when it was written to, off the UI thread.
    pub fn refresh_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.reading_usage, true) {
            return;
        }
        let known: Vec<(PathBuf, crate::agents::SessionRef, Option<std::time::SystemTime>)> = self
            .projects
            .iter()
            .flat_map(|p| &p.tasks)
            .filter(|t| t.view.is_some() && !crate::hosts::is_remote(&t.path))
            .filter_map(|t| Some((t.path.clone(), t.session_ref()?, self.usage.get(&t.path).and_then(|(written, _)| *written))))
            .collect();
        // Questions by their own sessions: how full each is, for its tab.
        let asked: Vec<(PathBuf, u32, crate::agents::SessionRef, Option<std::time::SystemTime>)> = self
            .projects
            .iter()
            .flat_map(|p| &p.tasks)
            .filter(|t| t.view.is_some())
            .flat_map(|t| t.questions.iter().filter(|q| q.closed == 0).map(move |q| (t, q)))
            .filter_map(|(t, q)| {
                let profile = crate::agents::profile_of(&t.path, &q.profile)?;
                let who = crate::agents::SessionRef { profile, id: Some(q.session.clone()?) };
                Some((t.path.clone(), q.id, who, self.question_usage.get(&(t.path.clone(), q.id)).copied().flatten()))
            })
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let fills = cx
                .background_spawn(async move {
                    asked
                        .into_iter()
                        .filter_map(|(path, n, who, before)| {
                            let transcript = who.own_transcript(&path)?;
                            let written = std::fs::metadata(&transcript).and_then(|m| m.modified()).ok();
                            (written != before).then(|| who.profile.adapter().usage(&transcript)).flatten().map(|usage| (path, n, written, usage.fill()))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let read = cx
                .background_spawn(async move {
                    known
                        .into_iter()
                        .filter_map(|(path, who, before)| {
                            let transcript = who.own_transcript(&path)?;
                            let written = std::fs::metadata(&transcript).and_then(|m| m.modified()).ok();
                            (written != before).then(|| who.profile.adapter().usage(&transcript)).flatten().map(|usage| (path, written, usage))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            if std::env::var_os("IDEAE_DEBUG").is_some() {
                eprintln!("usage read: {:?}", read.iter().map(|(path, _, usage)| (path, usage.context, usage.recent)).collect::<Vec<_>>());
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.reading_usage = false;
                // What is kept by task goes when the task has left the list.
                let listed: std::collections::HashSet<PathBuf> = this.projects.iter().flat_map(|p| &p.tasks).map(|t| t.path.clone()).collect();
                this.usage.retain(|path, _| listed.contains(path));
                this.usage_warned.retain(|path| listed.contains(path));
                crate::project_config::retain_tasks(&listed);
                this.last_in_space.retain(|_, path| listed.contains(path));
                this.question_usage.retain(|(path, n), _| this.projects.iter().flat_map(|p| &p.tasks).any(|t| t.path == *path && t.questions.iter().any(|q| q.id == *n && q.closed == 0)));
                for (path, n, written, fill) in fills {
                    if let Some(view) = this.find(&path).and_then(|(pi, ti)| this.projects[pi].tasks[ti].view.clone()) {
                        view.update(cx, |t, cx| t.set_question_fill(n, fill, cx));
                    }
                    this.question_usage.insert((path, n), written);
                }
                let limit = u64::from(state::with_settings(|s| s.token_warn_k)) * 1000;
                for (path, written, usage) in read {
                    // A new session of the task starts its count again.
                    if this.usage.get(&path).is_some_and(|(_, before)| before.spent() > usage.spent()) {
                        this.usage_warned.remove(&path);
                    }
                    if limit > 0 && usage.spent() >= limit && this.shows_path(&path) && this.usage_warned.insert(path.clone()) {
                        let name = this.find(&path).map(|(pi, ti)| format!("{} · {}", this.projects[pi].name, this.projects[pi].tasks[ti].label())).unwrap_or_default();
                        let told = format!("{name}: its agent has used {} tokens in this session.", crate::usage::tokens(usage.spent()));
                        window.push_notification(gpui_kit::component::notification::Notification::warning(told.clone()).title("Tokens"), cx);
                        if state::with_settings(|s| s.notifications) && !window.is_window_active() {
                            crate::desktop::notify("Tokens", &told);
                        }
                    }
                    let fill = usage.fill();
                    if let Some(view) = this.find(&path).and_then(|(pi, ti)| this.projects[pi].tasks[ti].view.clone()) {
                        view.update(cx, |t, cx| {
                            if t.context_fill != fill {
                                t.context_fill = fill;
                                cx.notify();
                            }
                        });
                    }
                    this.usage.insert(path, (written, usage));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// What all the tasks' agents took in the last day.
    pub fn tokens_of_the_day(&self) -> u64 {
        // Of the space in the window: what the others took is theirs to show.
        self.usage.iter().filter(|(path, _)| self.find(path).is_none_or(|(pi, _)| self.in_space(pi))).map(|(_, (_, usage))| usage.recent).sum()
    }

    /// The same by agent, with more than one: by profile name.
    pub fn tokens_by_agent(&self) -> Vec<(String, u64)> {
        let mut by: std::collections::BTreeMap<String, u64> = Default::default();
        state::with_settings(|s| {
            if !s.several_agents() {
                return;
            }
            for (path, (_, usage)) in &self.usage {
                let Some((pi, ti)) = self.find(path).filter(|(pi, _)| self.in_space(*pi)) else { continue };
                let id = self.projects[pi].tasks[ti].sessions.last().map(|x| x.profile.clone()).unwrap_or_default();
                let name = s.profile(&id).map_or_else(|| "Other".to_string(), |p| p.name.clone());
                *by.entry(name).or_default() += usage.recent;
            }
        });
        by.into_iter().filter(|(_, n)| *n > 0).collect()
    }

    /// The system kept from sleeping while a task works, as the settings say.
    fn keep_awake(&mut self, cx: &mut Context<Self>) {
        let (on, battery) = state::with_settings(|s| (s.keep_awake, s.keep_awake_on_battery));
        // Agents of this machine: another's keeps itself up (its daemon), this one need not.
        let working = on && self.projects.iter().flat_map(|p| &p.tasks).any(|t| !crate::hosts::is_remote(&t.path) && t.view.as_ref().is_some_and(|v| v.read(cx).at_work()));
        self.awake.want(working, !battery);
    }

    /// The battery low and in use, and power not saved: said once as it
    /// runs down, with the way to save. Read off the UI thread.
    fn look_at_battery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !state::with_settings(|s| s.battery_hint) {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let read = cx.background_spawn(async { crate::battery::read() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let (tell, told) = crate::battery::tell(read, this.battery_told);
                this.battery_told = told;
                if !tell || crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let full = read.map_or(0, |(full, _)| full);
                let this = cx.weak_entity();
                let note = Notification::info(format!("The battery is at {full}%. Power save mode stops the language servers, the background fetch, the pull request status and the load readout; File → Power Save Mode, or the battery in the title bar."))
                    .title("Save power?")
                    .autohide(false)
                    .action(move |_, _, cx| {
                        let (this, me) = (this.clone(), cx.entity().downgrade());
                        gpui_kit::component::button::Button::new("battery-save").xsmall().outline().label("Turn it on").on_click(move |_, window, cx| {
                            let _ = this.update(cx, |this, cx| {
                                if !crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed) {
                                    this.toggle_power_saving(cx);
                                }
                            });
                            let _ = me.update(cx, |note, cx| note.dismiss(window, cx));
                        })
                    });
                window.push_notification(note, cx);
            });
        })
        .detach();
    }

    pub fn toggle_power_saving(&mut self, cx: &mut Context<Self>) {
        let on = !crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed);
        crate::lsp::POWER_SAVING.store(on, std::sync::atomic::Ordering::Relaxed);
        for view in self.views() {
            view.update(cx, |t, cx| t.set_language_servers(!on, cx));
        }
        crate::predict::power_saving(on, cx);
        cx.notify();
    }

    fn check_build(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(exe) = self.build.check() else { return };
        cx.spawn_in(window, async move |this, cx| {
            let version = cx.background_spawn(async move { crate::build_watch::probe(exe) }).await;
            let _ = this.update(cx, |this, cx| {
                this.build.found(version);
                cx.notify();
            });
        })
        .detach();
    }

    /// What starts the agent of a task, and its session recorded: a task
    /// just made begins one (under an id the app chooses, when the agent
    /// takes one); another resumes its latest, by id when known, else the
    /// latest in its folder. The command line's agent goes over Settings;
    /// an agent's own worktree gets none, a shell for looking.
    fn launch_for(&mut self, path: &Path) -> Option<crate::agents::Launch> {
        use crate::agents::{Launch, Start};
        let (pi, ti) = self.find(path)?;
        let project_agent = self.projects[pi].values().agent.clone();
        let t = &mut self.projects[pi].tasks[ti];
        if t.side.is_some() {
            return None;
        }
        // The variants of a task: the same question in every one of them is what nobody sits through.
        let unasked = !t.group.is_empty();
        let profile = match &self.agent_override {
            Some(None) => return None,
            Some(Some(command)) => crate::agents::Profile { id: String::new(), ..crate::agents::Profile::from_command(command) },
            None => {
                let (agents, default) = crate::agents::profiles_for(&t.path);
                let last = t.sessions.last().map(|s| s.profile.clone()).unwrap_or_default();
                crate::agents::resolve(&agents, &[&last, &t.agent], Some(&project_agent), &default)?.clone()
            }
        };
        let same = t.sessions.last().is_some_and(|s| s.profile == profile.id);
        let start = if t.fresh && same {
            // Recorded already and never run (Continue with another agent).
            Start::New(t.sessions.last().and_then(|s| s.id.clone()))
        } else if same && t.asked > 0 && t.sessions.last().is_some_and(|s| s.id.is_none()) {
            // The latest session in the folder may be a question's.
            Start::New(None)
        } else if same {
            crate::agents::start_of(t.sessions.last())
        } else if !t.fresh && t.sessions.is_empty() && t.asked == 0 {
            // A task of before sessions were kept: as then, and its id is learned from the agent.
            t.sessions.push(state::AgentSession { profile: profile.id.clone(), id: None, started: state::now() });
            Start::Last
        } else {
            let id = profile.adapter().chooses_session_id().then(crate::agents::uuid);
            t.sessions.push(state::AgentSession { profile: profile.id.clone(), id: id.clone(), started: state::now() });
            Start::New(id)
        };
        t.fresh = false;
        Some(Launch { unasked, options: t.choices.clone(), ..Launch::new(profile, start, crate::hosts::split(path).map(|(host, _)| host)) })
    }

    /// New settings: they go to the state, and what they change now
    /// changes (new tasks' agent, language servers of open files).
    pub fn apply_settings(&mut self, mut new: state::Settings, cx: &mut Context<Self>) {
        let old = state::settings();
        let licence = new.lsp_licence.take().map(|key| key.trim().to_string()).filter(|key| key != crate::agents::MASK);
        let licence_changed = licence.as_ref().is_some_and(|key| crate::secrets::get(crate::lsp::INTELEPHENSE_LICENCE).as_ref() != Some(key));
        match licence.as_deref() {
            Some("") => crate::secrets::remove(crate::lsp::INTELEPHENSE_LICENCE),
            Some(key) => crate::secrets::set(crate::lsp::INTELEPHENSE_LICENCE, key),
            None => {}
        }
        state::set_settings(new.clone());
        self.resolve_projects(cx);
        // The keys of profiles that are gone.
        for scope in crate::secrets::scopes() {
            if scope.starts_with("lsp:") || crate::integrations::secret_store::is_token_scope(&scope) {
                continue;
            }
            let (profile, name) = scope.split_once('/').unwrap_or_default();
            if !new.agents.iter().any(|p| p.id == profile && p.secrets.contains(name)) {
                crate::secrets::remove(&scope);
            }
        }
        if !cfg!(test) {
            gpui_kit::base::input::HUMP_WORDS.store(new.hump_words, std::sync::atomic::Ordering::Relaxed);
        }
        if (old.theme.as_str(), old.dark_theme()) != (new.theme.as_str(), new.dark_theme()) {
            theme::choose(&new.theme, new.dark_theme(), cx);
            theme::apply_everywhere(self.appearance, cx);
        }
        if old.inlay_hints != new.inlay_hints || old.blame_at_line_end != new.blame_at_line_end {
            for view in self.views() {
                view.update(cx, |t, cx| t.code_settings_changed(cx));
            }
        }
        if old.notices_at != new.notices_at {
            theme::place_notices(cx);
        }
        if old.code_font != new.code_font {
            theme::set_code_font(&new.code_font, cx);
        }
        if old.ligatures != new.ligatures {
            theme::set_ligatures(new.ligatures, cx);
        }
        if (old.terminal_letter_spacing, old.terminal_line_height) != (new.terminal_letter_spacing, new.terminal_line_height) {
            let appearance = theme::terminal_appearance(&new);
            for view in self.views() {
                view.update(cx, |t, cx| t.set_terminal_appearance(&appearance, cx));
            }
        }
        if old.gutter_since_base != new.gutter_since_base {
            for view in self.views() {
                view.update(cx, |t, cx| t.reload_gutters(cx));
            }
        }
        if old.diff_ignore_whitespace != new.diff_ignore_whitespace {
            cx.set_global(crate::task::DiffWhitespace);
        }
        if old.keys != new.keys {
            crate::keymap::rebind(&old, cx);
            // Out of this window's update: the menus read the window.
            cx.defer(crate::menu::install);
        }
        if old.agent_lsp != new.agent_lsp {
            if let Some(bridge) = &self._bridge {
                let _ = bridge.write_settings();
            }
        }
        if old.lsp_off != new.lsp_off || old.lsp_choice != new.lsp_choice || old.lsp_separate != new.lsp_separate || licence_changed {
            for view in self.views() {
                view.update(cx, |t, cx| {
                    t.set_language_servers(false, cx);
                    if !crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed) {
                        t.set_language_servers(true, cx);
                    }
                });
            }
        }
        crate::predict::settings_changed(&old, &new, cx);
        self.save_later(cx);
        cx.notify();
    }

    /// Shows the task at `path` with its agent in front.
    pub fn focus_task(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pi, ti)) = self.find(path) else { return };
        // A group that is done is looked at as a whole; a variant that asks
        // something is answered in its own agent.
        let entry = &self.projects[pi].tasks[ti];
        let asks = entry.view.as_ref().is_some_and(|v| matches!(v.read(cx).agent.status, agent::AgentStatus::NeedsInput(_)));
        if !entry.group.is_empty() && !asks {
            let group = entry.group.clone();
            return self.open_group(&group, window, cx);
        }
        self.switch(path, window, cx);
        if let Some(view) = self.active_view() {
            view.update(cx, |t, cx| t.show_view(0, window, cx));
        }
    }

    pub fn placed_at(&mut self, origin: Option<Point<Pixels>>) {
        self.requested_origin = origin.map(|o| (o, std::time::Instant::now()));
    }

    /// The window's ×: the window goes, the daemon's agents and terminals
    /// stay for the next one.
    pub fn close_window(&mut self, cx: &mut Context<Self>) {
        #[cfg(unix)]
        if crate::daemon::client::get().is_some() {
            self.keep_terminals = true;
        }
        // Other machines' agents go on there; the links go.
        #[cfg(unix)]
        for daemon in crate::daemon::client::remotes() {
            daemon.disconnect();
        }
        self.save(cx);
        cx.quit();
    }

    /// What every open task's databases would lose.
    fn sql_at_risk(&self, cx: &App) -> Vec<String> {
        self.views().iter().flat_map(|v| v.read(cx).sql_at_risk(cx)).collect()
    }

    /// Asks before `then` when a transaction is open or cell changes wait
    /// anywhere; true when it asked, and `then` waits for the answer.
    pub fn ask_about_sql(&mut self, window: &mut Window, cx: &mut Context<Self>, then: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static) -> bool {
        let files = self.sql_at_risk(cx);
        self.ask_losing_sql(files, window, cx, then)
    }

    pub(crate) fn ask_losing_sql(&mut self, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>, then: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static) -> bool {
        if files.is_empty() {
            return false;
        }
        let this = cx.weak_entity();
        let then = Rc::new(then);
        let description = format!("{}: an uncommitted transaction or cell changes not saved will be lost.", files.join(", "));
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (this, then) = (this.clone(), then.clone());
            dialog.confirm().title("Lose database changes?").description(description.clone()).ok_text("Lose them").on_ok(move |_, window, cx| {
                let then = then.clone();
                let _ = this.update(cx, |w, cx| then(w, window, cx));
                true
            })
        });
        true
    }

    /// The window's ×, from the menu bar: without a daemon to keep the
    /// agents, that is a quit, and asks as one.
    pub fn close_or_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(unix)]
        if crate::daemon::client::get().is_some() {
            if !self.ask_about_sql(window, cx, |w, _, cx| w.close_window(cx)) {
                self.close_window(cx);
            }
            return;
        }
        self.request_quit(window, cx)
    }

    /// Exit: ends every agent and terminal, after a word when any run.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ask_about_sql(window, cx, |w, _, cx| {
            w.keep_terminals = false;
            w.save(cx);
            cx.quit();
        }) {
            return;
        }
        let running = self.views().len();
        if running == 0 {
            self.save(cx);
            return cx.quit();
        }
        let busy = self.views().iter().filter(|v| v.read(cx).agent_busy() || v.read(cx).question_busy()).count();
        let mut description = format!(
            "Stops the agents and terminals of {running} running task{}",
            if running == 1 { "" } else { "s" }
        );
        if busy > 0 {
            description.push_str(&format!(", {busy} of them working right now"));
        }
        description.push_str(". Closing the window (×) keeps them running instead.");
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog.confirm().title("Quit ideae?").description(description.clone()).ok_text("Quit").on_ok(move |_, _, cx| {
                let _ = this.update(cx, |w, cx| {
                    w.keep_terminals = false;
                    w.save(cx);
                    cx.quit();
                });
                true
            })
        });
    }

    /// A newer release, or new commits for the checkout this was built
    /// from. `asked`: from the menu, so the answer is told either way.
    pub fn check_update(&mut self, asked: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !asked && (!state::settings().check_updates || crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed)) {
            return;
        }
        let tell = move |window: &mut Window, cx: &mut App, said: String, error: bool| {
            if asked {
                let n = if error { Notification::error(said) } else { Notification::info(said) };
                window.push_notification(n.title("Updates"), cx);
            }
        };
        if crate::update::enabled() {
            cx.spawn_in(window, async move |this, cx| {
                let found = cx.background_spawn(crate::procs::on_thread(crate::update::newer)).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if found.is_none() {
                        tell(window, cx, format!("{} is the latest release.", crate::build_watch::version()), false);
                    }
                    this.update = found;
                    cx.notify();
                });
            })
            .detach();
        } else if let Some(dir) = crate::update::source() {
            cx.spawn_in(window, async move |this, cx| {
                let found = cx.background_spawn(crate::procs::on_thread(move || crate::update::behind(&dir))).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    match &found {
                        Ok(None) => tell(window, cx, format!("This build has all of main ({}).", crate::build_watch::version()), false),
                        Ok(Some(b)) => tell(window, cx, format!("{} new commits on {}: ↓ in the title bar pulls and builds them.", b.commits, b.base), false),
                        Err(e) => tell(window, cx, e.clone(), true),
                    }
                    if let Ok(behind) = found {
                        this.behind = behind;
                    }
                    cx.notify();
                });
            })
            .detach();
        } else {
            tell(
                window,
                cx,
                "This build updates neither from a release nor from a checkout: the folder it was built in is gone, or it is a debug build.".into(),
                false,
            );
        }
    }

    /// Pulls the new commits into the checkout and builds them over this
    /// binary: the new-build notice then offers the restart.
    pub fn rebuild_from_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = crate::update::source() else { return };
        if self.rebuilding {
            return;
        }
        self.rebuilding = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(crate::procs::on_thread(move || crate::update::rebuild(&dir))).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.rebuilding = false;
                match result {
                    Ok(()) => {
                        this.behind = None;
                        window.push_notification(Notification::info("Built. ↻ new build in the title bar restarts into it.").title("Update"), cx);
                    }
                    Err(e) => window.push_notification(Notification::error(e).title("Update"), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Downloads the newer release over this binary and restarts into it.
    pub fn install_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(release) = self.update.clone() else { return };
        if self.updating {
            return;
        }
        self.updating = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(crate::procs::on_thread(move || { crate::update::install(&release) })).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.updating = false;
                match result {
                    Ok(_) => this.restart(window, cx),
                    Err(e) => window.push_notification(Notification::error(e).title("Update"), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.restarting {
            return;
        }
        self.restarting = true;
        cx.notify();
        self.save(cx);
        self.keep_terminals = true;
        cx.spawn_in(window, async move |this, cx| {
            let started = cx.background_spawn(crate::procs::on_thread(crate::desktop::spawn_again)).await;
            let _ = this.update_in(cx, |this, window, cx| match started {
                Ok(()) => cx.quit(),
                Err(e) => {
                    this.keep_terminals = false;
                    this.restarting = false;
                    cx.notify();
                    window.push_notification(Notification::error(e.to_string()).title("Restart"), cx)
                }
            });
        })
        .detach();
    }

    /// Draws with a theme at once, and keeps it as the one set for the side
    /// drawn now, light or dark: for the list that shows each as it is gone
    /// through.
    pub fn use_theme(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut settings = state::settings();
        let dark = cx.theme().mode.is_dark();
        let (mut light_side, mut dark_side) = (settings.theme.clone(), settings.dark_theme().to_string());
        *if dark { &mut dark_side } else { &mut light_side } = name.to_string();
        theme::choose(&light_side, &dark_side, cx);
        theme::apply(self.appearance, window, cx);
        if (settings.theme.as_str(), settings.dark_theme()) != (light_side.as_str(), dark_side.as_str()) {
            settings.dark_theme = if dark_side == light_side { String::new() } else { dark_side };
            settings.theme = light_side;
            state::set_settings(settings);
            self.save_later(cx);
        }
        cx.notify();
    }

    /// The themes in a list: each shows as it is gone through; Enter
    /// keeps the one reached, Esc goes back to the one there was.
    pub fn theme_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::command::{Command, CommandItem, CommandState};
        let before = theme::family(cx);
        let command = cx.new(|cx| CommandState::new(window, cx));
        let this = cx.weak_entity();
        let focused = Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, _| {
            let (command, this, focused) = (command.clone(), this.clone(), focused.clone());
            dialog.close_button(false).p_0().w(px(360.)).content(move |content, window, cx| {
                if !focused.replace(true) {
                    let command = command.clone();
                    window.defer(cx, move |window, cx| command.read(cx).focus_handle(cx).focus(window, cx));
                }
                let now = theme::family(cx);
                let (shown, back) = (this.clone(), this.clone());
                content.child(
                    Command::new(&command)
                        .bordered(false)
                        .placeholder("Theme…")
                        .items(theme::FAMILIES.iter().map(|family| CommandItem::new().label(family.name).checked(family.name == now)))
                        .on_select(move |index, window, cx| {
                            if let Some(family) = theme::FAMILIES.get(index.row) {
                                let _ = shown.update(cx, |w, cx| w.use_theme(family.name, window, cx));
                            }
                        })
                        .on_confirm(|_, window, cx| window.close_dialog(cx))
                        .on_cancel(move |window, cx| {
                            let _ = back.update(cx, |w, cx| w.use_theme(before, window, cx));
                        }),
                )
            })
        });
    }

    pub fn set_appearance(&mut self, appearance: Appearance, window: &mut Window, cx: &mut Context<Self>) {
        self.appearance = appearance;
        theme::apply(appearance, window, cx);
        self.save_later(cx);
        cx.notify();
    }
}

/// "DEV-123" at the start of a title.
/// Writes the state when it is not what was written last, saves in the
/// order they were asked for.
/// The store first (crate::machine), then state.json with its rev.
/// `snapshot`: the state, and where the model was when it was taken (crate::machine::taken_in).
fn write_state(last: &std::sync::Mutex<(u64, String)>, seq: u64, (mut snapshot, taken_in, aside): (State, u64, Vec<PathBuf>)) {
    let mut last = last.lock().unwrap();
    if seq < last.0 {
        return;
    }
    last.0 = seq;
    crate::machine::sync(&mut snapshot, taken_in, &aside);
    let json = serde_json::to_string_pretty(&snapshot).unwrap_or_default();
    if json != last.1 {
        match state::save(&json) {
            Ok(()) => last.1 = json,
            Err(e) => eprintln!("saving state: {e}"),
        }
    }
}

/// The repository a path is in: its main worktree, or the path itself.
fn project_root(path: &Path) -> Option<PathBuf> {
    let path = std::fs::canonicalize(path).ok()?;
    Some(git::main_worktree(&path).unwrap_or(path))
}

/// Lets what else waits for the UI thread (a frame, input) go first.
fn yield_now() -> impl std::future::Future<Output = ()> {
    let mut yielded = false;
    std::future::poll_fn(move |cx| {
        if std::mem::replace(&mut yielded, true) {
            return std::task::Poll::Ready(());
        }
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    })
}

/// What a new task's agent is told of the ticket its title names.
fn told_of(found: &crate::integrations::tickets::Found, keyed_history: bool) -> String {
    let what = match found.ticket.kind {
        Some(kind) => format!("{} issue", kind.label()),
        None => "Ticket".to_string(),
    };
    let key = &found.ticket.key;
    let told = match &found.ticket.url {
        Some(url) => format!("{what} {key} ({url}): read it, its description and comments, and do what it asks."),
        None => format!("{what} {key}: read it, its description and comments, and do what it asks."),
    };
    // Where the project's commits carry keys, its tracker finds them by it.
    if keyed_history && crate::integrations::tickets::is_key(key) { format!("{told} Start each commit's subject with {key}.") } else { told }
}

/// An issue's title and body, asked of the project's code host (GitHub,
/// GitLab) from here: its repository read where it is, here or on its host.
fn repo_issue(repo: &Path, number: u64) -> Option<(String, String)> {
    crate::integrations::code_host(repo).and_then(|host| host.issue(repo, number)).ok()
}

/// A task of the same ticket as the one being made: open, or archived.
#[derive(Clone)]
enum Existing {
    Open(String, PathBuf, String),
    Archived(String, crate::archive::Archived),
}

/// Under a new task's title: the ticket has a task already; go to it, or
/// restore it. Making a new one anyway is the dialog's own button.
fn existing_row(same: Existing, this: WeakEntity<Workspace>, cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let (said, button): (String, Button) = match same {
        Existing::Open(key, path, label) => (
            format!("{key} has a task already: {label}."),
            Button::new("same-ticket-switch").xsmall().outline().label("Switch to it").on_click(move |_, window, cx| {
                window.close_dialog(cx);
                let _ = this.update(cx, |w, cx| w.switch(&path, window, cx));
            }),
        ),
        Existing::Archived(key, item) => (
            format!("{key} was worked on in an archived task: {}.", item.meta.title),
            Button::new("same-ticket-restore").xsmall().outline().label("Restore…").on_click(move |_, window, cx| {
                window.close_dialog(cx);
                let item = item.clone();
                let _ = this.update(cx, |w, cx| w.restore_dialog(item, window, cx));
            }),
        ),
    };
    h_flex()
        .id("same-ticket")
        .test_support()
        .gap_2()
        .child(div().flex_1().text_xs().text_color(muted).child(said))
        .child(button)
}

/// origin's branches as last fetched: by name, and by the ticket keys in
/// their names, the latest first.
#[derive(Default)]
struct OnOrigin {
    named: std::collections::HashMap<String, git::OriginBranch>,
    keyed: std::collections::HashMap<String, Vec<String>>,
}

impl OnOrigin {
    fn read(repo: &Path) -> Self {
        Self::from_branches(git::origin_branches(repo))
    }

    fn from_branches(branches: Vec<git::OriginBranch>) -> Self {
        let mut on = Self::default();
        for branch in branches {
            for key in crate::integrations::tickets::keys_in(&branch.name) {
                on.keyed.entry(key).or_default().push(branch.name.clone());
            }
            on.named.insert(branch.name.clone(), branch);
        }
        on
    }
}

fn branch_hint(title: &str, template: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        "The branch name comes from the title.".into()
    } else if crate::integrations::tickets::key_at_start(title) == Some(title) {
        format!("Branch: {}. The title is named from what its agent reads in the issue", naming::preview(title, template))
    } else {
        let branch = naming::preview(title, template);
        format!("Branch: {branch}")
    }
}

/// What origin has of the branch a title makes, told apart from the branch's
/// line: that branch, which the task then starts from; or others of its key.
/// With what to look for in Review… instead: someone's work is reviewed there.
fn origin_note(title: &str, template: &str, origin: &OnOrigin) -> Option<(String, String)> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let branch = naming::preview(title, template);
    let by = |b: &git::OriginBranch| format!("last commit by {}, {}", b.author, b.when);
    if let Some(there) = origin.named.get(&branch) {
        return Some((format!("{branch} is on origin already ({}): Create starts the task on it, from its commits.", by(there)), branch));
    }
    let key = crate::integrations::tickets::key_at_start(title)?;
    let others: Vec<&git::OriginBranch> = origin.keyed.get(key).into_iter().flatten().filter_map(|name| origin.named.get(name)).collect();
    let first = others.first()?;
    let more = match others.len() - 1 {
        0 => String::new(),
        n => format!(" and {n} more"),
    };
    Some((format!("{key} has {} on origin ({}){more}: Create makes {branch} beside it.", first.name, by(first)), key.to_string()))
}

use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;

#[cfg(test)]
mod origin_hint_tests {
    use super::{OnOrigin, branch_hint, origin_note};
    use crate::git::OriginBranch;

    /// What origin has of the title's branch, apart from the branch's line:
    /// that branch, which Create starts on; else others of its key.
    #[test]
    fn the_hint_says_what_origin_has_of_the_branch() {
        let branch = |name: &str| OriginBranch { name: name.into(), author: "Someone".into(), when: "2 days ago".into() };
        let origin = OnOrigin::from_branches(vec![branch("DEV-7"), branch("DEV-7_more"), branch("DEV-8_fixLogin")]);
        let template = crate::naming::DEFAULT_TEMPLATE;
        assert_eq!(branch_hint("DEV-7", template), "Branch: DEV-7. The title is named from what its agent reads in the issue");
        assert_eq!(
            origin_note("DEV-7", template, &origin),
            Some(("DEV-7 is on origin already (last commit by Someone, 2 days ago): Create starts the task on it, from its commits.".into(), "DEV-7".into()))
        );
        assert!(origin_note("DEV-8 Fix login", template, &origin).is_some_and(|(said, find)| said.starts_with("DEV-8_fixLogin is on origin already") && find == "DEV-8_fixLogin"));
        assert_eq!(
            origin_note("DEV-8", template, &origin),
            Some(("DEV-8 has DEV-8_fixLogin on origin (last commit by Someone, 2 days ago): Create makes DEV-8 beside it.".into(), "DEV-8".into()))
        );
        assert_eq!(origin_note("Plain words", template, &origin), None);
        assert_eq!(branch_hint("Plain words", template), "Branch: plain-words-…");
        assert_eq!(origin_note("DEV-9 Fix", template, &OnOrigin::default()), None);
        // The branch Create makes, by the project's template, a key alone too.
        let feature = OnOrigin::from_branches(vec![branch("feature/DEV-7"), branch("DEV-8")]);
        assert_eq!(branch_hint("DEV-7", "feature/{key}-{slug}"), "Branch: feature/DEV-7. The title is named from what its agent reads in the issue");
        assert!(origin_note("DEV-7", "feature/{key}-{slug}", &feature).is_some_and(|(said, _)| said.starts_with("feature/DEV-7 is on origin already")));
        assert!(origin_note("DEV-8", "feature/{key}-{slug}", &feature).is_some_and(|(said, _)| said.ends_with("Create makes feature/DEV-8 beside it.")));
    }
}

// They script a repository with bash, which Windows lacks or has as WSL's.
#[cfg(all(test, unix))]
mod in_the_way_tests {
    use super::{TaskEntry, Workspace};
    use crate::state::{ProjectState, State};
    use gpui_kit::{AppContext as _, Bounds, Point, TestAppContext, WindowBounds, WindowOptions, px, size};

    /// A task already on the branch a new one would make keeps its row: the
    /// new one is refused before git. One git fails takes only its own row,
    /// the one still being made, whichever else is at its path.
    #[gpui_kit::test]
    fn a_new_task_leaves_the_row_of_one_there(cx: &mut TestAppContext) {
        let _one = super::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ideae-in-the-way-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ok = std::process::Command::new("bash")
            .arg("-ec")
            .arg("git init -q -b main repo; cd repo; git config user.email t@t; git config user.name t; echo one > f.txt; git add f.txt; git commit -qm base")
            .current_dir(&dir)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let repo = dir.join("repo");
        // SAFETY: set before anything of the app reads them, one window at a time.
        unsafe {
            std::env::set_var("IDEAE_STATE", dir.join("state.json"));
            std::env::set_var("IDEAE_NO_DAEMON", "1");
        }
        let saved = State { projects: vec![ProjectState { path: repo.clone(), ..Default::default() }], ..Default::default() };
        std::fs::write(dir.join("state.json"), serde_json::to_string(&saved).unwrap()).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_cn::init(cx);
            crate::theme::init(cx);
        });
        let (handle, workspace) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1000.), px(700.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(Vec::new(), Some(None), None, window, cx))).expect("a test window")
        });
        let path = crate::git::task_worktree_dir(&repo, "DEV-7");
        let rows = |cx: &mut TestAppContext| workspace.update(cx, |w, _| w.projects[0].tasks.iter().filter(|t| !t.main).map(|t| (t.title.clone(), t.busy.is_some())).collect::<Vec<_>>());
        // A real worktree: the list keeps no row of a task git has none of.
        let made = std::process::Command::new("git").arg("-C").arg(&repo).args(["worktree", "add", "-q", "-b", "DEV-7"]).arg(&path).status().unwrap().success();
        assert!(made);
        cx.run_until_parked();
        workspace.update(cx, |w, _| {
            let tasks = &mut w.projects[0].tasks;
            match tasks.iter_mut().find(|t| t.path == path) {
                Some(t) => t.title = "DEV-7 paused".into(),
                None => tasks.push(TaskEntry { path: path.clone(), branch: "DEV-7".into(), title: "DEV-7 paused".into(), ..Default::default() }),
            }
        });
        cx.update_window(handle.into(), |_, window, cx| {
            workspace.update(cx, |w, cx| w.create_worktree_task(0, "DEV-7".into(), String::new(), vec![String::new()], Vec::new(), false, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(rows(cx), [("DEV-7 paused".to_string(), false)], "refused: one task of the branch, the one there");
        // Its folder there already, of no task: git refuses, its row goes.
        std::fs::create_dir_all(crate::git::task_worktree_dir(&repo, "DEV-8")).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            workspace.update(cx, |w, cx| w.create_worktree_task(0, "DEV-8".into(), String::new(), vec![String::new()], Vec::new(), false, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(rows(cx), [("DEV-7 paused".to_string(), false)], "failed: its own row gone, the other kept");
        // One made at its path anyway (by an older build) and failed: its row goes, not the other.
        workspace.update(cx, |w, _| {
            w.projects[0].tasks.push(TaskEntry { path: path.clone(), branch: "DEV-7".into(), title: "DEV-7".into(), busy: Some("creating…".into()), ..Default::default() });
            w.drop_unmade(&path);
        });
        assert_eq!(rows(cx), [("DEV-7 paused".to_string(), false)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod ticket_tests {
    #[cfg_attr(not(unix), allow(unused_imports))]
    use super::{ProjectState, State, Workspace, state};

    /// A task titled by a ticket's key alone is named after its agent's
    /// first turn, by the agent for quick questions: its title only, the
    /// branch left to its agent and the project's rules.
    ///
    /// `cargo test -- --ignored a_ticket_task_is_named` after changing it.
    #[cfg(unix)]
    #[gpui_kit::test]
    #[ignore]
    fn a_ticket_task_is_named_by_its_agent(cx: &mut gpui_kit::TestAppContext) {
        use crate::agent::HookEvent;
        use crate::agents::Profile;
        use gpui_kit::{AppContext as _, Bounds, Point, WindowBounds, WindowOptions, px, size};
        use std::os::unix::fs::PermissionsExt as _;
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("ideae-ticket-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let sh = |script: &str| assert!(std::process::Command::new("sh").arg("-ec").arg(script).current_dir(&dir).status().unwrap().success(), "{script}");
        sh("git init -q --bare -b main remote.git; git init -q -b main repo; cd repo; git config user.email t@t; git config user.name t; echo one > f.txt; git add f.txt; git commit -qm base; git remote add origin \"$(cd ../remote.git && pwd)\"; git push -q -u origin main; git remote set-head origin main");
        let repo = dir.join("repo").canonicalize().unwrap();
        // Claude Code's stand-in: asked once (-p), it names the task; else it waits.
        let claude = dir.join("bin").join("claude");
        std::fs::write(&claude, "#!/bin/sh\nif [ \"$1\" = -p ]; then cat >/dev/null; echo 'Fix login'; exit 0; fi\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: set before anything of the app reads them.
        unsafe {
            std::env::set_var("IDEAE_STATE", dir.join("state.json"));
            std::env::set_var("IDEAE_NO_DAEMON", "1");
        }
        let saved = State { projects: vec![ProjectState { path: repo.clone(), ..Default::default() }], ..Default::default() };
        std::fs::write(dir.join("state.json"), serde_json::to_string(&saved).unwrap()).unwrap();
        let mut settings = state::Settings { notifications: false, pr_status: false, ..Default::default() };
        settings.agents = vec![Profile { command: claude.display().to_string(), ..Profile::claude_code() }];
        state::set_settings(settings);
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_cn::init(cx);
            crate::theme::init(cx);
        });
        let (handle, workspace) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1300.), px(800.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(Vec::new(), None, None, window, cx))).expect("a test window")
        });
        let step = |cx: &mut gpui_kit::TestAppContext, act: &mut dyn FnMut(&mut Workspace, &mut gpui_kit::Window, &mut gpui_kit::Context<Workspace>)| {
            cx.update_window(handle.into(), |_, window, cx| workspace.update(cx, |w, cx| act(w, window, cx))).unwrap();
            cx.run_until_parked();
        };
        let wait = |cx: &mut gpui_kit::TestAppContext, what: &str, done: &mut dyn FnMut(&mut gpui_kit::TestAppContext) -> bool| {
            for _ in 0..1500 {
                cx.run_until_parked();
                if done(cx) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                cx.executor().advance_clock(std::time::Duration::from_millis(100));
            }
            panic!("waited in vain for {what}");
        };
        let made = |cx: &mut gpui_kit::TestAppContext, key: &str| -> std::path::PathBuf {
            let key = key.to_string();
            step(cx, &mut |w, window, cx| w.create_task(0, key.clone(), String::new(), vec![String::new()], Vec::new(), window, cx));
            let mut path = None;
            wait(cx, "the task", &mut |cx| {
                path = workspace.update(cx, |w, _| w.projects[0].tasks.iter().find(|t| t.title == key && t.busy.is_none() && t.view.is_some()).map(|t| t.path.clone()));
                path.is_some()
            });
            path.unwrap()
        };
        let turn = |cx: &mut gpui_kit::TestAppContext, task: &std::path::Path| {
            for (event, said) in [("UserPromptSubmit", None), ("Stop", Some("The issue asks to fix the login form."))] {
                let hook = HookEvent { task: task.to_path_buf(), agent: None, event: event.into(), message: None, kind: None, file: None, said: said.map(str::to_string), replayed: None, session: None, guessed: false, worktree: None, call: None };
                step(cx, &mut |w, window, cx| w.on_hook(hook.clone(), window, cx));
            }
        };
        let named = |cx: &mut gpui_kit::TestAppContext, task: &std::path::Path| workspace.update(cx, |w, _| w.projects[0].tasks.iter().find(|t| t.path == task).map(|t| (t.title.clone(), t.branch.clone())).unwrap());
        let head = |task: &std::path::Path| String::from_utf8(std::process::Command::new("git").args(["branch", "--show-current"]).current_dir(task).output().unwrap().stdout).unwrap().trim().to_string();

        let one = made(cx, "DEV-1");
        turn(cx, &one);
        wait(cx, "DEV-1 named", &mut |cx| named(cx, &one).0 == "DEV-1 Fix login");
        assert_eq!((named(cx, &one).1, head(&one)), ("DEV-1".to_string(), "DEV-1".to_string()), "the branch is not renamed under its agent");
        // Named once: a title of yours after it stays.
        step(cx, &mut |w, _, _| w.entry_mut(&one).unwrap().title = "DEV-1".into());
        turn(cx, &one);
        assert_eq!(named(cx, &one).0, "DEV-1");

        step(cx, &mut |w, _, cx| {
            for view in w.views() {
                view.update(cx, |t, cx| t.shut_down_at_quit(cx));
            }
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_ticket_s_words_name_its_branch() {
        let linear = "https://linear.app/acme/issue/ENG-123/fix-the-login-form";
        let found = crate::integrations::tickets::ticket_in(linear, &[]).unwrap();
        assert_eq!(crate::naming::branch_name(&format!("{} {}", found.ticket.key, found.words)), "ENG-123_fixTheLoginForm");
        assert_eq!(super::told_of(&found, false), format!("Linear issue ENG-123 ({linear}): read it, its description and comments, and do what it asks."));
    }
}
