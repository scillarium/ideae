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
    /// A worktree an agent made for itself (side.rs), not a task of the user's.
    pub side: Option<side::Side>,
    /// The agent chosen when it was made, by profile id; empty for its
    /// project's or the default.
    pub agent: String,
    /// Its agents' sessions; the last is the one it runs.
    pub sessions: Vec<state::AgentSession>,
    /// Made this run and not started yet, or its latest session recorded
    /// and not run: its agent starts a new session.
    pub fresh: bool,
    /// The archive it was restored from, and whether that one keeps work
    /// it did not get back (archive.rs).
    pub archive: String,
    pub archive_keep: bool,
}

pub mod agents_ui;
pub mod archive_ui;
mod merged_ui;
pub mod automation;
pub mod present;
pub mod side;
pub mod spaces;
pub mod variants;
pub mod welcome;

impl TaskEntry {
    /// The agent's latest session, to find its file by.
    pub fn session_ref(&self) -> Option<crate::agents::SessionRef> {
        let session = self.sessions.last()?;
        let profile = state::with_settings(|s| s.profile(&session.profile).cloned())?;
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
    /// The space it is sorted into; none, and it is in every one.
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
    /// The agent of its new tasks, by profile id; none for the default.
    pub agent: Option<String>,
    /// Branches of origin whose PR was merged, and when they were looked for.
    pub merged_branches: Vec<crate::merged_branches::MergedBranch>,
    pub merged_checked: Option<std::time::Instant>,
}

impl Project {
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
            agent: None,
            merged_branches: Vec::new(),
            merged_checked: None,
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
    /// In the GNOME dash; asked once, not while the menu opens.
    pub pinned: bool,
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
    _subscriptions: Vec<Subscription>,
    _tick: Task<()>,
    pub build: crate::build_watch::BuildWatch,
    /// Clicked notifications send their task here.
    open_task_tx: async_channel::Sender<PathBuf>,
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
    /// Quitting while the agents go on: a restart, or the window's ×.
    keep_terminals: bool,
    /// What each task changed since its fork, by worktree path: tasks of
    /// a project that touch the same files are marked before they clash.
    pub touched: std::collections::HashMap<PathBuf, std::collections::BTreeSet<String>>,
    /// Of those, the ones two tasks of a project both changed, by task.
    shared: std::collections::HashMap<PathBuf, Vec<(PathBuf, Vec<String>)>>,
    /// When each project's base branch was last fetched in the background.
    fetched: std::collections::HashMap<PathBuf, std::time::Instant>,
    /// The group of variants whose screen is in view, in place of a task.
    pub active_group: Option<String>,
    /// On the group's screen, the variant the menu bar's task items act on: the one clicked last.
    pub group_card: Option<PathBuf>,
    /// Groups folded in the sidebar.
    pub collapsed_groups: std::collections::HashSet<String>,
    /// The agents' worktrees shown under a task (or a project's path, for
    /// those in temporary places): folded until opened.
    pub open_sides: std::collections::HashSet<PathBuf>,
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
        saved: State,
        repos: Vec<PathBuf>,
        agent: Option<Option<String>>,
        appearance: Option<Appearance>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        state::init_settings(&saved.settings);
        cx.background_spawn(async { crate::lsp::look_for_servers() }).detach();
        let appearance = appearance
            .or_else(|| saved.appearance.as_deref().and_then(Appearance::parse))
            .unwrap_or(Appearance::Auto);
        theme::choose(&saved.settings.theme, cx);
        theme::apply(appearance, window, cx);
        crate::lsp::POWER_SAVING.store(saved.power_saving, std::sync::atomic::Ordering::Relaxed);
        gpui_kit::base::input::HUMP_WORDS.store(saved.settings.hump_words, std::sync::atomic::Ordering::Relaxed);
        if let Some(width) = saved.side_panel_width {
            cx.set_global(SidePanelWidth(px(width)));
        }
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
                event: h.event,
                message: h.message,
                kind: h.kind,
                file: h.file,
                said: h.said,
                session: h.session,
                guessed: false,
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
                    for view in this.views() {
                        view.update(cx, |t, cx| t.autosave_all(cx));
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
                if this.keep_terminals {
                    for view in this.views() {
                        view.update(cx, |t, cx| t.save_all_now(cx));
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
                    // Here, not on the tasks' threads: the app exits next.
                    if let Some(socket) = this.hook_socket.clone() {
                        crate::procs::end_task_strays(None, &socket, seen);
                    }
                }
                crate::procs::end_child_sessions();
                crate::browser::quit();
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
        let (open_task_tx, open_task_rx) = async_channel::unbounded::<PathBuf>();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(path) = open_task_rx.recv().await {
                if this.update_in(cx, |this, window, cx| {
                    window.activate_window();
                    this.focus_task(&path, window, cx);
                }).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Closing the window the window manager's way is the same as ×.
        let closing = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = closing.update(cx, |w, cx| w.close_window(cx));
            true
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
                    }
                    // Every six hours, the first time soon after start.
                    if n % 7200 == 3 {
                        this.check_update(window, cx);
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
                    if n % 10 == 0 {
                        this.sleep_idle_language_servers(cx);
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
                    // Once a day, the first soon after start.
                    if n % 28_800 == 5 {
                        this.clean_up_archives(cx);
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
            space: saved.space.clone().filter(|name| saved.spaces.iter().any(|s| &s.name == name)),
            last_in_space: Default::default(),
            agent_override,
            hook_socket,
            _bridge: bridge,
            appearance,
            sidebar_width: px(saved.sidebar_width.unwrap_or(260.)),
            sidebar_rail: saved.sidebar_rail,
            sidebar_peek: false,
            peek_wait: Task::ready(()),
            pinned: false,
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
            _subscriptions: subscriptions,
            _tick: tick,
            requested_origin: None,
            origin_offset: Point::default(),
            open_task_tx,
            window_place: saved.window.clone(),
            update: None,
            updating: false,
            keep_terminals: false,
            touched: Default::default(),
            shared: Default::default(),
            fetched: Default::default(),
            build: crate::build_watch::BuildWatch::new(),
            auto: Default::default(),
            archive_dir: crate::archive::root(),
        };
        this.restore(&saved, window, cx);
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
            if let Some(ix) = this.add_project(&repo) {
                first_cli.get_or_insert(ix);
            }
        }
        if this.active.is_none() {
            if let Some(ix) = first_cli {
                this.open_main(ix, window, cx);
            }
        }
        this.sync_worktrees(window, cx);
        if crate::desktop::can_pin() {
            cx.spawn(async move |this, cx| {
                let pinned = cx.background_spawn(async { crate::desktop::is_pinned() }).await;
                let _ = this.update(cx, |this, cx| {
                    this.pinned = pinned;
                    cx.notify();
                });
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
        if let (Ok(task), false) = (std::env::var("IDEAE_NEW_TASK"), this.projects.is_empty()) {
            let (title, prompt) = task.split_once('\n').unwrap_or((task.as_str(), ""));
            let variants = std::env::var("IDEAE_VARIANTS").ok().and_then(|n| n.parse::<usize>().ok()).unwrap_or(1).clamp(1, variants::MOST);
            // IDEAE_AGENTS="claude,codex": the agent of each, by profile id.
            let agents = std::env::var("IDEAE_AGENTS").map(|a| a.split(',').map(|id| id.trim().to_string()).collect()).unwrap_or_default();
            this.create_task(0, title.trim().to_string(), prompt.trim().to_string(), vec![String::new(); variants], agents, window, cx);
        }
        this
    }

    fn restore(&mut self, saved: &State, window: &mut Window, cx: &mut Context<Self>) {
        for p in &saved.projects {
            let mut project = Project::new(p.path.clone());
            project.space = p.space.clone().filter(|name| saved.spaces.iter().any(|s| &s.name == name));
            project.collapsed = p.collapsed;
            project.check = p.check.clone();
            project.check_auto = p.check_auto;
            project.trusted = p.trusted.clone();
            project.hints_off = p.hints_off.iter().cloned().collect();
            project.agent = p.agent.clone();
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
            project.tasks = p
                .tasks
                .iter()
                .map(|t| TaskEntry {
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
                    side: None,
                    agent: t.agent.clone(),
                    sessions: t.sessions.clone(),
                    fresh: false,
                    archive: t.archive.clone(),
                    archive_keep: t.archive_keep,
                })
                .collect();
            self.projects.push(project);
        }
        let open: Vec<PathBuf> = saved
            .projects
            .iter()
            .flat_map(|p| &p.tasks)
            .filter(|t| t.open && t.path.exists())
            .map(|t| t.path.clone())
            .collect();
        // The one in view now; the rest after the window is up (`restoring`).
        self.restoring = open.into_iter().rev().collect();
        if let Some(active) = saved.active_task.as_ref().filter(|p| self.find(p).is_some()) {
            self.restoring.retain(|p| p != active);
            self.switch(active, window, cx);
        }
    }

    // ------------------------------------------------------------ lookup

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

    fn snapshot(&self, cx: &App) -> State {
        State {
            appearance: Some(self.appearance.label().to_lowercase()),
            sidebar_width: Some(f32::from(self.sidebar_width)),
            sidebar_rail: self.sidebar_rail,
            side_panel_width: cx.try_global::<SidePanelWidth>().map(|w| f32::from(w.0)),
            active_collapsed: self.active_collapsed,
            starred_collapsed: self.starred_collapsed,
            presenting: self.presenting.as_ref().map(|p| state::PresentingState { kept: p.kept.iter().cloned().collect(), since: p.since }),
            spaces: self.spaces.clone(),
            space: self.space.clone(),
            power_saving: crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed),
            window: self.window_place.clone(),
            settings: state::settings(),
            active_task: self.active.clone(),
            zoom: crate::zoom::current(cx),
            projects: self
                .projects
                .iter()
                .map(|p| ProjectState {
                    path: p.path.clone(),
                    space: p.space.clone(),
                    collapsed: p.collapsed,
                    check: p.check.clone(),
                    check_auto: p.check_auto,
                    own: crate::project_config::own(&p.path),
                    trusted: p.trusted.clone(),
                    hints_off: p.hints_off.iter().cloned().collect(),
                    agent: p.agent.clone(),
                    verdicts: self
                        .judges
                        .iter()
                        .filter(|(group, run)| run.is_over() && p.tasks.iter().any(|t| &t.group == *group))
                        .map(|(group, run)| (group.clone(), run.clone()))
                        .collect(),
                    stale_verdicts: self.stale_verdicts.iter().filter(|group| p.tasks.iter().any(|t| &t.group == *group)).cloned().collect(),
                    tasks: p
                        .tasks
                        .iter()
                        // One being archived is kept till it is: the app may quit before.
                        .filter(|t| t.busy.is_none() || t.view.is_some() || !t.group.is_empty() || t.busy.as_deref() == Some(archive_ui::ARCHIVING))
                        .map(|t| TaskState {
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
                            sessions: t.sessions.clone(),
                            archive: t.archive.clone(),
                            archive_keep: t.archive_keep,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn serialize(&self, cx: &App) -> String {
        serde_json::to_string_pretty(&self.snapshot(cx)).unwrap_or_default()
    }

    /// On disk when it returns: before the window or the app goes.
    pub fn save(&mut self, cx: &App) {
        let json = self.serialize(cx);
        self.save_seq += 1;
        write_state(&self.last_saved, self.save_seq, json);
    }

    /// The now-and-then save: what it takes from the UI thread is the copy
    /// of the state; turning it into text and writing it happen off it.
    fn save_later(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        self.save_seq += 1;
        let (seq, last) = (self.save_seq, self.last_saved.clone());
        cx.background_spawn(async move { write_state(&last, seq, serde_json::to_string_pretty(&snapshot).unwrap_or_default()) }).detach();
    }

    // ------------------------------------------------------------ tasks

    /// Starts a task: its terminals, agent and watchers.
    fn load(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TaskView>> {
        self.restoring.retain(|p| p != path);
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
        }
        let entry = self.entry_mut(path)?;
        let (root, branch, session, pr) = (entry.path.clone(), entry.branch.clone(), entry.session.clone(), entry.pr.clone());
        let fill = self.usage.get(path).map_or(0., |(_, usage)| usage.fill());
        let view = cx.new(|cx| {
            let mut t = TaskView::new(root, branch, launch, socket, slot, window, cx);
            t.context_fill = fill;
            // Servers start when the task comes into view (switch).
            t.lsp_asleep = true;
            t.restore(&session, window, cx);
            t.pr = pr;
            t
        });
        cx.subscribe_in(&view, window, |this, view, event: &TaskEvent, window, cx| this.on_task_event(view, event, window, cx)).detach();
        self.entry_mut(path)?.view = Some(view.clone());
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
            t.set_visible(true, window, cx)
        });
        self.refresh_prs(true, window, cx);
        cx.notify();
    }

    /// Asks `gh` about the PRs of tasks: running ones every two minutes,
    /// the rest every ten, merged and closed ones hourly; `now` for the
    /// task just shown.
    fn refresh_prs(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::pr::PrState;
        // Not while saving power: each asks GitHub. What is known stays on the rows.
        if !state::settings().pr_status || crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        use std::time::Duration;
        let mut due = Vec::new();
        for p in &mut self.projects {
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
                }
            }
        }
        if due.is_empty() {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(crate::procs::on_thread(move || {
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
            let _ = this.update(cx, |this, cx| {
                for (path, pr) in found {
                    // A PR merged since: its branch may be left on origin.
                    let merged = |pr: &Option<crate::pr::Pr>| pr.as_ref().is_some_and(|pr| pr.state == PrState::Merged);
                    if merged(&pr) && this.entry_mut(&path).is_some_and(|entry| !merged(&entry.pr)) {
                        if let Some((pi, _)) = this.find(&path) {
                            this.projects[pi].merged_checked = None;
                        }
                    }
                    if let Some(entry) = this.entry_mut(&path) {
                        if let Some(view) = &entry.view {
                            view.update(cx, |t, cx| {
                                t.pr = pr.clone();
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
            .filter(|p| p.tasks.iter().filter(|t| !t.main).count() > 1)
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

    fn sleep_idle_language_servers(&mut self, cx: &mut Context<Self>) {
        for view in self.views() {
            view.update(cx, |t, cx| t.drop_unused_language_servers(Self::lsp_idle(), cx));
        }
        for p in &self.projects {
            for t in &p.tasks {
                let (Some(view), Some(left)) = (&t.view, t.left_view) else { continue };
                if self.active.as_deref() != Some(t.path.as_path()) && left.elapsed() >= Self::lsp_idle() {
                    if view.read(cx).has_language_servers() {
                        view.update(cx, |t, cx| t.put_language_servers_to_sleep(cx));
                    }
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
        if view.read(cx).agent_busy() {
            let this = cx.weak_entity();
            let path = path.to_path_buf();
            window.open_alert_dialog(cx, move |dialog, _, _| {
                let (this, path) = (this.clone(), path.clone());
                dialog
                    .confirm()
                    .title("Stop the agent?")
                    .description("It is in the middle of a turn. The task's terminals are stopped too.")
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
        cx.notify();
    }

    /// A new title for a task; its branch and worktree stay as they are.
    pub fn rename_task_dialog(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry_mut(path) else { return };
        let current = entry.label();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Task title"));
        input.update(cx, |i, cx| {
            i.set_value(current, window, cx);
            i.select_all(window, cx);
        });
        let path = path.to_path_buf();
        let apply = {
            let (input, path) = (input.clone(), path.clone());
            move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                let title = input.read(cx).value().trim().to_string();
                if title.is_empty() {
                    return;
                }
                // The variants of a task are one task by name.
                let group = this.entry_mut(&path).map(|entry| entry.group.clone()).unwrap_or_default();
                for entry in this.projects.iter_mut().flat_map(|p| &mut p.tasks) {
                    if entry.path == path || (!group.is_empty() && entry.group == group) {
                        entry.title = title.clone();
                    }
                }
                window.close_dialog(cx);
                cx.notify();
            }
        };
        let on_enter = apply.clone();
        // Kept by the dialog and gone with it: detached, it would hold the input it listens to for good.
        let listening = Rc::new(cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                on_enter(this, window, cx);
            }
        }));
        let this = cx.weak_entity();
        let focused = std::rc::Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, focused, this, apply) = (input.clone(), focused.clone(), this.clone(), apply.clone());
            let _ = &listening;
            dialog
                .title("Rename task")
                .w(px(480.))
                .content(move |content, window, cx| {
                    if !focused.replace(true) {
                        let input = input.clone();
                        window.defer(cx, move |window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
                    }
                    content.child(
                        v_flex()
                            .gap_1()
                            .child(Input::new(&input))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("The branch keeps its name.")),
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

    pub fn toggle_star(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(entry) = self.entry_mut(path) {
            entry.starred = !entry.starred;
        }
        self.save(cx);
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
        use gpui_kit::component::input::{Textarea, TextareaState};
        let Some(p) = self.projects.get(project) else { return };
        let name = p.name.clone();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("What is it about? DEV-123 Fix login, or plain words"));
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 14)
                .placeholder("For the agent, optional: put into its prompt for you to finish and send")
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
        let submit = {
            let (this, filled_in) = (this.clone(), filled_in.clone());
            move |window: &mut Window, cx: &mut App| {
                let Some((title, text, notes, agents)) = filled_in(cx) else { return };
                window.close_dialog(cx);
                let _ = this.update(cx, |this, cx| this.create_task(project, title, text, notes, agents, window, cx));
            }
        };
        // Runs inside this entity's update, so it cannot go through `submit`.
        let on_enter = {
            let filled_in = filled_in.clone();
            move |this: &mut Self, event: &InputEvent, in_prompt: bool, window: &mut Window, cx: &mut Context<Self>| {
                // Enter in the title, Ctrl+Enter anywhere.
                let InputEvent::PressEnter { secondary, .. } = event else { return };
                if in_prompt && !*secondary {
                    return;
                }
                if let Some((title, text, notes, agents)) = filled_in(cx) {
                    window.close_dialog(cx);
                    this.create_task(project, title, text, notes, agents, window, cx);
                }
            }
        };
        let on_title = on_enter.clone();
        // Kept by the dialog and gone with it: detached, they would hold the inputs they listen to for good.
        let mut listening = vec![cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| on_title(this, event, false, window, cx))];
        // A ticket in the title fills the prompt, unless you wrote one yourself.
        let filled: Rc<RefCell<String>> = Rc::default();
        let fetch: Rc<RefCell<Option<Task<()>>>> = Rc::default();
        let repo = p.path.clone();
        {
            let prompt = prompt.clone();
            listening.push(cx.subscribe_in(&input, window, move |_, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let title = input.read(cx).value().trim().to_string();
                let current = prompt.read(cx).value().to_string();
                if !current.trim().is_empty() && current != *filled.borrow() {
                    return;
                }
                let set = |text: String, window: &mut Window, cx: &mut App| {
                    *filled.borrow_mut() = text.clone();
                    prompt.update(cx, |p, cx| p.set_value(text, window, cx));
                };
                if let Some(key) = jira_key(&title) {
                    *fetch.borrow_mut() = None;
                    set(format!("Jira issue {key}: read it, its description and comments, and do what it asks."), window, cx);
                } else if let Some(number) = github_issue(&title) {
                    let (repo, input, prompt, filled) = (repo.clone(), input.clone(), prompt.clone(), filled.clone());
                    let task = window.spawn(cx, async move |cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
                        let issue = cx.background_spawn(crate::procs::on_thread(move || { gh_issue(&repo, number) })).await;
                        let Some((issue_title, body)) = issue else { return };
                        let _ = cx.update(|window, cx| {
                            let current = prompt.read(cx).value().to_string();
                            if !current.trim().is_empty() && current != *filled.borrow() {
                                return;
                            }
                            let text = format!("GitHub issue #{number}: {issue_title}\n\n{}\n", body.trim());
                            *filled.borrow_mut() = text.clone();
                            prompt.update(cx, |p, cx| p.set_value(text, window, cx));
                            // Only the reference typed: the issue's title names the task.
                            let typed = input.read(cx).value().trim().to_string();
                            if github_issue(&typed) == Some(number) && typed.split_whitespace().count() == 1 {
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
        listening.push(cx.subscribe_in(&prompt, window, move |this, _, event: &InputEvent, window, cx| on_enter(this, event, true, window, cx)));
        let listening = Rc::new(listening);
        let focused = std::rc::Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, cx| {
            let (input, prompt, submit, focused) = (input.clone(), prompt.clone(), submit.clone(), focused.clone());
            let (count, notes, agents) = (count.clone(), notes.clone(), agents.clone());
            let _ = &listening;
            let many = count.get();
            let several = state::with_settings(|s| s.several_agents());
            let muted = cx.theme().muted_foreground;
            dialog
                .title(format!("New task in {name}"))
                .w(px(640.))
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
                                    .child(branch_hint(&input.read(cx).value())),
                            )
                            .child(Textarea::new(&prompt))
                            .when(several && many == 1, |form| {
                                form.child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(div().text_sm().child("Agent"))
                                        .child(agents_ui::picker("new-task-agent", agents[0].clone())),
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
    fn create_task(&mut self, project: usize, title: String, prompt: String, notes: Vec<String>, agents: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if notes.len() > 1 {
            return self.create_variants(project, title, prompt, notes, agents, window, cx);
        }
        let Some(p) = self.projects.get_mut(project) else { return };
        let branch = naming::branch_name(&title);
        let repo = p.path.clone();
        let path = git::task_worktree_dir(&repo, &branch);
        p.collapsed = false;
        p.tasks.push(TaskEntry {
            path: path.clone(),
            branch: branch.clone(),
            title,
            last_active: state::now(),
            busy: Some("creating…".into()),
            prompt: prompt.clone(),
            created: state::now(),
            agent: agents.into_iter().next().unwrap_or_default(),
            fresh: true,
            ..Default::default()
        });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let copy_env = state::settings().copy_env;
            let result = cx
                .background_spawn(async move {
                    let copy = crate::project_config::resolve(&repo).config.task.copy;
                    let made = git::create_task_worktree(&repo, &branch, copy_env && copy.is_none());
                    if let (Ok(dir), Some(copy)) = (&made, copy) {
                        crate::project_config::copy_ignored(&repo, dir, &copy);
                    }
                    made
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(_) => {
                        if let Some(entry) = this.entry_mut(&path) {
                            entry.busy = None;
                        }
                        this.switch(&path, window, cx);
                        this.task_made(&path, window, cx);
                        // To its own agent: the task in view is another when the switch did not happen.
                        let view = this.entry_mut(&path).and_then(|entry| entry.view.clone());
                        if let (false, Some(view)) = (prompt.is_empty(), view) {
                            view.update(cx, |t, cx| t.queue_prompt(prompt, false, window, cx));
                        }
                    }
                    Err(e) => {
                        if let Some((pi, ti)) = this.find(&path) {
                            this.projects[pi].tasks.remove(ti);
                        }
                        window.push_notification(Notification::error(e).title("Could not create the task"), cx);
                    }
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
            let roots = cx.background_spawn(async move { paths.iter().filter_map(|path| project_root(path)).collect::<Vec<_>>() }).await;
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
                .description("Its files and worktrees stay on disk; running tasks are stopped.")
                .ok_text("Remove")
                .on_ok(move |_, window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        for task in &running {
                            this.stop(task, window, cx);
                        }
                        this.projects.retain(|p| p.path != path);
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// Brings the task list in line with `git worktree list`: worktrees made
    /// outside the app appear, removed ones go.
    pub fn sync_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repos: Vec<PathBuf> = self.projects.iter().map(|p| p.path.clone()).collect();
        cx.spawn_in(window, async move |this, cx| {
            let lists = cx
                .background_spawn(async move {
                    repos
                        .into_iter()
                        .map(|r| {
                            let worktrees = git::worktrees(&r);
                            let all: Vec<PathBuf> = worktrees.iter().map(|w| w.path.clone()).collect();
                            // An agent's worktrees, with what they hold beyond the branch they came from.
                            let sides: std::collections::HashMap<PathBuf, side::Side> = worktrees
                                .iter()
                                .skip(1)
                                .filter(|w| !w.in_use())
                                .filter_map(|w| {
                                    let parent = side::made_by_agent(&w.path, &all)?;
                                    let from = parent.as_ref().unwrap_or(&all[0]);
                                    let base = worktrees.iter().find(|o| &o.path == from).map(|o| o.branch.clone()).unwrap_or_default();
                                    let (ahead, dirty) = git::work_beyond(&w.path, &base);
                                    let about = side::about(&w.path, from);
                                    Some((w.path.clone(), side::Side { parent, ahead, dirty, about }))
                                })
                                .collect();
                            (worktrees, sides, r)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let restoring = this.restoring.clone();
                for (worktrees, mut sides, repo) in lists {
                    let Some(p) = this.projects.iter_mut().find(|p| p.path == repo) else { continue };
                    // Not readable now (unmounted, git missing): leave it be.
                    if worktrees.first().is_none_or(|w| w.path != repo) {
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
                                t.branch = wt.branch.clone();
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
        // Which session the agent runs: what a restart, or the next run, resumes.
        if let Some(id) = event.session.as_deref().filter(|id| !id.is_empty()) {
            self.learn_session(&event.task, id, cx);
        }
        let Some(status) = event.status() else { return };
        let Some((pi, ti)) = self.find(&event.task) else { return };
        let entry = &self.projects[pi].tasks[ti];
        let Some(view) = entry.view.clone() else { return };
        let group = (!entry.group.is_empty()).then(|| entry.group.clone());
        let mut label = format!("{} · {}", self.projects[pi].name, entry.label());
        let variant = variants::letter(entry.variant);
        // A group on its screen is in view with all its variants.
        let in_view = self.active.as_deref() == Some(event.task.as_path()) || (group.is_some() && self.active_group == group);
        let looked = in_view && window.is_window_active();
        // Heard again after a restart: new only when it came after the last look.
        let seen = looked || event.replayed.is_some_and(|at| at <= entry.seen_at);
        let was = view.read(cx).agent_status.clone();
        // The Notification about a question its PreToolUse told already: no second notice, the question kept.
        let status = if agent::asked_again(&was, &event) { was.clone() } else { status };
        if looked {
            self.projects[pi].tasks[ti].seen_at = state::now();
        }
        // A turn's end, not the reminder a minute later or a session's start.
        let turn_over = status == agent::AgentStatus::Done && matches!(was, agent::AgentStatus::Working | agent::AgentStatus::NeedsInput(_));
        // A message that waited for this goes out now: the agent is not done yet.
        let more_coming = turn_over && view.read(cx).has_queued_message();
        let heard = view.update(cx, |t, cx| {
            let said = (t.agent_said.clone(), t.agent_asks.clone());
            t.hear(&event, &status);
            t.status_guessed = event.guessed;
            t.set_agent_status(status.clone(), seen, cx);
            let before = t.agent_shown();
            t.follow_agent(&event.event, event.file.as_deref());
            match event.event.as_str() {
                "PreToolUse" if status == agent::AgentStatus::Working => t.agent_activity = event.message.clone(),
                "UserPromptSubmit" => t.agent_activity = Some("Thinking".into()),
                _ => {}
            }
            if t.agent_shown() != before {
                cx.emit(TaskEvent::Changed);
                cx.notify();
            }
            if turn_over && event.replayed.is_none() {
                t.deliver_queued(window, cx);
            }
            said != (t.agent_said.clone(), t.agent_asks.clone())
        });
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
        if status.is_attention() && !seen && !hidden && status != was && event.replayed.is_none() && state::settings().notifications {
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
                    let _ = tx.send_blocking(path);
                });
            }
        }
        // The rest reaches the list through the task, when it shows another way.
        if status != was || turn_over || heard {
            cx.notify();
        }
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
            view.update(cx, |t, _| t.learned_session(id));
        }
    }

    /// What the agent of a task said, as far as it is to be shown: not
    /// while presenting, not when turned off. Whole, for a notification.
    pub fn said_shown(&self, view: &Entity<crate::task::TaskView>, cx: &App) -> Option<SharedString> {
        if self.is_presenting() || !state::with_settings(|s| s.show_said) {
            return None;
        }
        let v = view.read(cx);
        match v.agent_status {
            agent::AgentStatus::NeedsInput(_) => v.agent_asks.as_ref(),
            _ => v.agent_said.as_ref(),
        }
        .map(|said| said.whole.clone())
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
            .filter(|t| t.view.is_some())
            .filter_map(|t| Some((t.path.clone(), t.session_ref()?, self.usage.get(&t.path).and_then(|(written, _)| *written))))
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    known
                        .into_iter()
                        .filter_map(|(path, who, before)| {
                            let transcript = who.transcript(&path)?;
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

    pub fn toggle_power_saving(&mut self, cx: &mut Context<Self>) {
        let on = !crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed);
        crate::lsp::POWER_SAVING.store(on, std::sync::atomic::Ordering::Relaxed);
        for view in self.views() {
            view.update(cx, |t, cx| t.set_language_servers(!on, cx));
        }
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
        let project_agent = self.projects[pi].agent.clone();
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
                let settings = state::settings();
                let last = t.sessions.last().map(|s| s.profile.clone()).unwrap_or_default();
                crate::agents::resolve(&settings.agents, &[&last, &t.agent], project_agent.as_deref(), &settings.default_agent)?.clone()
            }
        };
        let same = t.sessions.last().is_some_and(|s| s.profile == profile.id);
        let start = if t.fresh && same {
            // Recorded already and never run (Continue with another agent).
            Start::New(t.sessions.last().and_then(|s| s.id.clone()))
        } else if same {
            crate::agents::start_of(t.sessions.last())
        } else if !t.fresh && t.sessions.is_empty() {
            // A task of before sessions were kept: as then, and its id is learned from the agent.
            t.sessions.push(state::AgentSession { profile: profile.id.clone(), id: None, started: state::now() });
            Start::Last
        } else {
            let id = profile.adapter().chooses_session_id().then(crate::agents::uuid);
            t.sessions.push(state::AgentSession { profile: profile.id.clone(), id: id.clone(), started: state::now() });
            Start::New(id)
        };
        t.fresh = false;
        Some(Launch { profile, start, unasked })
    }

    /// New settings: they go to the state, and what they change now
    /// changes (new tasks' agent, language servers of open files).
    pub fn apply_settings(&mut self, new: state::Settings, cx: &mut Context<Self>) {
        let old = state::settings();
        state::set_settings(new.clone());
        gpui_kit::base::input::HUMP_WORDS.store(new.hump_words, std::sync::atomic::Ordering::Relaxed);
        if old.theme != new.theme {
            theme::choose(&new.theme, cx);
            theme::apply_everywhere(self.appearance, cx);
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
        if old.lsp_off != new.lsp_off {
            for view in self.views() {
                view.update(cx, |t, cx| {
                    t.set_language_servers(false, cx);
                    if !crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed) {
                        t.set_language_servers(true, cx);
                    }
                });
            }
        }
        self.save(cx);
        cx.notify();
    }

    /// Shows the task at `path` with its agent in front.
    pub fn focus_task(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pi, ti)) = self.find(path) else { return };
        // A group that is done is looked at as a whole; a variant that asks
        // something is answered in its own agent.
        let entry = &self.projects[pi].tasks[ti];
        let asks = entry.view.as_ref().is_some_and(|v| matches!(v.read(cx).agent_status, agent::AgentStatus::NeedsInput(_)));
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
        self.save(cx);
        cx.quit();
    }

    /// The window's ×, from the menu bar: without a daemon to keep the
    /// agents, that is a quit, and asks as one.
    pub fn close_or_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(unix)]
        if crate::daemon::client::get().is_some() {
            return self.close_window(cx);
        }
        self.request_quit(window, cx)
    }

    /// Exit: ends every agent and terminal, after a word when any run.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let running = self.views().len();
        if running == 0 {
            self.save(cx);
            return cx.quit();
        }
        let busy = self.views().iter().filter(|v| v.read(cx).agent_busy()).count();
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

    fn check_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::update::enabled() || !state::settings().check_updates {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let found = cx.background_spawn(crate::procs::on_thread(move || { crate::update::newer() })).await;
            let _ = this.update(cx, |this, cx| {
                this.update = found;
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
        self.save(cx);
        self.keep_terminals = true;
        match crate::desktop::spawn_again() {
            Ok(()) => cx.quit(),
            Err(e) => {
                self.keep_terminals = false;
                window.push_notification(Notification::error(e.to_string()).title("Restart"), cx)
            }
        }
    }

    /// Draws with a theme at once, and keeps it as the one set: for the
    /// list that shows each as it is gone through.
    pub fn use_theme(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        theme::choose(name, cx);
        theme::apply(self.appearance, window, cx);
        let mut settings = state::settings();
        if settings.theme != theme::family(cx) {
            settings.theme = theme::family(cx).to_string();
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
        cx.notify();
    }
}

/// "DEV-123" at the start of a title.
/// Writes the state when it is not what was written last, saves in the
/// order they were asked for.
fn write_state(last: &std::sync::Mutex<(u64, String)>, seq: u64, json: String) {
    let mut last = last.lock().unwrap();
    if seq < last.0 {
        return;
    }
    last.0 = seq;
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

fn jira_key(title: &str) -> Option<&str> {
    let first = title.split_whitespace().next()?;
    let (project, number) = first.split_once('-')?;
    let ok = project.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && project.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit());
    ok.then_some(first)
}

/// "#123" at the start of a title, or a GitHub issue link in it.
fn github_issue(title: &str) -> Option<u64> {
    let first = title.split_whitespace().next()?;
    if let Some(n) = first.strip_prefix('#') {
        return n.parse().ok();
    }
    let (_, rest) = title.split_once("/issues/")?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// The issue's title and body, by `gh` in the repository.
fn gh_issue(repo: &Path, number: u64) -> Option<(String, String)> {
    let out = crate::procs::command("gh")
        .current_dir(repo)
        .env("GH_PROMPT_DISABLED", "1")
        .args(["issue", "view", &number.to_string(), "--json", "title,body"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    Some((v["title"].as_str()?.to_string(), v["body"].as_str().unwrap_or_default().to_string()))
}

fn branch_hint(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        "The branch name comes from the title.".into()
    } else {
        let branch = naming::preview(title);
        format!("Branch: {branch}")
    }
}

use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
