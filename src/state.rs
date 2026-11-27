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
    /// The Tabs panel is of the upper half: apart from `side_lower`, which
    /// builds of before it wrote without it.
    pub side_tabs_above: bool,
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
    /// By its name, as older versions read it; and by its id, which a
    /// rename does not change.
    pub space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    pub power_saving: bool,
    pub window: Option<WindowPlace>,
    /// What was chosen of them alone (`Settings::written`).
    #[serde(with = "chosen_only")]
    pub settings: Settings,
    pub active_task: Option<PathBuf>,
    /// Text size per kind of view.
    pub zoom: crate::zoom::Zoom,
    pub projects: Vec<ProjectState>,
    /// Agents tasks' agents started, by the ids they were given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crew: Vec<CrewRecord>,
    /// How many times each shortcut was done without its keys (learn.rs).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub learn_counts: std::collections::BTreeMap<String, u32>,
    /// The machine's store's rev this was written at (crate::machine): a
    /// state.json without it was written by an older build since.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub machine_rev: u64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct PresentingState {
    pub kept: Vec<PathBuf>,
    pub since: u64,
}

impl State {
    /// Spaces get ids, and projects and the space in the window are found
    /// by them, else by name (a state of before ids, or one an older
    /// version wrote, which keeps names alone); a project's old fields
    /// changed by an older version go into its layer.
    pub fn normalize_layers(&mut self) {
        for at in 0..self.spaces.len() {
            let taken: Vec<String> = self.spaces.iter().map(|s| s.id.clone()).collect();
            if self.spaces[at].id.is_empty() || taken[..at].contains(&self.spaces[at].id) {
                let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
                self.spaces[at].id = crate::layers::new_space_id(&taken);
            }
        }
        let spaces = &self.spaces;
        let find = |id: &Option<String>, name: &Option<String>| -> Option<String> {
            id.as_ref()
                .and_then(|id| spaces.iter().find(|s| &s.id == id && name.as_ref().is_none_or(|n| n == &s.name)))
                .or_else(|| name.as_ref().and_then(|name| spaces.iter().find(|s| &s.name == name)))
                .map(|s| s.id.clone())
        };
        self.space_id = find(&self.space_id, &self.space);
        for p in &mut self.projects {
            p.space_id = find(&p.space_id, &p.space);
            let old = crate::layers::OldFields { agent: p.agent.clone(), base: p.base.clone(), worktrees: p.worktrees.clone(), lsp_off: p.lsp_off, pr_off: p.pr_off, check_to_agent: p.check_to_agent };
            old.reconcile(&mut p.settings);
        }
    }

