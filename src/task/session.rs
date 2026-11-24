use super::*;

// ---------------------------------------------------------------- session

/// The width of the right side panel, the same in every task.
pub struct SidePanelWidth(pub Pixels);
impl Global for SidePanelWidth {}
pub use strip::{Assets, SideLayout};

impl TaskView {
    /// The tabs and panels, to open the same way after a restart.
    pub fn session(&self, cx: &App) -> crate::state::TaskSession {
        use crate::state::{TabSession, TaskSession};
        let rel = |path: &std::path::Path| {
            path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().into_owned()
        };
        let tabs: Vec<TabSession> = self
            .tabs
            .iter()
            .map(|tab| {
                let mut s = TabSession { right: tab.pane.right, below: tab.pane.below, preview: tab.preview, pinned: tab.pinned, ..Default::default() };
                match &tab.kind {
                    // The task's first terminal: the agent, or the shell without one.
                    // Not started: a restart leaves it so.
                    // Asleep for having nothing to do, it wakes when shown; paused, it waits for a click.
                    TabKind::Asleep if tab.agent == Some(Who::Main) => s.kind = if self.agent_slept { "slept-agent" } else { "paused-agent" }.into(),
                    TabKind::Terminal(_) | TabKind::Chat(_) if tab.agent == Some(Who::Main) || tab.title == "shell" => s.kind = "agent".into(),
                    TabKind::Terminal(_) | TabKind::Chat(_) | TabKind::Asleep if tab.agent.is_some() => {
                        s.kind = "question".into();
                        // `line` holds its number, as a shell's.
                        if let Some(Who::Question(n)) = tab.agent {
                            s.line = n;
                        }
                    }
                    TabKind::Chat(_) | TabKind::Asleep => {}
                    TabKind::Terminal(view) if view.read(cx).service.is_some() => {
                        s.kind = "service".into();
                        s.path = view.read(cx).service.clone();
                    }
                    TabKind::Terminal(view) => {
                        s.kind = "shell".into();
                        s.line = view.read(cx).seq as u32;
                    }
                    TabKind::Changes => s.kind = "changes".into(),
                    TabKind::History => s.kind = "history".into(),
                    TabKind::File(file) => {
                        let f = file.read(cx);
                        s.kind = "file".into();
                        s.path = Some(rel(&f.path));
                        s.line = f.cursor_line;
                        s.look = f.preview.look.name().map(String::from);
                        s.folds = f.saved_folds(cx);
                    }
                    TabKind::Binary(view) => {
                        s.kind = "file".into();
                        s.path = Some(rel(&view.read(cx).path));
                    }
                    // Asked for once: the restart opens without it.
                    TabKind::Diff(_) => s.kind = "diff".into(),
                    TabKind::Browser(view) => {
                        s.kind = "browser".into();
                        s.url = Some(view.read(cx).url());
                        s.web = view.read(cx).look();
                    }
                }
                s
            })
            .collect();
        let index = |id: Option<usize>| id.and_then(|id| self.ix_of(id));
        TaskSession {
            tabs,
            left_active: index(self.active[0]),
            left_below_active: index(self.active[1]),
            right_active: index(self.active[2]),
            right_below_active: index(self.active[3]),
            focus_right: self.current.right,
            focus_below: self.current.below,
            expanded: self.expanded.iter().map(|e| e.to_string()).collect(),
            review: self.changes.read(cx).review.clone(),
            viewed: self.viewed.clone(),
            bookmarks: self.all_bookmarks(cx),
            side_panel: self.side[0].map_or(0, |pane| pane as u8),
            side_closed: self.side[0].is_none(),
            side_below: self.side[1].map(|pane| pane as u8),
        }
    }

