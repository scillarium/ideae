//! The commands on a task, its project, its variants and its git, with
//! their names and what allows them: one model for the macOS menu bar, the
//! sidebar's menus and the command palette.

use gpui_kit::{App, Context, Window};

use crate::task::GitCommand;
use crate::workspace::Workspace;

/// What is done to the task in hand.
#[derive(Clone, Copy, PartialEq)]
pub enum TaskCommand {
    RestartAgent,
    NewTerminal,
    NewBrowserTab,
    NewUserBrowserTab,
    ForgetUserBrowser,
    StartServices,
    StopServices,
    Star,
    Rename,
    CopyBranchName,
    CopyTitle,
    AnotherVariant,
    KeepVariant,
    Pause,
    Archive,
    Defer,
    RemoveSide,
    CompareWithBranch,
    CompareThisFile,
}

impl TaskCommand {
    pub fn name(self, starred: bool) -> &'static str {
        use TaskCommand::*;
        match self {
            RestartAgent => "Restart Agent",
            NewTerminal => "New Terminal",
            NewBrowserTab => "New Browser Tab",
            NewUserBrowserTab => "New Tab of Your Own Browser",
            ForgetUserBrowser => "Forget the Logins of Your Own Browser",
            StartServices => "Start All in Run Panel",
            StopServices => "Stop All in Run Panel",
            Star if starred => "Unstar Task",
            Star => "Star Task",
            Rename => "Rename Task…",
            CopyBranchName => "Copy Branch Name",
            CopyTitle => "Copy Title",
            AnotherVariant => "Another Variant…",
            KeepVariant => "Keep Only This Variant…",
            Pause => "Pause Task",
            Archive => "Archive Task",
            Defer => "Defer Task…",
            RemoveSide => "Remove Agent's Worktree",
            CompareWithBranch => "Compare with Branch, Tag, or Commit…",
            CompareThisFile => "Compare This File with…",
        }
    }

    pub fn hint(self) -> &'static str {
        use TaskCommand::*;
        match self {
            AnotherVariant => "task: one more agent tries it, in its own worktree",
            KeepVariant => "task: the other variants go",
            StartServices => "task: all of .ideae.toml",
            Pause => "task: its agent and terminals end, the task stays",
            Archive => "task: it goes into the archive, to be restored",
            Defer => "task: archived and kept, back on a day you choose",
            RemoveSide => "task: a worktree the agent made for itself",
            _ => "task",
        }
    }

    /// Opens a dialog: not over one that is already up. Pausing, archiving
    /// or removing may ask first.
    pub fn opens(self) -> bool {
        use TaskCommand::*;
        matches!(self, Rename | AnotherVariant | KeepVariant | Pause | Archive | Defer | RemoveSide | CompareWithBranch | CompareThisFile)
    }
}

/// What is done to the project in hand, or to the window's spaces.
#[derive(Clone, Copy, PartialEq)]
pub enum ProjectCommand {
    Remove,
    ArchiveMerged,
    EditSpaces,
    Review,
}

impl ProjectCommand {
    pub fn name(self) -> &'static str {
        match self {
            ProjectCommand::Remove => "Remove Project from List…",
            ProjectCommand::ArchiveMerged => "Archive Merged Tasks…",
            ProjectCommand::EditSpaces => "Edit Spaces…",
            ProjectCommand::Review => "Review…",
        }
    }
}

/// What is done to the variants in hand: the task's, or those side by side.
#[derive(Clone, Copy, PartialEq)]
pub enum GroupCommand {
    Show,
    Judge,
    Stop,
    Discard,
}

impl GroupCommand {
    pub fn name(self) -> &'static str {
        match self {
            GroupCommand::Show => "Show All Variants",
            GroupCommand::Judge => "Have an Agent Judge the Variants",
            GroupCommand::Stop => "Stop All Variants…",
            GroupCommand::Discard => "Discard All Variants…",
        }
    }
}

pub fn git_name(what: GitCommand) -> &'static str {
    match what {
        GitCommand::Commit => "Commit…",
        GitCommand::WriteMessage => "Generate Commit Message",
        GitCommand::Stash => "Stash Changes",
        GitCommand::Unstash => "Pop Latest Stash of This Branch",
        GitCommand::Fetch => "Fetch",
        GitCommand::Pull => "Pull",
        GitCommand::Rebase => "Rebase onto Base Branch",
        GitCommand::Push => "Push",
        GitCommand::PullRequest => "Create or Open Pull Request",
    }
}

pub fn git_hint(what: GitCommand) -> &'static str {
    match what {
        GitCommand::Commit => "git: the message box, to commit from",
        GitCommand::WriteMessage => "git: the agent writes it",
        GitCommand::Stash => "git: everything not committed",
        GitCommand::PullRequest => "git: created if there is none",
        _ => "git",
    }
}

/// "Rename Task…" as the command palette and the sidebar write it: "Rename task…".
pub fn sentence(name: &str) -> String {
    let mut words = name.split(' ');
    let first = words.next().unwrap_or_default().to_string();
    // An acronym stays as it is: "PR", "URL".
    words.fold(first, |out, word| {
        let acronym = word.len() > 1 && word.chars().all(|c| !c.is_lowercase());
        out + " " + &if acronym { word.to_string() } else { word.to_lowercase() }
    })
}

