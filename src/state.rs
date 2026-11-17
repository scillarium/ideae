//! What survives a restart: projects, their tasks and each task's tabs, kept
//! in `~/.local/share/ideae/state.json` (`%LOCALAPPDATA%\ideae` on Windows).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct State {
    pub appearance: Option<String>,
    pub sidebar_width: Option<f32>,
    /// The sidebar is a narrow rail that opens under the pointer.
    pub sidebar_rail: bool,
    pub side_panel_width: Option<f32>,
    pub active_collapsed: bool,
    pub starred_collapsed: bool,
    /// A presentation that was on: it is on again after a restart, or the
    /// restart would show everything.
    pub presenting: Option<PresentingState>,
    /// The spaces projects are sorted into, and the one in the window;
    /// none for all of them.
    pub spaces: Vec<crate::workspace::spaces::Space>,
    pub space: Option<String>,
    pub power_saving: bool,
    pub window: Option<WindowPlace>,
    pub settings: Settings,
    pub active_task: Option<PathBuf>,
    /// Text size per kind of view.
    pub zoom: crate::zoom::Zoom,
    pub projects: Vec<ProjectState>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct PresentingState {
    pub kept: Vec<PathBuf>,
    pub since: u64,
}

impl State {
    /// Whether a task is one to tell of, as the window was left: of the
    /// space that was in it, and not left out of a presentation.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn tells_of(&self, task: &std::path::Path) -> bool {
        let project = self.projects.iter().find(|p| p.tasks.iter().any(|t| t.path == task));
        let in_space = match (&self.space, project.and_then(|p| p.space.as_ref())) {
            (Some(shown), Some(its)) => shown == its,
            _ => true,
        };
        let presented = self.presenting.as_ref().is_none_or(|p| p.kept.iter().any(|kept| kept == task));
        in_space && presented
    }
}

