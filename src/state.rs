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
    /// The side panels of the lower half (a bit each; not set, Structure
    /// and Run), and how high it is.
    pub side_lower: Option<u8>,
    pub side_split: Option<f32>,
    /// Told once that the app is not listed with the applications.
    pub told_unlisted: bool,
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
    /// Agents tasks' agents started, by the ids they were given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crew: Vec<CrewRecord>,
}

/// One agent a task's agent started (workspace/crew.rs): a question
/// (`main`, `n`), a task (`path`), one proposed (`title`, `prompt`) or declined.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct CrewRecord {
    pub id: u32,
    pub parent: PathBuf,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "is_zero")]
    pub n: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prompt: String,
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
    /// Where tasks' worktrees go, a folder of each project's name in it;
    /// empty: beside each repository.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub worktrees_dir: String,
    /// The hints said no more of, in every project ("check", "setup").
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints_off: Vec<String>,
    /// For a language with more than one server (`lsp::ALTERNATIVES`), the
    /// one it runs, by the language's key; none, the first.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub lsp_choice: std::collections::BTreeMap<String, String>,
    /// A licence key typed in Settings, on its way to the sealed store
    /// (`secrets`), never written here: empty to remove the one kept.
    #[serde(skip)]
    pub lsp_licence: Option<String>,
    /// Every task has its own language servers, not one for the project's
    /// tasks (`lsp_share`).
    pub lsp_separate: bool,
    /// Minutes a task out of view keeps its language servers.
    pub lsp_idle_minutes: u32,
    /// Minutes a chat agent with nothing to do keeps running in a task out
    /// of view; 0: no end.
    pub agent_idle_minutes: u32,
    /// A terminal agent too, when it can be resumed and nothing of its own
    /// runs on (`TaskView::put_agent_to_sleep`).
    pub sleep_terminal_agents: bool,
    /// A task's agent may ask other agents of its project and propose tasks
    /// (workspace/crew.rs): its MCP server lists those tools.
    pub agents_start_agents: bool,
    /// Minutes a question's agent with nothing to do keeps running out of
    /// sight, and days an open question nobody came back to stays open; 0:
    /// no end.
    pub question_sleep_minutes: u32,
    pub question_close_days: u32,
    /// Minutes a chat agent's question waits before it is declined; 0: it
    /// waits for you.
    pub agent_unanswered_minutes: u32,
    pub notifications: bool,
    /// A notification has the agent's words (its answer, its question);
    /// off, only "Finished" or "Needs your answer".
    pub notify_said: bool,
    /// The window's corner its notices show in: top-left, top-right,
    /// bottom-left, bottom-right.
    pub notices_at: String,
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
    /// The theme drawn while light, by the name of its family: "Ayu", "Gruvbox"…
    pub theme: String,
    /// The one drawn while dark; empty for the same family as [`Self::theme`].
    #[serde(skip_serializing_if = "String::is_empty")]
    pub dark_theme: String,
    /// A new task's prompt is sent once its agent is up, not left in the
    /// agent's input to finish.
    pub send_new_prompt: bool,
    /// The family code is drawn in: one built in, or one installed.
    pub code_font: String,
    /// Pairs such as `->` and `!=` are drawn as one sign where the font has one.
    pub ligatures: bool,
    /// A terminal's cell, in percent of the font's own width and height.
    pub terminal_letter_spacing: u32,
    pub terminal_line_height: u32,
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
    /// A command with keys, chosen with the pointer, says its keys (learn.rs).
    pub learn_keys: bool,
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
    /// Diffs show their files in a column beside them.
    pub diff_files: bool,
    /// Its width, as last dragged.
    pub diff_files_width: f32,
    /// How much of a file's room its text takes beside its drawing, by what
    /// it is drawn as ("markdown", "svg", "table", "mermaid"), as last dragged.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub split_shares: std::collections::BTreeMap<String, f32>,
    /// How a file is shown when it opens, by what it is drawn as, as last
    /// chosen: "code", "split", "preview".
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub file_looks: std::collections::BTreeMap<String, String>,
    /// Changed files as a list of paths, not a tree of folders: in the
    /// Changes panel and beside a diff.
    pub changes_flat: bool,
    /// A page of the task's own site is loaded again when its agent's turn ends.
    pub web_reload_after_turn: bool,
    /// Said once as the battery runs low: there is a power save mode.
    pub battery_hint: bool,
    /// The system does not go to sleep while an agent works, a setup or a
    /// check runs; the screen still dims and locks.
    pub keep_awake: bool,
    /// Nor while on the battery; off, only while plugged in.
    pub keep_awake_on_battery: bool,
    /// The Files panel goes to the file in the editor by itself.
    pub tree_follows: bool,
    /// What the Structure panel leaves out, and whether it is in the
    /// order of the names instead of the file's.
    pub structure_public: bool,
    pub structure_no_fields: bool,
    pub structure_sorted: bool,
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
    /// Edit prediction by a local model (predict/): None until the offer
    /// to turn it on is answered.
    pub edit_prediction: Option<bool>,
    /// "auto", or the id of a model (predict::assets::Model).
    pub prediction_model: String,
    /// Linux and Windows: the menu names stay in the title bar; off, they are behind ☰.
    pub menu_bar_shown: bool,
}