/// A task, as far as its menus depend on it: the menu bar's, the sidebar's,
/// the command palette's.
#[derive(Clone, Copy, PartialEq)]
pub struct InHand {
    pub(crate) main: bool,
    /// On another machine: what reads or writes its files here is not there yet.
    pub(crate) remote: bool,
    /// A worktree an agent made for itself: no variants, removed rather than archived.
    pub(crate) side: bool,
    /// Not at work on something that a variant or a delete would get in the way of.
    pub(crate) ready: bool,
    pub(crate) grouped: bool,
    pub(crate) starred: bool,
    pub(crate) open: bool,
    /// Started with an agent: its tab is there.
    pub(crate) agent: bool,
    /// Its agent's program is not running: typed into its shell, a restart does no harm.
    pub(crate) agent_ended: bool,
    /// The front tab can be closed (the agent's cannot), a file is in front,
    /// a place back and forward, a tab to reopen.
    pub(crate) closable: bool,
    pub(crate) file: bool,
    pub(crate) back: bool,
    pub(crate) forward: bool,
    pub(crate) reopen: bool,
    pub(crate) services: bool,
    pub(crate) services_running: bool,
    pub(crate) services_waiting: bool,
    pub(crate) git: [bool; GitCommand::ALL.len()],
    /// Which of Agent, Changes, History and Shell is the front tab; which side panel shows.
    pub(crate) front: Option<crate::task::FrontTab>,
    pub(crate) panels: [bool; 7],
}

impl InHand {
    /// What the entry alone tells, for a row of the sidebar: not what its
    /// agent, services and git have to do.
    pub fn basic(t: &crate::workspace::TaskEntry) -> Self {
        InHand {
            main: t.main,
            remote: crate::hosts::is_remote(&t.path),
            side: t.side.is_some(),
            ready: t.busy.is_none(),
            grouped: !t.group.is_empty(),
            starred: t.starred,
            open: t.view.is_some(),
            agent: false,
            agent_ended: false,
            closable: false,
            file: false,
            back: false,
            forward: false,
            reopen: false,
            services: false,
            services_running: false,
            services_waiting: false,
            git: [false; GitCommand::ALL.len()],
            front: None,
            panels: [false; 7],
        }
    }

    pub(crate) fn of(t: &crate::workspace::TaskEntry, cx: &App) -> Self {
        let Some(view) = t.view.as_ref().map(|v| v.read(cx)) else { return Self::basic(t) };
        let (running, waiting) = view.services_running_and_waiting(cx);
        let crate::task::TabsState { closable, file, back, forward, reopen, front, panels } = view.tabs_state();
        InHand {
            closable,
            file,
            front,
            panels,
            back,
            forward,
            reopen,
            agent: view.has_agent(),
            agent_ended: view.agent_restartable(cx),
            services: view.has_services(),
            services_running: running,
            services_waiting: waiting,
            git: GitCommand::ALL.map(|what| view.git_can(what)),
            ..Self::basic(t)
        }
    }

    /// The main thread is not archived, a task at work gets no
    /// variant and is not archived, a paused one has no agent.
    pub fn allows(&self, what: TaskCommand) -> bool {
        use TaskCommand::*;
        match what {
            Star | CopyBranchName | CopyTitle | ForgetUserBrowser => true,
            Rename => true,
            Archive | Defer | AnotherVariant => !self.main && !self.side && self.ready,
            RemoveSide => self.side && self.ready,
            KeepVariant => !self.main && !self.side && self.ready && self.grouped,
            RestartAgent => self.open && self.agent_ended,
            // With nothing in .ideae.toml yet, the Run panel tells how to begin.
            StartServices => self.open && (!self.services || self.services_waiting),
            StopServices => self.services_running,
            CompareThisFile => self.file,
            CompareWithBranch => self.open,
            NewTerminal | NewBrowserTab | NewUserBrowserTab | Pause => self.open,
        }
    }

    pub(crate) fn git(&self, what: GitCommand) -> bool {
        self.open && GitCommand::ALL.iter().position(|w| *w == what).is_some_and(|i| self.git[i])
    }
}

impl Workspace {
    pub(crate) fn in_hand(&self, cx: &App) -> Option<InHand> {
        let Some(path) = &self.active else {
            // On the group's screen the variant chosen there, out of view: what needs its tabs is grey.
            let (pi, ti) = self.find(&self.card_in_hand()?)?;
            return Some(InHand { open: false, ..InHand::basic(&self.projects[pi].tasks[ti]) });
        };
        let (pi, ti) = self.find(path)?;
        Some(InHand::of(&self.projects[pi].tasks[ti], cx))
    }

    /// The card clicked last on the group's screen, while it is still one of
    /// the cards shown there; else the first of them.
    pub(crate) fn card_in_hand(&self) -> Option<std::path::PathBuf> {
        let shown: Vec<_> = self.members(self.active_group.as_deref()?).into_iter().filter(|&(pi, ti)| self.shows(pi, ti)).map(|(pi, ti)| &self.projects[pi].tasks[ti].path).collect();
        shown.iter().find(|p| Some(**p) == self.group_card.as_ref()).or(shown.first()).map(|p| (*p).clone())
    }