/// What the user set in Settings (Ctrl+,).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    /// The default agent's command, as older versions read it: kept in step
    /// with `agents`, of which it is the first when a state has none.
    pub agent: String,
    /// The agents to choose from (agents/mod.rs); none, and tasks get a
    /// plain shell. `--agent` and `--no-agent` on the command line win.
    #[serde(default)]
    pub agents: Vec<crate::agents::Profile>,
    /// The one new tasks run, by its id.
    #[serde(default)]
    pub default_agent: String,
    /// The one asked one-shot questions (commit messages, the variants'
    /// judge); empty for the default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quick_agent: String,
    /// Language servers (program names) never started.
    pub lsp_off: Vec<String>,
    /// Minutes a task out of view keeps its language servers.
    pub lsp_idle_minutes: u32,
    pub notifications: bool,
    /// A notification has the agent's words (its answer, its question);
    /// off, only "Finished" or "Needs your answer".
    pub notify_said: bool,
    /// Copy the main checkout's ignored `.env*` files into new worktrees.
    pub copy_env: bool,
    pub pr_status: bool,
    /// Minutes between background fetches of each project's base branch;
    /// 0 for never.
    pub fetch_minutes: u32,
    /// Release builds ask GitHub for newer releases.
    pub check_updates: bool,
    /// Words in code end at the humps of camelCase and the underscores of
    /// snake_case, for moving, selecting and deleting by word.
    pub hump_words: bool,
    /// The editor's gutter marks what the branch changed since it left its
    /// base, not only since the last commit.
    pub gutter_since_base: bool,
    /// Agents keep Claude Code's LSP plugins (a rust-analyzer per agent…).
    pub agent_lsp: bool,
    /// A session that has used this many thousand tokens is told of, once;
    /// 0 for never.
    pub token_warn_k: u32,
    /// While presenting, code and terminals are this many percent of their size.
    pub present_zoom: u32,
    /// Milliseconds the pointer rests on the sidebar's rail before the
    /// whole sidebar opens; 0 for never.
    pub rail_open_ms: u32,
    /// The theme, by the name of its family: "Ayu", "Gruvbox"…
    pub theme: String,
    /// The title bar shows the machine's load, and what the agents took.
    pub show_load: bool,
    pub show_tokens: bool,
    /// A task in the sidebar shows how full its agent's context is.
    pub context_dial: bool,
    /// So does the agent's tab of the task in view.
    pub context_on_tab: bool,
    /// The language server's completions come up as you type, not only
    /// when asked for; its colors for names are drawn; a file is
    /// reformatted when saved by hand.
    pub complete_as_typed: bool,
    pub semantic_colors: bool,
    /// The language server's hints in the code: types after names,
    /// parameter names before arguments.
    pub inlay_hints: bool,
    /// Who last changed the cursor's line is told at the end of that line;
    /// off, in the file's header.
    pub blame_at_line_end: bool,
    pub format_on_save: bool,
    /// The worktrees agents make for themselves are listed, folded under
    /// the task they came from.
    pub show_side_worktrees: bool,
    /// What the agent said shows under its task in Needs you, in the row's
    /// tooltip and in the notification.
    pub show_said: bool,
    /// A file opened to look at (a jump, a search result) takes the place of
    /// the one opened that way before, until it is edited.
    pub preview_tabs: bool,
    /// File tabs kept open; past it, those left longest close. 0 for no limit.
    pub tab_limit: u32,
    /// The Changes tab starts with old and new one under the other.
    pub diff_unified: bool,
    /// Diffs leave out changes of whitespace and blank lines.
    pub diff_ignore_whitespace: bool,
    /// A pull, rebase, pick or revert that would conflict asks the agent
    /// to do it and resolve the conflicts.
    pub conflicts_to_agent: bool,
    /// While presenting: the sidebar is a rail, the files git ignores are
    /// not shown, the load and token readouts are hidden.
    pub present_rail: bool,
    pub present_veil: bool,
    pub present_hide_readouts: bool,
    /// Shortcuts set by hand, by id (keymap.rs): keystrokes such as
    /// "ctrl-e", empty for none.
    pub keys: std::collections::BTreeMap<String, String>,
    /// A restored task's agent starts a new session told a summary, not
    /// its whole conversation again.
    pub restore_summary: bool,
    /// Days an archived task is kept; 0 for always.
    pub archive_keep_days: u32,
    /// The first start's word on which agents are installed was answered.
    /// A state from before it had its agents set up already.
    #[serde(default = "yes")]
    pub agents_welcomed: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            agent: "claude".into(),
            agents: vec![crate::agents::Profile::claude_code()],
            default_agent: crate::agents::DEFAULT_ID.into(),
            quick_agent: String::new(),
            lsp_off: Vec::new(),
            lsp_idle_minutes: 15,
            notifications: true,
            notify_said: true,
            copy_env: true,
            pr_status: true,
            fetch_minutes: 5,
            preview_tabs: true,
            show_side_worktrees: true,
            show_said: true,
            tab_limit: 12,
            check_updates: true,
            hump_words: false,
            gutter_since_base: false,
            agent_lsp: false,
            token_warn_k: 0,
            present_zoom: 125,
            rail_open_ms: 750,
            theme: "Ayu".into(),
            show_load: true,
            show_tokens: true,
            context_dial: true,
            context_on_tab: true,
            complete_as_typed: true,
            semantic_colors: true,
            inlay_hints: true,
            blame_at_line_end: true,
            format_on_save: false,
            diff_unified: false,
            diff_ignore_whitespace: false,
            conflicts_to_agent: true,
            present_rail: true,
            present_veil: true,
            present_hide_readouts: true,
            keys: std::collections::BTreeMap::new(),
            restore_summary: false,
            archive_keep_days: 90,
            agents_welcomed: false,
        }
    }
}

impl Settings {
    /// A state of before profiles: its agent command becomes the one
    /// profile, the default. After: the default named exists, and `agent`
    /// is its command, for an older version reading the state.
    pub fn normalize(&mut self) {
        if self.agents.is_empty() && !self.agent.trim().is_empty() {
            self.agents.push(crate::agents::Profile::from_command(&self.agent));
        }
        for at in 0..self.agents.len() {
            if self.agents[at].id.is_empty() || self.agents[..at].iter().any(|p| p.id == self.agents[at].id) {
                let name = self.agents[at].name.clone();
                self.agents[at].id = crate::agents::new_profile_id(&name, &self.agents);
            }
        }
        if !self.agents.iter().any(|p| p.id == self.default_agent) {
            self.default_agent = self.agents.first().map(|p| p.id.clone()).unwrap_or_default();
        }
        if !self.quick_agent.is_empty() && !self.agents.iter().any(|p| p.id == self.quick_agent) {
            self.quick_agent.clear();
        }
        self.agent = self.default_profile().map(|p| p.command.clone()).unwrap_or_default();
    }