    /// Whether a task is one to tell of, as the window was left: of the
    /// space that was in it, and not left out of a presentation.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn tells_of(&self, task: &std::path::Path) -> bool {
        let project = self.projects.iter().find(|p| p.tasks.iter().any(|t| t.path == task));
        let in_space = match (&self.space_id, project.and_then(|p| p.space_id.as_ref())) {
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
    /// Trackers and code hosts for every project (crate::integrations),
    /// the Global layer of the list a space and a project add to.
    #[serde(default)]
    pub integrations: Vec<crate::layers::Entry>,
    /// Tickets off: no marks, questions, variables or tools of tickets,
    /// and the integrations of a team's file stay silent.
    #[serde(default = "yes")]
    pub tickets: bool,
    /// Global's layer of how a ticket's task names its branch, its request's
    /// title, and what the request says of its tickets (crate::layers).
    #[serde(default)]
    pub branch_template: String,
    #[serde(default)]
    pub pr_title: String,
    #[serde(default)]
    pub pr_closes: String,
    /// Hosts of dev stands a new task's browser is signed in to besides
    /// those found (browser/seed.rs): Global's layer of `stand_hosts`.
    #[serde(default)]
    pub stand_hosts: String,
    /// Global's layer of `links_open_in`: system, tab; empty for the default.
    #[serde(default)]
    pub links_open_in: String,
    /// Sites a task's links always open in the system's browser, whatever
    /// `links_open_in` says: ones the user's own browser cannot sign in to.
    #[serde(default)]
    pub system_hosts: Vec<String>,
    /// The agents to choose from (agents/mod.rs); none, and tasks get a
    /// plain shell. `--agent` and `--no-agent` on the command line win.
    #[serde(default)]
    pub agents: Vec<crate::agents::Profile>,
    /// The one new tasks run, by its id.
    #[serde(default)]
    pub default_agent: String,
    /// The one asked one-shot questions (commit messages, the variants'
    /// judge); empty for the default.
    #[serde(default)]
    pub quick_agent: String,
    /// Language servers (program names) never started.
    pub lsp_off: Vec<String>,
    /// Where tasks' worktrees go, a folder of each project's name in it;
    /// empty: beside each repository.
    pub worktrees_dir: String,
    /// The hints said no more of, in every project ("check", "setup").
    pub hints_off: Vec<String>,
    /// For a language with more than one server (`lsp::ALTERNATIVES`), the
    /// one it runs, by the language's key; none, the first.
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
    /// `.http` files: seconds a request may take, whether it follows
    /// redirects, checks the server's certificate and runs its scripts.
    pub http_timeout_secs: u32,
    pub http_follow_redirects: bool,
    pub http_verify_tls: bool,
    pub http_scripts: bool,
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
    /// The side has a Tabs panel: the task's tabs listed one under another.
    pub tabs_in_side: bool,
    /// While that panel is in view, the panes have no row of tabs: a small
    /// label over each tells what it shows.
    pub no_tab_row: bool,
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
    pub split_shares: std::collections::BTreeMap<String, f32>,
    /// How a file is shown when it opens, by what it is drawn as, as last
    /// chosen: "code", "split", "preview".
    pub file_looks: std::collections::BTreeMap<String, String>,
    /// Changed files as a list of paths, not a tree of folders: in the
    /// Changes panel and beside a diff.
    pub changes_flat: bool,
    /// A page of the task's own site is loaded again when its agent's turn ends.
    pub web_reload_after_turn: bool,
    /// An agent on another machine drives the task's Chrome here: its
    /// DevTools port is reached there, where that machine's other users
    /// reach it too. Off unless asked for.
    pub remote_browser: bool,
    /// Motion in Web tabs: "reduce" (a stream of frames slows with each
    /// animation), "full", or "" as the system asks.
    pub web_motion: String,
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
    /// Agents' own files (`~/.kimi-code/config.toml`) not to be offered
    /// ideae's hooks again (agents/own_config.rs).
    #[serde(default)]
    pub agent_hooks_declined: Vec<String>,
    /// Edit prediction by a local model (predict/): None until the offer
    /// to turn it on is answered.
    pub edit_prediction: Option<bool>,
    /// "auto", or the id of a model (predict::assets::Model).
    pub prediction_model: String,
    /// Linux and Windows: the menu names stay in the title bar; off, they are behind ☰.
    pub menu_bar_shown: bool,
    /// The settings chosen, by their names in the state: written even at
    /// their default, which the others follow when it changes. ↺ takes
    /// one out (`Settings::reset`).
    #[serde(skip)]
    pub chosen: std::collections::BTreeSet<String>,
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
            integrations: Vec::new(),
            tickets: true,
            branch_template: String::new(),
            pr_title: String::new(),
            pr_closes: String::new(),
            stand_hosts: String::new(),
            links_open_in: String::new(),
            system_hosts: Vec::new(),
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
            agents_start_agents: true,
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
            tabs_in_side: false,
            no_tab_row: false,
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
            http_timeout_secs: 60,
            http_follow_redirects: true,
            http_verify_tls: true,
            http_scripts: true,
            diff_unified: false,
            diff_ignore_whitespace: false,
            diff_files: false,
            changes_flat: false,
            web_reload_after_turn: true,
            remote_browser: false,
            web_motion: "reduce".into(),
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
            agent_hooks_declined: Vec::new(),
            edit_prediction: None,
            prediction_model: "auto".into(),
            menu_bar_shown: false,
            chosen: Default::default(),
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

/// In the state's settings: the keys chosen, of a build that keeps what was
/// chosen (beside them only the Machine layer, for a daemon there). A
/// state without it has every setting, the defaults of then among them.
const CHOSEN_ONLY: &str = "chosen_only";

/// Defaults of the builds that wrote every setting, where they are not
/// the ones now: a state of then at one of them had it by default.
const DEFAULTS_BEFORE: &[(&str, fn() -> serde_json::Value)] = &[("agents_start_agents", || false.into())];

type Fields = serde_json::Map<String, serde_json::Value>;

fn fields(settings: &Settings) -> Fields {
    match serde_json::to_value(settings) {
        Ok(serde_json::Value::Object(all)) => all,
        _ => Fields::new(),
    }
}

impl Settings {
    pub fn is_chosen(&self, key: &str) -> bool {
        self.chosen.contains(key)
    }

    /// As the state keeps them: what was chosen, and what is not as by
    /// default (set by no hand: a state's of before, normalized).
    pub fn written(&self) -> serde_json::Result<serde_json::Value> {
        let serde_json::Value::Object(mut all) = serde_json::to_value(self)? else {
            return Err(serde::ser::Error::custom("the settings are not a map"));
        };
        let default = fields(&Settings::default());
        let chosen: Vec<String> = all.iter().filter(|(k, v)| self.chosen.contains(*k) || default.get(*k) != Some(*v)).map(|(k, _)| k.clone()).collect();
        // The Machine layer and the profiles whole: a daemon there makes its store of state.json
        // (StoreOpenHere), knowing no defaults. Read back, they are not chosen for being there.
        let machine = |k: &str| k == "agents" || ideae_core::store::migrate::MACHINE_FIELDS.contains(&k);
        all.retain(|k, _| chosen.contains(k) || machine(k));
        all.insert(CHOSEN_ONLY.into(), chosen.into());
        Ok(serde_json::Value::Object(all))
    }

    /// Read from the state: what it has is chosen; of a state of before,
    /// what was not as by default then (`DEFAULTS_BEFORE`), the others as
    /// by default now.
    pub fn read(value: serde_json::Value) -> Result<Settings, serde_json::Error> {
        let serde_json::Value::Object(mut given) = value else { return serde_json::from_value(value) };
        let listed = given.remove(CHOSEN_ONLY);
        if let Some(serde_json::Value::Array(chosen)) = &listed {
            let chosen: Vec<&str> = chosen.iter().filter_map(serde_json::Value::as_str).collect();
            given.retain(|k, _| chosen.contains(&k.as_str()));
        } else if listed.is_none() {
            let before: Vec<(String, serde_json::Value)> = DEFAULTS_BEFORE.iter().map(|(k, v)| (k.to_string(), v())).collect();
            for (k, v) in &before {
                given.entry(k.clone()).or_insert_with(|| v.clone());
            }
            let mut then: Settings = serde_json::from_value(serde_json::Value::Object(given))?;
            then.normalize();
            let mut by_default = fields(&Settings::default());
            by_default.extend(before);
            given = fields(&then).into_iter().filter(|(k, v)| by_default.get(k) != Some(v)).collect();
        }
        // What is not there is as by default, not as an older state's missing field reads.
        let mut all = fields(&Settings::default());
        let chosen = given.keys().cloned().collect();
        all.extend(given);
        let mut settings: Settings = serde_json::from_value(serde_json::Value::Object(all))?;
        settings.chosen = chosen;
        Ok(settings)
    }

    /// One setting back to its default, not chosen any more.
    pub fn reset(&mut self, key: &str) {
        self.chosen.remove(key);
        let mut all = fields(self);
        match fields(&Settings::default()).remove(key) {
            Some(default) => all.insert(key.into(), default),
            None => all.remove(key),
        };
        if let Ok(mut back) = serde_json::from_value::<Settings>(serde_json::Value::Object(all)) {
            back.chosen = std::mem::take(&mut self.chosen);
            back.lsp_licence = self.lsp_licence.take();
            // A default agent of the defaults may be a profile removed since.
            back.normalize();
            *self = back;
        }
    }

    /// What changed since `old` is chosen, but what was taken out of the
    /// chosen since (reset).
    fn choose_changed(&mut self, old: &Settings) {
        if self == old {
            return;
        }
        let (now, was) = (fields(self), fields(old));
        let keys: std::collections::BTreeSet<&String> = now.keys().chain(was.keys()).collect();
        for key in keys {
            if now.get(key) != was.get(key) && !(old.chosen.contains(key) && !self.chosen.contains(key)) {
                self.chosen.insert(key.clone());
            }
        }
    }

    /// Those of `machine` (the store's Machine layer, every field it keeps)
    /// taken in: one another window changed is chosen, or not when it is at
    /// its default now; the others keep their marks.
    pub fn take_machine(&mut self, machine: &Fields) -> serde_json::Result<()> {
        let default = fields(&Settings::default());
        let mut all = fields(self);
        let changed: Vec<&String> = machine.iter().filter(|(k, v)| all.get(*k) != Some(*v)).map(|(k, _)| k).collect();
        for (k, v) in machine {
            all.insert(k.clone(), v.clone());
        }
        let mut taken: Settings = serde_json::from_value(serde_json::Value::Object(all))?;
        taken.chosen = std::mem::take(&mut self.chosen);
        taken.lsp_licence = self.lsp_licence.take();
        for k in changed {
            if default.get(k) == machine.get(k) {
                taken.chosen.remove(k);
            } else {
                taken.chosen.insert(k.clone());
            }
        }
        *self = taken;
        Ok(())
    }
}

/// The state's settings: what was chosen alone, every field when read.
mod chosen_only {
    use super::Settings;
    use serde::{Deserialize as _, Serialize as _};

    pub fn serialize<S: serde::Serializer>(settings: &Settings, to: S) -> Result<S::Ok, S::Error> {
        settings.written().map_err(serde::ser::Error::custom)?.serialize(to)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(from: D) -> Result<Settings, D::Error> {
        Settings::read(serde_json::Value::deserialize(from)?).map_err(serde::de::Error::custom)
    }
}

/// The state as the machine's store takes it in (crate::machine): every
/// setting it keeps, at its default too, not only those chosen.
pub fn for_store(state: &State) -> serde_json::Result<serde_json::Value> {
    let mut value = serde_json::to_value(state)?;
    value["settings"] = serde_json::to_value(&state.settings)?;
    Ok(value)
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

/// The settings in force from now; what changed of them is chosen, but
/// what `settings` was reset of (chosen before, not now).
pub fn set_settings(mut settings: Settings) {
    settings.normalize();
    let mut current = SETTINGS.write().unwrap();
    if let Some(old) = current.as_ref() {
        settings.choose_changed(old);
    }
    *current = Some(settings);
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
    /// The space it is sorted into, by its name (for older versions) and
    /// by its id; none, and it is in every one.
    pub space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    /// Its own layer of the settings (crate::layers). `agent`, `base`,
    /// `worktrees`, `lsp_off`, `pr_off` and `check_to_agent` below are
    /// written from it for older versions, and read into it when an older
    /// version changed them.
    #[serde(skip_serializing_if = "crate::layers::Overrides::is_empty")]
    pub settings: crate::layers::Overrides,
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
    /// Its tickets, its request and the addresses it was given; the first
    /// ticket is its own (the PR's title, `IDEAE_TICKET`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<TaskLink>,
    /// Its links were read once out of its prompt and branch (a task of
    /// before links): not again, or one removed would come back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub links_read: bool,
    /// On a colleague's request (made from its link): to review, not yours.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub review: bool,
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
pub use ideae_core::store::records::{AgentSession, CrewRecord, QuestionState, ReviewNote, TaskLink};
pub use ideae_core::paths::APP_ID;
pub use ideae_core::paths::{now, runtime_dir};

pub use ideae_core::paths::data_home;

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
            state.normalize_layers();
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
        let mut back: State = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(back.settings.is_chosen("default_agent"), "what is not as by default is written, so chosen");
        back.settings.chosen.clear();
        assert!(back == state);
        // What an older state has not: none of it.
        let old: TaskState = serde_json::from_str(r#"{"path":"/w/t"}"#).unwrap();
        assert!(old.sessions.is_empty() && old.agent.is_empty());
    }
}

#[cfg(test)]
mod chosen_tests {
    use super::*;

    fn written(settings: &Settings) -> serde_json::Value {
        serde_json::to_value(State { settings: settings.clone(), ..Default::default() }).unwrap()["settings"].clone()
    }

    fn read(settings: serde_json::Value) -> Settings {
        serde_json::from_value::<State>(serde_json::json!({ "settings": settings })).unwrap().settings
    }

    /// A state of before kept every setting: those as by default then are
    /// nobody's, and follow the default now (agents starting agents, off
    /// then).
    #[test]
    fn a_state_of_before_keeps_what_differs_from_the_default() {
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["agents_start_agents"] = false.into();
        old["theme"] = "One".into();
        let settings = read(old);
        assert_eq!(settings.chosen.iter().map(String::as_str).collect::<Vec<_>>(), ["theme"]);
        assert!(settings.agents_start_agents && settings.theme == "One");
        // Turned on by hand then: as by default now, and chosen.
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["agents_start_agents"] = true.into();
        assert!(read(old).is_chosen("agents_start_agents"));
        // A field an older state had not reads as it did then: chosen when that is not the default.
        let older = read(serde_json::json!({ "agent": "claude" }));
        assert_eq!(older.chosen.iter().map(String::as_str).collect::<Vec<_>>(), ["agents_welcomed"]);
        assert!(older.agents_welcomed);
    }

    #[test]
    fn what_was_chosen_is_written_even_at_its_default() {
        let mut settings = Settings::default();
        settings.fetch_minutes = 5;
        settings.chosen.insert("fetch_minutes".into());
        settings.tab_limit = 20;
        let json = written(&settings);
        assert_eq!(json["chosen_only"], serde_json::json!(["fetch_minutes", "tab_limit"]));
        assert!(json.get("theme").is_none() && json.get("keep_awake").is_some(), "the Machine layer written whole, for a daemon there");
        let back = read(json);
        assert_eq!((back.fetch_minutes, back.tab_limit), (5, 20));
        assert_eq!(back.chosen.iter().map(String::as_str).collect::<Vec<_>>(), ["fetch_minutes", "tab_limit"]);
        // Chosen at a default that is empty: written all the same.
        let empty = Settings { chosen: ["links_open_in".to_string(), "tickets".to_string()].into(), ..Default::default() };
        let back = read(written(&empty));
        assert!(back.is_chosen("links_open_in") && back.is_chosen("tickets") && back.tickets);
        // Nothing chosen: the defaults when read, not an older state's.
        let mut old_default = written(&Settings::default());
        old_default["keep_awake"] = false.into();
        assert!(read(old_default).keep_awake, "a Machine field written, not chosen, follows the default");
        let fresh = read(written(&Settings::default()));
        assert!(fresh == Settings::default() && !fresh.agents_welcomed);
    }

    #[test]
    fn a_reset_follows_the_default_again() {
        let mut settings = Settings { tab_limit: 20, archive_keep_days: 30, ..Default::default() };
        settings.chosen.extend(["tab_limit".to_string(), "archive_keep_days".to_string()]);
        settings.reset("tab_limit");
        assert_eq!((settings.tab_limit, settings.archive_keep_days), (Settings::default().tab_limit, 30));
        assert!(!settings.is_chosen("tab_limit") && settings.is_chosen("archive_keep_days"));
        // Reset and saved: the change back is not taken for a choice.
        let old = Settings { tab_limit: 20, chosen: ["tab_limit".to_string()].into(), ..Default::default() };
        let mut now = old.clone();
        now.reset("tab_limit");
        now.archive_keep_days = 90;
        now.choose_changed(&old);
        assert!(!now.is_chosen("tab_limit"));
        // Changed to its default, it is chosen: it stays when the default changes.
        let old = Settings { archive_keep_days: 30, ..Default::default() };
        let mut now = Settings { archive_keep_days: Settings::default().archive_keep_days, ..old.clone() };
        now.choose_changed(&old);
        assert!(now.is_chosen("archive_keep_days") && written(&now).get("archive_keep_days").is_some());
    }

    /// The machine's settings from another window: one at its default is
    /// not chosen here, the store keeps every field of the Machine layer.
    #[test]
    fn the_machine_s_settings_are_taken_in() {
        let mut settings = Settings { lsp_idle_minutes: 60, chosen: ["lsp_idle_minutes".to_string(), "keep_awake".to_string()].into(), ..Default::default() };
        let machine = serde_json::json!({ "lsp_idle_minutes": Settings::default().lsp_idle_minutes, "worktrees_dir": "~/wt", "keep_awake": true });
        settings.take_machine(machine.as_object().unwrap()).unwrap();
        assert_eq!((settings.lsp_idle_minutes, settings.worktrees_dir.as_str()), (Settings::default().lsp_idle_minutes, "~/wt"));
        assert_eq!(settings.chosen.iter().map(String::as_str).collect::<Vec<_>>(), ["keep_awake", "worktrees_dir"], "one unchanged keeps its mark");
        let state = State { settings: Settings::default(), ..Default::default() };
        let store = ideae_core::store::migrate::from_state(&for_store(&state).unwrap()).unwrap();
        assert_eq!(store.settings["keep_awake"], true, "every field of it, not only those chosen");
        assert!(!store.profiles.is_empty());
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
        let space = |name: &str| crate::workspace::spaces::Space { name: name.into(), ..Default::default() };
        let mut state = State { spaces: vec![space("Work"), space("Home")], projects: vec![project(Some("Work"), "/w/a"), project(Some("Home"), "/h/b"), project(None, "/n/c")], ..Default::default() };
        state.normalize_layers();
        let told = |state: &State| ["/w/a", "/h/b", "/n/c", "/elsewhere"].map(|path| state.tells_of(Path::new(path)));
        assert_eq!(told(&state), [true, true, true, true], "all of them in the window");
        state.space_id = state.spaces.iter().find(|s| s.name == "Work").map(|s| s.id.clone());
        assert_eq!(told(&state), [true, false, true, true], "one sorted into no space is in every one");
        state.presenting = Some(PresentingState { kept: vec![PathBuf::from("/n/c")], since: 0 });
        assert_eq!(told(&state), [false, false, true, false], "and of those, the ones chosen to present");
    }
}

#[cfg(test)]
mod layers_tests {
    use super::{ProjectState, State};
    use crate::layers::{OldFields, Overrides};

    /// A project of an older version, as it read and wrote the state:
    /// unknown fields dropped on the way.
    #[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq)]
    #[serde(default)]
    struct OldProject {
        path: std::path::PathBuf,
        space: Option<String>,
        agent: Option<String>,
        base: String,
        worktrees: String,
        lsp_off: bool,
        pr_off: bool,
        check_to_agent: u32,
    }

    #[derive(serde::Serialize, serde::Deserialize, Default, Debug)]
    #[serde(default)]
    struct OldSpace {
        name: String,
        icon: String,
        color: u8,
    }

    #[derive(serde::Serialize, serde::Deserialize, Default, Debug)]
    #[serde(default)]
    struct OldState {
        spaces: Vec<OldSpace>,
        space: Option<String>,
        projects: Vec<OldProject>,
    }

    fn load(json: &str) -> State {
        let mut state: State = serde_json::from_str(json).unwrap();
        state.normalize_layers();
        state
    }

    /// What the window writes of a project: its layer, and the old fields from it.
    fn written(p: &ProjectState) -> ProjectState {
        let old = OldFields::from_overrides(&p.settings);
        ProjectState { agent: old.agent, base: old.base, worktrees: old.worktrees, lsp_off: old.lsp_off, pr_off: old.pr_off, check_to_agent: old.check_to_agent, ..p.clone() }
    }

    #[test]
    fn a_state_of_before_layers_is_read_and_an_older_version_reads_what_is_written() {
        let before = r#"{"spaces":[{"name":"Work","icon":"W","color":1}],"space":"Work",
            "projects":[{"path":"/w/api","space":"Work","agent":"codex","base":"develop","pr_off":true,"tasks":[]}]}"#;
        let state = load(before);
        let id = state.spaces[0].id.clone();
        assert!(!id.is_empty());
        assert_eq!((state.space_id.as_deref(), state.projects[0].space_id.as_deref()), (Some(id.as_str()), Some(id.as_str())));
        assert_eq!(
            state.projects[0].settings,
            Overrides { agent: Some("codex".into()), base: Some("develop".into()), pull_requests: Some(false), ..Default::default() }
        );

        // Written again, with a space's setting and a rename: an older version reads names and its own fields.
        let mut state = state;
        state.spaces[0].name = "Job".into();
        state.spaces[0].settings.agent = Some("claude".into());
        state.space = Some("Job".into());
        state.projects[0].space = Some("Job".into());
        state.projects[0].settings.language_servers = Some(false);
        state.projects[0] = written(&state.projects[0]);
        let json = serde_json::to_string(&state).unwrap();
        let old: OldState = serde_json::from_str(&json).unwrap();
        assert_eq!(old.space.as_deref(), Some("Job"));
        assert_eq!(
            old.projects[0],
            OldProject { path: "/w/api".into(), space: Some("Job".into()), agent: Some("codex".into()), base: "develop".into(), lsp_off: true, pr_off: true, ..Default::default() }
        );
        // And reads it back the same.
        let again = load(&json);
        assert_eq!((again.spaces[0].id.as_str(), again.projects[0].settings.clone()), (id.as_str(), state.projects[0].settings.clone()));

        // What the older version writes in turn: ids and layers gone, names kept; its changes are taken.
        let mut old = old;
        old.projects[0].pr_off = false;
        let back = load(&serde_json::to_string(&old).unwrap());
        assert_eq!(back.projects[0].space_id, back.space_id, "found by name again");
        assert_eq!(back.spaces[0].name, "Job");
        assert_eq!(back.projects[0].settings.pull_requests, None, "turned on again by the older version");
        assert_eq!(back.projects[0].settings.language_servers, Some(false));
    }

    #[test]
    fn a_project_whose_space_id_is_gone_is_found_by_name() {
        let json = r#"{"spaces":[{"id":"s1","name":"Work"}],"projects":[{"path":"/a","space":"Work","space_id":"gone"},{"path":"/b","space":"Nowhere"}]}"#;
        let state = load(json);
        assert_eq!(state.projects[0].space_id.as_deref(), Some("s1"));
        assert_eq!(state.projects[1].space_id, None, "a space that is not there: in none");
    }
}