fn yes() -> bool {
    true
}

/// The percent a terminal's letter spacing and line height may be.
pub const SPACING: (u32, u32) = (50, 200);

impl Default for Settings {
    fn default() -> Self {
        Self {
            agent: "claude".into(),
            agents: vec![crate::agents::Profile::claude_code()],
            default_agent: crate::agents::DEFAULT_ID.into(),
            quick_agent: String::new(),
            lsp_off: Vec::new(),
            worktrees_dir: String::new(),
            hints_off: Vec::new(),
            lsp_choice: Default::default(),
            lsp_licence: None,
            lsp_separate: false,
            lsp_idle_minutes: 15,
            agent_idle_minutes: 30,
            sleep_terminal_agents: false,
            agents_start_agents: false,
            question_sleep_minutes: 15,
            question_close_days: 3,
            agent_unanswered_minutes: 0,
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
            dark_theme: String::new(),
            send_new_prompt: false,
            code_font: crate::theme::CODE_FONT.into(),
            ligatures: true,
            terminal_letter_spacing: 100,
            terminal_line_height: 100,
            show_load: false,
            show_tokens: false,
            context_dial: true,
            context_on_tab: true,
            complete_as_typed: true,
            semantic_colors: true,
            inlay_hints: true,
            blame_at_line_end: true,
            notices_at: "bottom-right".into(),
            format_on_save: false,
            learn_keys: false,
            diff_unified: false,
            diff_ignore_whitespace: false,
            diff_files: false,
            changes_flat: false,
            web_reload_after_turn: true,
            battery_hint: true,
            keep_awake: true,
            keep_awake_on_battery: true,
            tree_follows: false,
            structure_public: false,
            structure_no_fields: false,
            structure_sorted: false,
            diff_files_width: 240.,
            split_shares: std::collections::BTreeMap::new(),
            file_looks: std::collections::BTreeMap::new(),
            conflicts_to_agent: true,
            present_rail: true,
            present_veil: true,
            present_hide_readouts: true,
            keys: std::collections::BTreeMap::new(),
            restore_summary: false,
            archive_keep_days: 90,
            agents_welcomed: false,
            edit_prediction: None,
            prediction_model: "auto".into(),
            menu_bar_shown: false,
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
        self.terminal_letter_spacing = self.terminal_letter_spacing.clamp(SPACING.0, SPACING.1);
        self.terminal_line_height = self.terminal_line_height.clamp(SPACING.0, SPACING.1);
    }

    pub fn dark_theme(&self) -> &str {
        if self.dark_theme.is_empty() { &self.theme } else { &self.dark_theme }
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

/// Changes one agent profile in the settings in force, when `change` says
/// it changed anything.
pub fn update_profile(id: &str, change: impl FnOnce(&mut crate::agents::Profile) -> bool) {
    let mut settings = settings();
    let changed = settings.agents.iter_mut().find(|p| p.id == id).is_some_and(change);
    if changed {
        set_settings(settings);
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
    /// Where its new tasks branch from (`develop`, `origin/release`); empty
    /// for the remote's default branch.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub base: String,
    /// The folder its tasks' worktrees go in; empty for what Settings say.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub worktrees: String,
    /// No language servers in this project's tasks.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub lsp_off: bool,
    /// Its tasks' pull requests are not asked of gh.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pr_off: bool,
    /// How many times in a row a check failed after its agent's turn goes
    /// back to it by itself, on this machine; 0: only by a click.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub check_to_agent: u32,
    /// Worktrees an agent made with `git worktree add` outside the places
    /// that tell (beside the repository), and the task whose agent did.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub made_by_agents: std::collections::BTreeMap<PathBuf, PathBuf>,
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
    /// A chat agent's choices for this task alone (a variant's model).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<(String, String)>,
    /// Its agents' sessions, the one it runs last.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<AgentSession>,
    /// The archive it was restored from, by key: archived again, it takes
    /// its place, unless that one keeps work the task did not get back.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub archive: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub archive_keep: bool,
    /// Its latest session recorded and never run: a new one, not resumed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fresh: bool,
    /// A deferred task come back on its own, not opened yet: what its agent
    /// is told when it is (workspace.rs, `TaskEntry::woke`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub woke: Option<String>,
    /// Questions asked aside of its agent: open, and closed ones as history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<QuestionState>,
    /// How many questions it was ever asked: the next one's number.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub asked: u32,
    /// Started by the agent of this task (workspace/crew.rs): it starts
    /// none of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_by: Option<PathBuf>,
}

/// A question asked of an agent of its own beside a task's agent.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct QuestionState {
    /// Its number in the task, never taken again: `q-<id>` in its reports.
    pub id: u32,
    /// What it was asked, then the name its agent for quick questions gave it.
    pub title: String,
    /// The profile that answers, and its session, once known.
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Unix seconds.
    pub created: u64,
    /// When it was closed; open while 0.
    #[serde(skip_serializing_if = "is_zero")]
    pub closed: u64,
    /// When its last message was, yours or its agent's answer: to the minute.
    #[serde(skip_serializing_if = "is_zero")]
    pub active: u64,
    /// The files its agent wrote, in the task's folder: what it takes along
    /// when it becomes a task of its own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub wrote: Vec<String>,
    /// Its agent ended with a pause of its task: it starts again when the
    /// question is opened, not with the task.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub asleep: bool,
    /// Its agent has answered and the answer was not looked at.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
    /// Asked by the agent of this task (crew.rs): its answers are that
    /// agent's to read, not yours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_by: Option<PathBuf>,
    /// Its agent's last answer, to read from its row.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub answer: String,
    /// When its tab was last brought up: how recent it is among the tasks.
    #[serde(skip_serializing_if = "is_zero")]
    pub opened: u64,
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
    /// The same for the lower panes of the left and right columns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_below_active: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_below_active: Option<usize>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focus_below: bool,
    /// Folders open in the file tree.
    pub expanded: Vec<String>,
    /// The panel in the side's upper half: 0 Changes, 1 Files, 2 Structure,
    /// 3 Run; none when `side_closed`. And the one in its lower half.
    pub side_panel: u8,
    pub side_closed: bool,
    pub side_below: Option<u8>,
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
    /// Times it went to the agent; 0: not yet. It stays after, until you
    /// are done with it.
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub sent: u32,
    /// After the agent's turn: whether the code it is on is gone or
    /// changed (true), or still there as it was (false), moved to where it
    /// is now. None: not looked at since it was sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
}

#[cfg(test)]
#[test]
fn a_review_note_is_found_where_its_code_went() {
    let note = ReviewNote { code: "let a = 1;\nlet b = 2;\n".into(), line: 8, sent: 1, ..Default::default() };
    // Moved down by lines written above it; the nearer of two copies.
    assert_eq!(note.find_in("x\nlet a = 1;\nlet b = 2;\ny\nz\nw\nq\nlet a = 1;\nlet b = 2;\n"), Some(9));
    assert_eq!(note.find_in("fn f() {}\n"), None, "changed: not there");
    assert!(!note.open() && ReviewNote { changed: Some(false), ..note.clone() }.open());
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

impl ReviewNote {
    /// To go with the next Send: never sent, or sent and its code is still
    /// as it was after the agent's turn.
    pub fn open(&self) -> bool {
        self.sent == 0 || self.changed == Some(false)
    }

    /// Where its code is in `text` (the file now): the place nearest to
    /// where it was, its end line; none when the code is no longer there.
    pub fn find_in(&self, text: &str) -> Option<usize> {
        let code = self.code.trim_end_matches('\n');
        if code.trim().is_empty() {
            return None;
        }
        let span = code.matches('\n').count();
        text.match_indices(code)
            .map(|(at, _)| text.as_bytes().iter().take(at).filter(|&&b| b == b'\n').count() + 1 + span)
            .min_by_key(|end| end.abs_diff(self.line))
    }
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
    /// In its column's lower pane.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub below: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preview: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// How a Web tab shows its page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebLook>,
    /// A file shown drawn, beside its text or instead of it: "split", "preview".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<String>,
    /// What is folded in a file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folds: Vec<SavedFold>,
}

/// A fold by its first line, and that line's words: the file may have
/// changed by the time it opens again.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
pub struct SavedFold {
    pub line: u32,
    pub text: String,
}

/// A Web tab's screen, colors and DevTools, kept with the session.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct WebLook {
    /// A screen's name, as the tab's menu lists it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub turned: bool,
    /// "light", "dark", or empty for the IDE's.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub colors: String,
    /// "bottom" or "right" where DevTools is open.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub devtools: String,
}

/// Debug builds are a separate app: their own data, sockets, window class
/// and launcher, so a dev build never touches the release one's tasks.
pub use ideae_core::paths::APP_ID;
pub use ideae_core::paths::{now, runtime_dir};

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

pub fn data_dir() -> PathBuf {
    data_home().join(APP_ID)
}

/// Where the state is, and what is kept next to it.
pub fn state_dir() -> PathBuf {
    state_path().parent().map(PathBuf::from).unwrap_or_else(data_dir)
}

pub fn state_path() -> PathBuf {
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

fn is_zero<T: Default + PartialEq>(n: &T) -> bool {
    *n == T::default()
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