    pub fn profile(&self, id: &str) -> Option<&crate::agents::Profile> {
        self.agents.iter().find(|p| p.id == id)
    }

    pub fn default_profile(&self) -> Option<&crate::agents::Profile> {
        self.profile(&self.default_agent).or_else(|| self.agents.first())
    }

    /// Who answers one-shot questions.
    pub fn quick_profile(&self) -> Option<&crate::agents::Profile> {
        self.profile(&self.quick_agent).or_else(|| self.default_profile())
    }

    /// More than one agent to choose from: marks and choices show.
    pub fn several_agents(&self) -> bool {
        self.agents.len() > 1
    }
}

static SETTINGS: std::sync::RwLock<Option<Settings>> = std::sync::RwLock::new(None);

/// The settings in force, readable from any thread.
pub fn settings() -> Settings {
    if let Some(s) = SETTINGS.read().unwrap().as_ref() {
        return s.clone();
    }
    let loaded = load().settings;
    *SETTINGS.write().unwrap() = Some(loaded.clone());
    loaded
}

/// One thing out of the settings, without a copy of them all.
pub fn with_settings<T>(read: impl FnOnce(&Settings) -> T) -> T {
    if let Some(s) = SETTINGS.read().unwrap().as_ref() {
        return read(s);
    }
    read(&settings())
}

/// The settings as the state just read has them, unless set already.
pub fn init_settings(settings: &Settings) {
    let mut current = SETTINGS.write().unwrap();
    if current.is_none() {
        *current = Some(settings.clone());
    }
}

pub fn set_settings(mut settings: Settings) {
    settings.normalize();
    *SETTINGS.write().unwrap() = Some(settings);
}

/// The window's size and place. Wayland lets no app choose its position;
/// there the size and the maximized state still come back.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct WindowPlace {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
    pub fullscreen: bool,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct ProjectState {
    /// The main worktree.
    pub path: PathBuf,
    /// The space it is sorted into, by its name; none, and it is in every one.
    pub space: Option<String>,
    pub collapsed: bool,
    pub tasks: Vec<TaskState>,
    /// The command last run in every variant of a task, and whether it
    /// runs by itself when they have finished.
    pub check: String,
    pub check_auto: bool,
    /// What the judge said of each group of variants, and the groups that
    /// changed since it spoke.
    pub verdicts: std::collections::BTreeMap<String, crate::workspace::variants::Run>,
    pub stale_verdicts: Vec<String>,
    /// Project settings kept for yourself ("Only for me"): used in place
    /// of the repository's `.ideae.toml`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub own: Option<crate::project_config::Config>,
    /// Whether the commands of `.ideae.toml` may run, by their trust key:
    /// asked once for each version of them.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub trusted: std::collections::BTreeMap<String, bool>,
    /// Hints asked not to be shown again.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints_off: Vec<String>,
    /// The agent its new tasks run, by profile id; none for the default.
    /// Here and not in `.ideae.toml`: whose subscription runs is personal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct TaskState {
    /// The worktree; for the main thread, the project's own path.
    pub path: PathBuf,
    pub branch: String,
    pub title: String,
    pub main: bool,
    pub starred: bool,
    /// Unix seconds of the last time it was in view.
    pub last_active: u64,
    /// Running when the app quit, so started again with it.
    pub open: bool,
    /// Its share of the services' ports: 0 for the main checkout.
    pub port_slot: u32,
    pub session: TaskSession,
    /// One of several tries at the same task: the group they share (empty
    /// for a task on its own), its number there, and what its agent alone
    /// was told on top of the prompt.
    pub group: String,
    pub variant: u32,
    pub note: String,
    /// What the agent was told to start with, and when: Unix seconds.
    pub prompt: String,
    pub created: u64,
    /// When its agent's latest turn ended.
    pub done_at: u64,
    /// When it was last looked at: in view in the active window.
    #[serde(skip_serializing_if = "is_zero")]
    pub seen_at: u64,
    /// The commit a variant was made from: what its diff is counted against.
    pub base: String,
    /// How the command last run in every variant went in this one.
    pub check: Option<crate::workspace::variants::Run>,
    /// The agent chosen when it was made, by profile id.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub agent: String,
    /// Its agents' sessions, the one it runs last.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<AgentSession>,
    /// The archive it was restored from, by key: archived again, it takes
    /// its place, unless that one keeps work the task did not get back.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub archive: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub archive_keep: bool,
}