    /// Opens the tabs of a saved session next to the agent's.
    pub fn restore(&mut self, session: &crate::state::TaskSession, window: &mut Window, cx: &mut Context<Self>) {
        let mut ids: Vec<Option<usize>> = Vec::new();
        // Before the files open: each takes its own.
        self.bookmarks = session.bookmarks.clone();
        for saved in &session.tabs {
            let before: HashSet<usize> = self.tabs.iter().map(|t| t.id).collect();
            match saved.kind.as_str() {
                "agent" | "paused-agent" | "slept-agent" => {
                    ids.push(self.tabs.iter().find(|t| t.agent == Some(Who::Main) || t.title == "shell").map(|t| t.id));
                    continue;
                }
                "question" => self.restore_question_tab(saved.line),
                // `line` holds the shell's number, for its session.
                "shell" if saved.line > 0 => {
                    self.terminal_tab(saved.line as usize, window, cx);
                }
                "shell" => {
                    self.new_terminal_tab(window, cx);
                }
                // Attaches to it in the daemon, or starts it again.
                "service" => {
                    if let Some(name) = &saved.path {
                        self.service_tab(name, window, cx);
                    }
                }
                "changes" => self.tabs.push(TabItem::new("Changes", TabKind::Changes)),
                "history" => self.tabs.push(TabItem::new("History", TabKind::History)),
                "file" => {
                    if let Some(path) = &saved.path {
                        let path = self.root.join(path);
                        if path.exists() {
                            self.open_file_as(path, saved.line, 0, OpenAs::Restored, window, cx);
                        }
                    }
                }
                "browser" => {
                    self.open_browser(saved.url.as_deref().unwrap_or("about:blank"), window, cx);
                    if let (Some(look), Some(TabKind::Browser(view))) = (&saved.web, self.tabs.last().map(|t| &t.kind)) {
                        view.update(cx, |v, cx| v.set_look(look, window, cx));
                    }
                }
                _ => {}
            }
            let new = self.tabs.iter_mut().find(|t| !before.contains(&t.id));
            if let (Some(look), Some(TabKind::File(file))) = (saved.look.as_deref().and_then(super::preview::Look::named), new.as_ref().map(|t| &t.kind)) {
                file.update(cx, |f, cx| f.set_look(look, cx));
            }
            if let (false, Some(TabKind::File(file))) = (saved.folds.is_empty(), new.as_ref().map(|t| &t.kind)) {
                file.update(cx, |f, cx| f.restore_folds(&saved.folds, cx));
            }
            ids.push(new.map(|tab| {
                tab.pane = Pane { right: saved.right, below: saved.below };
                tab.preview = saved.preview;
                tab.pinned = saved.pinned;
                tab.id
            }));
        }
        let pick = |ix: Option<usize>| ix.and_then(|ix| ids.get(ix).copied().flatten());
        let picked = [session.left_active, session.left_below_active, session.right_active, session.right_below_active].map(pick);
        for (shown, id) in self.active.iter_mut().zip(picked) {
            if id.is_some() {
                *shown = id;
            }
        }
        self.current = Pane { right: session.focus_right, below: session.focus_below };
        self.normalize();
        // In front and still running in the daemon: there, not its place.
        self.attach_questions_in_front(window, cx);
        self.expanded = session.expanded.iter().map(|e| SharedString::from(e.clone())).collect();
        self.viewed = session.viewed.clone();
        let review = session.review.clone();
        self.changes.update(cx, |c, cx| {
            c.review = review;
            cx.notify();
        });
        let pane = |ix: u8| strip::PANES[usize::from(ix).min(strip::PANES.len() - 1)];
        self.side = [(!session.side_closed).then(|| pane(session.side_panel)), session.side_below.map(pane)];
        self.keyed = self.side[0].or(self.side[1]).unwrap_or(RightPane::Changes);
        self.structure_file = self.focused_ix().filter(|&ix| matches!(self.tabs[ix].kind, TabKind::File(_))).map(|ix| self.tabs[ix].id);
        self.sync_visibility(cx);
        self.show_diff_if_seen(cx);
        cx.notify();
    }