    /// The task's project, or on the variants' screen the group's.
    pub(crate) fn project_in_hand(&self) -> Option<usize> {
        self.active_project().or_else(|| self.find(&self.card_in_hand()?).map(|(pi, _)| pi))
    }

    pub fn task_allows(&self, what: TaskCommand, cx: &App) -> bool {
        self.in_hand(cx).is_some_and(|t| t.allows(what))
    }

    pub fn task_command(&mut self, what: TaskCommand, window: &mut Window, cx: &mut Context<Self>) {
        use TaskCommand::*;
        if !self.task_allows(what, cx) {
            return;
        }
        let Some(path) = self.active.clone().or_else(|| self.card_in_hand()) else { return };
        let view = self.active_view();
        let in_view = |f: fn(&mut crate::task::TaskView, &mut Window, &mut Context<crate::task::TaskView>), window: &mut Window, cx: &mut Context<Self>| {
            if let Some(view) = &view {
                view.update(cx, |t, cx| f(t, window, cx));
            }
        };
        match what {
            RestartAgent => in_view(crate::task::TaskView::restart_agent, window, cx),
            NewTerminal => in_view(crate::task::TaskView::new_terminal, window, cx),
            NewBrowserTab => in_view(crate::task::TaskView::new_browser, window, cx),
            NewUserBrowserTab => in_view(crate::task::TaskView::new_user_browser, window, cx),
            ForgetUserBrowser => in_view(crate::task::TaskView::forget_user_browser, window, cx),
            StartServices => in_view(
                |t, window, cx| {
                    t.leave_focus_mode(window, cx);
                    t.reveal_side_panel(3, cx);
                    t.start_services(window, cx);
                },
                window,
                cx,
            ),
            StopServices => in_view(crate::task::TaskView::stop_services, window, cx),
            CompareWithBranch => in_view(|t, window, cx| t.compare_with_dialog(None, window, cx), window, cx),
            CompareThisFile => in_view(crate::task::TaskView::compare_open_file, window, cx),
            Star => self.toggle_star(&path, cx),
            CopyTitle => {
                if let Some((pi, ti)) = self.find(&path) {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(self.task_title(pi, ti, cx)));
                }
            }
            CopyBranchName => {
                if let Some((pi, ti)) = self.find(&path) {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(self.projects[pi].tasks[ti].branch.clone()));
                }
            }
            Rename => self.rename_task_dialog(&path, window, cx),
            AnotherVariant => self.another_variant_dialog(&path, window, cx),
            KeepVariant => self.keep_variant_dialog(&path, window, cx),
            Pause => self.end_with_children(&path, false, window, cx),
            Archive => self.end_with_children(&path, true, window, cx),
            Defer => self.defer_task(&path, window, cx),
            RemoveSide => self.remove_side(&path, window, cx),
        }
    }

    pub(crate) fn project_command(&mut self, what: ProjectCommand, window: &mut Window, cx: &mut Context<Self>) {
        match (what, self.project_in_hand()) {
            (ProjectCommand::EditSpaces, _) => self.spaces_dialog(window, cx),
            (ProjectCommand::Remove, Some(project)) => self.remove_project(project, window, cx),
            (ProjectCommand::ArchiveMerged, Some(project)) => self.archive_merged(project, window, cx),
            (ProjectCommand::Review, Some(project)) => self.review_dialog(project, window, cx),
            _ => {}
        }
    }

    pub(crate) fn group_command(&mut self, what: GroupCommand, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self.group_in_hand().map(str::to_string) else { return };
        match what {
            GroupCommand::Show => self.open_group(&group, window, cx),
            // The verdict is told on the group's screen.
            GroupCommand::Judge => {
                if self.active_group.is_none() {
                    self.open_group(&group, window, cx);
                }
                self.judge_group(&group, window, cx)
            }
            GroupCommand::Stop => self.stop_group(&group, window, cx),
            GroupCommand::Discard => self.settle_group_dialog(&group, None, window, cx),
        }
    }

    pub(crate) fn group_in_hand(&self) -> Option<&str> {
        self.active_group.as_deref().or_else(|| {
            let (pi, ti) = self.find(self.active.as_ref()?)?;
            Some(self.projects[pi].tasks[ti].group.as_str()).filter(|g| !g.is_empty())
        })
    }

    /// The keyboard stays where it is; for Commit and its message the Changes
    /// panel comes up first.
    pub fn git_command(&mut self, what: GitCommand, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(task) = self.active_view() {
            task.update(cx, |t, cx| {
                // The others' answer is a notification while Changes is out of
                // view, as for the branch line's buttons.
                if matches!(what, GitCommand::Commit | GitCommand::WriteMessage) {
                    t.leave_focus_mode(window, cx);
                    t.reveal_side_panel(0, cx);
                }
                t.git_command(what, window, cx);
            });
        }
    }
}