/// A session of an agent in a task.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct AgentSession {
    /// The profile it ran.
    pub profile: String,
    /// The agent's own id for it, once known: what is resumed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Unix seconds.
    pub started: u64,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct TaskSession {
    pub tabs: Vec<TabSession>,
    /// Indices into `tabs`.
    pub left_active: Option<usize>,
    pub right_active: Option<usize>,
    pub focus_right: bool,
    /// Folders open in the file tree.
    pub expanded: Vec<String>,
    /// 0 Changes, 1 Files, 2 Structure.
    pub side_panel: u8,
    /// Review comments not sent to the agent yet.
    pub review: Vec<ReviewNote>,
    /// Changed files marked viewed, with a hash of their diff then.
    pub viewed: std::collections::BTreeMap<String, u64>,
    /// Bookmarked lines (0-based) by file, relative to the worktree.
    pub bookmarks: std::collections::BTreeMap<String, Vec<u32>>,
}

/// A review comment on lines of the diff.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct ReviewNote {
    /// "path:12-14".
    pub location: String,
    pub code: String,
    pub language: String,
    pub text: String,
    /// Where it shows in the diff: under this line of the file.
    pub path: String,
    pub modified_side: bool,
    pub line: usize,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct TabSession {
    /// agent, shell, changes, history, file, browser.
    pub kind: String,
    /// A file, relative to the worktree.
    pub path: Option<String>,
    /// 0-based cursor line of a file.
    pub line: u32,
    pub url: Option<String>,
    pub right: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preview: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

/// Debug builds are a separate app: their own data, sockets, window class
/// and launcher, so a dev build never touches the release one's tasks.
pub const APP_ID: &str = if cfg!(debug_assertions) { "ideae-dev" } else { "ideae" };

/// Where apps keep their data: `~/.local/share`, `%LOCALAPPDATA%` on Windows.
pub fn data_home() -> PathBuf {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        return var("LOCALAPPDATA").unwrap_or_else(std::env::temp_dir);
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = var("HOME") {
            return home.join("Library/Application Support");
        }
    }
    var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local/share"))).unwrap_or_else(std::env::temp_dir)
}

/// Sockets, hook files and browser profiles of running instances.
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(APP_ID)
}

pub fn data_dir() -> PathBuf {
    data_home().join(APP_ID)
}

/// Where the state is, and what is kept next to it.
pub fn state_dir() -> PathBuf {
    state_path().parent().map(PathBuf::from).unwrap_or_else(data_dir)
}

fn state_path() -> PathBuf {
    // A separate state for experiments, such as headless test runs.
    match std::env::var_os("IDEAE_STATE") {
        Some(path) => PathBuf::from(path),
        None => data_dir().join("state.json"),
    }
}

pub fn load() -> State {
    let path = state_path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<State>(&bytes).map(|mut state| {
            state.settings.normalize();
            state
        }).unwrap_or_else(|e| {
            // Keep the unreadable file for a look rather than overwrite it.
            eprintln!("state {}: {e}; starting empty", path.display());
            let _ = std::fs::rename(&path, path.with_extension("json.bad"));
            State::default()
        }),
        Err(_) => State::default(),
    }
}