    /// The daemon sessions of this task's terminals.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn sessions(&self, cx: &App) -> Vec<String> {
        self.all_tabs()
            .filter_map(|t| match &t.kind {
                TabKind::Terminal(view) => view.read(cx).session().map(str::to_string),
                TabKind::Chat(chat) => chat.read(cx).session().map(str::to_string),
                _ => None,
            })
            .chain(self.unshown_sessions())
            .collect()
    }

    /// The agent is in the middle of a turn.
    pub fn agent_busy(&self) -> bool {
        self.agent.status == crate::agent::AgentStatus::Working
    }

    /// One of its questions' agents is.
    pub fn question_busy(&self) -> bool {
        self.questions.iter().any(|q| q.agent.status == crate::agent::AgentStatus::Working)
    }

    /// What a pause would cost its questions, to say before it.
    pub fn question_pause_note(&self) -> Option<&'static str> {
        use crate::agent::AgentStatus::{NeedsInput, Working};
        let running = |q: &Question| !q.asleep && (q.has_parked() || self.agent_ix(Who::Question(q.id)).is_some());
        if self.questions.iter().any(|q| running(q) && q.agent.launch.as_ref().is_some_and(|l| l.session_id().is_none())) {
            return Some("A question's conversation has no known session and could not be continued.");
        }
        if self.questions.iter().any(|q| q.agent.has_queued_message()) {
            return Some("A message waiting to be sent to a question's agent would be lost.");
        }
        if self.questions.iter().any(|q| matches!(q.agent.status, NeedsInput(_))) {
            return Some("A question's agent is waiting for your answer. The question stays and goes on when you open it.");
        }
        self.questions.iter().any(|q| q.agent.status == Working).then_some("A question's agent is in the middle of a turn. The question stays and goes on when you open it.")
    }

    /// Something of the task works that the machine should stay up for:
    /// one of its agents in a turn, its setup or its check.
    pub fn at_work(&self) -> bool {
        use crate::agent::AgentStatus::Working;
        use super::auto::RunState::Running;
        self.agent.status == Working
            || self.questions.iter().any(|q| q.agent.status == Working)
            || self.auto.setup == Some(Running)
            || self.auto.check.as_ref().is_some_and(|c| c.state == Running)
    }

    /// Stops every terminal and browser of the task before it is dropped,
    /// and what they started that left their sessions.
    pub fn shut_down(&mut self, cx: &mut Context<Self>) {
        self.end(true, cx);
    }

    /// As `shut_down` when the app quits: it looks for what every task
    /// left behind once, itself.
    pub fn shut_down_at_quit(&mut self, cx: &mut Context<Self>) {
        self.end(false, cx);
    }

    pub(super) fn end(&mut self, strays: bool, cx: &mut Context<Self>) {
        self.save_all_now(cx);
        self.auto.stop();
        for tab in self.all_tabs() {
            if let TabKind::File(file) = &tab.kind {
                file.update(cx, |f, cx| f.end_sql(cx));
            }
        }
        // Questions not shown since a restart, whose agents may run on in the daemon.
        let unshown: Vec<String> = self.unshown_sessions().collect();
        agent_tab::end_sessions(&unshown);
        for chat in self.chats().collect::<Vec<_>>() {
            chat.update(cx, |c, _| c.end());
        }
        let task = self.root.display().to_string();
        let socket = self.hook_socket.clone().filter(|_| strays);
        let terminals: Vec<Entity<TermView>> = self.all_tabs().filter_map(|tab| if let TabKind::Terminal(view) = &tab.kind { Some(view.clone()) } else { None }).collect();
        let Some(socket) = socket else {
            for view in &terminals {
                TermView::end(view, cx);
            }
            return;
        };
        // What the terminals started is told by them, so it is looked for
        // before they go; all of /proc is read for it, off the UI thread.
        // The terminals are held here: the task may be dropped meanwhile.
        cx.spawn(async move |_, cx| {
            let (look_task, look_socket) = (task.clone(), socket.clone());
            let seen = cx.background_spawn(async move { crate::procs::task_processes(Some(&look_task), &look_socket) }).await;
            let _ = cx.update(|cx| {
                for view in &terminals {
                    TermView::end(view, cx);
                }
            });
            std::thread::spawn(move || crate::procs::end_task_strays(Some(&task), &socket, seen));
        })
        .detach();
    }
}

// ---------------------------------------------------------------- power saving

impl TaskView {
    /// Definitions by search, so Ctrl+click and Ctrl+B work without a
    /// language server; `replace` drops the server's providers first.
    pub(super) fn use_search_definitions(&self, path: &PathBuf, editor: &Entity<EditorState>, replace: bool, cx: &mut Context<Self>) {
        let search = UsagesAtDeclaration::wrap(
            Rc::new(SearchDefinitionProvider { root: self.root.clone(), files: self.shared_files.clone() }),
            path,
        );
        let task_weak = cx.weak_entity();
        let this_path = path.clone();
        let predict = replace.then(|| crate::predict::provider(super::sql_schema::completions(path), path, &self.root, editor.read(cx).text().clone(), cx));
        editor.update(cx, |state, cx| {
            let lsp = state.lsp_mut();
            if replace {
                // All of them: one left would keep the ended server's client,
                // and ask it on every key.
                lsp.hover_provider = None;
                lsp.definition_provider = None;
                lsp.completion_provider = predict;
                lsp.code_action_providers.clear();
                lsp.semantic_tokens_provider = None;
                lsp.inlay_hints_provider = None;
            }
            if lsp.definition_provider.is_none() {
                lsp.definition_provider = Some(search);
                lsp.show_document = Some(show_document_handler(task_weak, this_path));
            }
            if replace {
                state.refresh_inlay_hints(cx);
            }
            cx.notify();
        });
    }