/// Writes the serialized state; a crash mid-write leaves the old file whole.
pub fn save(json: &str) -> std::io::Result<()> {
    write_atomic(&state_path(), json.as_bytes())
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod agents_tests {
    use super::*;

    #[test]
    fn a_fresh_install_runs_claude_code() {
        let s = Settings::default();
        assert_eq!(s.agents, [crate::agents::Profile::claude_code()]);
        assert_eq!((s.default_profile().map(|p| p.name.as_str()), s.quick_profile().map(|p| p.id.as_str())), (Some("Claude Code"), Some("claude")));
        assert!(!s.several_agents());
        let mut empty: State = serde_json::from_str("{}").unwrap();
        empty.settings.normalize();
        assert_eq!(empty.settings, Settings::default());
    }

    #[test]
    fn the_agent_command_of_before_becomes_the_default_profile() {
        let mut old: Settings = serde_json::from_str(r#"{"agent":"claude --model opus"}"#).unwrap();
        old.normalize();
        assert_eq!(old.agents.len(), 1);
        assert_eq!((old.agents[0].kind, old.agents[0].command.as_str(), old.default_agent.as_str()), (crate::agents::Kind::Claude, "claude --model opus", "claude"));
        let mut other: Settings = serde_json::from_str(r#"{"agent":"aider --yes"}"#).unwrap();
        other.normalize();
        assert_eq!((other.agents[0].kind, other.agents[0].name.as_str(), other.agent.as_str()), (crate::agents::Kind::Generic, "aider", "aider --yes"));
        // A plain shell stays one.
        let mut shell: Settings = serde_json::from_str(r#"{"agent":""}"#).unwrap();
        shell.normalize();
        assert!(shell.agents.is_empty() && shell.default_profile().is_none());
    }

    #[test]
    fn profiles_and_sessions_are_kept() {
        let mut s = Settings::default();
        let codex = crate::agents::Profile { id: "codex".into(), name: "Codex".into(), kind: crate::agents::Kind::Codex, command: "codex".into(), ..Default::default() };
        s.agents.push(codex);
        s.default_agent = "codex".into();
        s.quick_agent = "gone".into();
        s.normalize();
        assert_eq!((s.agent.as_str(), s.quick_agent.as_str()), ("codex", ""), "the older field follows the default; a missing quick one is the default");
        let task = TaskState {
            agent: "codex".into(),
            sessions: vec![AgentSession { profile: "claude".into(), id: Some("u-1".into()), started: 5 }, AgentSession { profile: "codex".into(), id: None, started: 9 }],
            ..Default::default()
        };
        let state = State { settings: s, projects: vec![ProjectState { agent: Some("codex".into()), tasks: vec![task], ..Default::default() }], ..Default::default() };
        let back: State = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(back == state);
        // What an older state has not: none of it.
        let old: TaskState = serde_json::from_str(r#"{"path":"/w/t"}"#).unwrap();
        assert!(old.sessions.is_empty() && old.agent.is_empty());
    }
}

#[cfg(test)]
mod spaces_tests {
    use super::{PresentingState, ProjectState, State, TaskState};
    use std::path::{Path, PathBuf};

    /// With the window closed, a task is told of as the window was left:
    /// of the space in it, and not left out of a presentation.
    #[test]
    fn what_is_out_of_the_window_is_not_told_of() {
        let task = |path: &str| TaskState { path: path.into(), ..Default::default() };
        let project = |space: Option<&str>, path: &str| ProjectState { space: space.map(str::to_string), tasks: vec![task(path)], ..Default::default() };
        let mut state = State { projects: vec![project(Some("Work"), "/w/a"), project(Some("Home"), "/h/b"), project(None, "/n/c")], ..Default::default() };
        let told = |state: &State| ["/w/a", "/h/b", "/n/c", "/elsewhere"].map(|path| state.tells_of(Path::new(path)));
        assert_eq!(told(&state), [true, true, true, true], "all of them in the window");
        state.space = Some("Work".into());
        assert_eq!(told(&state), [true, false, true, true], "one sorted into no space is in every one");
        state.presenting = Some(PresentingState { kept: vec![PathBuf::from("/n/c")], since: 0 });
        assert_eq!(told(&state), [false, false, true, false], "and of those, the ones chosen to present");
    }
}