    /// Stops this task's language servers, or starts them again for the
    /// open files.
    pub fn set_language_servers(&mut self, on: bool, cx: &mut Context<Self>) {
        let status = if on || !self.lsp_asleep { "language servers off (power saving)" } else { ASLEEP };
        // Asleep, a task starts its servers when it comes into view, not now.
        if on && self.lsp_asleep {
            return;
        }
        let files: Vec<Entity<FileView>> = self
            .tabs
            .iter()
            .filter_map(|t| match &t.kind {
                TabKind::File(f) => Some(f.clone()),
                _ => None,
            })
            .collect();
        if !on {
            // Each lets go of its folder: its own server ends, a shared one goes on for the others.
            self.lsp.clear();
        }
        for file in files {
            let (path, editor) = {
                let f = file.read(cx);
                (f.path.clone(), f.editor.clone())
            };
            if on {
                let content = editor.read(cx).value().to_string();
                self.attach_lsp(&path, content, &editor, &file, cx);
            } else {
                file.update(cx, |f, cx| {
                    f.lsp_attach += 1;
                    f.lsp_client = None;
                    f.lsp_program.clear();
                    f.lsp_ok = Some(false);
                    f.lsp_missing = None;
                    f.lsp_status = status.into();
                    cx.notify();
                });
                self.use_search_definitions(&path, &editor, true, cx);
            }
        }
        cx.notify();
    }
}

pub(super) const ASLEEP: &str = "language server asleep: the task or the window was left a while ago";

impl TaskView {
    /// Stops the language servers of a task out of view, or of the one in
    /// view in a window left long ago; they cost
    /// gigabytes each, and reindexing on the way back takes a minute or two.
    pub fn put_language_servers_to_sleep(&mut self, cx: &mut Context<Self>) {
        if self.lsp_asleep {
            return;
        }
        self.lsp_asleep = true;
        self.set_language_servers(false, cx);
    }

    pub fn wake_language_servers(&mut self, cx: &mut Context<Self>) {
        if !self.lsp_asleep {
            return;
        }
        self.lsp_asleep = false;
        // Under power saving the files say so rather than "asleep".
        let on = !lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed);
        self.set_language_servers(on, cx);
    }

    /// Ends the servers no open file has used for `idle`: one runs on for a
    /// while after its last file closes, since the next file of the
    /// language would have it index the project again.
    pub fn drop_unused_language_servers(&mut self, idle: Duration, cx: &App) {
        let used: Vec<Arc<lsp::LspClient>> = self
            .tabs
            .iter()
            .filter_map(|t| match &t.kind {
                TabKind::File(f) => f.read(cx).lsp_client.clone(),
                _ => None,
            })
            .collect();
        let now = std::time::Instant::now();
        let mut over = Vec::new();
        for (key, client) in self.lsp.entries() {
            if used.iter().any(|u| Arc::ptr_eq(u, &client)) {
                self.lsp_unused.remove(&key);
            } else if now.duration_since(*self.lsp_unused.entry(key.clone()).or_insert(now)) >= idle {
                over.push(key);
            }
        }
        // What its agent asks of them through MCP is use too.
        if self.asked_of_lately(idle) {
            return;
        }
        for key in over {
            self.lsp_unused.remove(&key);
            self.lsp.remove(&key);
        }
    }

    pub fn has_language_servers(&self) -> bool {
        !self.lsp.is_empty()
    }

    /// A server of this task ended by itself (crashed, killed).
    pub fn lost_a_language_server(&self) -> bool {
        self.lsp.clients().iter().any(|client| client.is_gone())
    }

    /// Starts again the servers that ended by themselves, for the files
    /// that had them: a shared one for every task it served, each joining
    /// the new one.
    pub fn restart_lost_language_servers(&mut self, cx: &mut Context<Self>) {
        let lost: Vec<_> = self.lsp.entries().into_iter().filter(|(key, client)| client.is_gone() && crate::lsp_share::may_restart(key.0, client)).collect();
        for (key, client) in lost {
            self.lsp.remove(&key);
            self.lsp_unused.remove(&key);
            let files: Vec<Entity<FileView>> = self
                .tabs
                .iter()
                .filter_map(|t| match &t.kind {
                    TabKind::File(f) if f.read(cx).lsp_client.as_ref().is_some_and(|c| Arc::ptr_eq(c, &client)) => Some(f.clone()),
                    _ => None,
                })
                .collect();
            for file in files {
                let (path, editor) = {
                    let f = file.read(cx);
                    (f.path.clone(), f.editor.clone())
                };
                let content = editor.read(cx).value().to_string();
                self.attach_lsp(&path, content, &editor, &file, cx);
            }
        }
    }
}
