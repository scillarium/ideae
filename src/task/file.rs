use super::*;

// ---------------------------------------------------------------- file

actions!(ideae_git, [ShowBranches]);
actions!(ideae_file, [SaveFile, ExpandSelection, ShrinkSelection, ToggleComment, ShowOutline, ShowUsages, SendToAgent, CompareFileWith, CopyReference, CopyFilePath, ShowFileInFiles]);
actions!(ideae_edit, [DeleteLine, DuplicateLine, MoveLineUp, MoveLineDown, JoinLines, ToggleCase, NextOccurrence, GoToLine, RenameSymbol, FormatFile, Fold, Unfold, FoldAll, UnfoldAll, ParameterInfo, NextProblem, PrevProblem, QuickDocumentation, OptimizeImports]);
actions!(ideae_task, [ToggleZoom, SendDiffToAgent, CommentOnDiff, FindInTerminal]);

actions!(ideae_task, [NextTab, PrevTab, NavigateBack, NavigateForward, LastEditLocation, CloseTab, ReopenClosedTab]);

/// Ctrl+1/2/3/`: the agent, Changes, History, a shell.
#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = ideae_task, no_json)]
pub struct ShowView(pub usize);

/// Alt+1/2/3: the side panel's Changes, Files, Structure.
#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = ideae_task, no_json)]
pub struct ShowSidePanel(pub usize);

pub struct FileView {
    pub(super) path: PathBuf,
    pub(super) editor: Entity<EditorState>,
    /// How the file is indented: what Tab inserts and formatting is asked for.
    pub(super) indent: crate::indent::Indent,
    /// The text as last read from or written to disk, and that file's mtime.
    pub(super) saved: Print,
    pub(super) disk_mtime: Option<std::time::SystemTime>,
    pub dirty: bool,
    /// Changed on disk while it had unsaved edits: the user picks a side.
    pub(super) conflict: bool,
    /// The last save failed, with why.
    pub(super) error: Option<String>,
    pub(super) lsp_version: i32,
    /// Bumped on every change of the text, for caches of things derived from it.
    pub edit_version: u64,
    pub(super) _lsp_sync: Task<()>,
    /// The edit version being written, if a write is under way, and what
    /// orders it with a later one.
    pub(super) saving: Option<u64>,
    pub(super) save_seq: u64,
    pub(super) write_gate: Arc<WriteGate>,
    pub(super) _save: Task<()>,
    /// The task's worktree (for git) and the task, to open a blamed commit.
    pub(super) root: PathBuf,
    pub(super) task: WeakEntity<TaskView>,
    /// Who last changed each line, and the line the cursor is on.
    pub(super) blame: Rc<Vec<Option<Arc<git::BlameLine>>>>,
    pub(super) cursor_line: u32,
    /// The cursor's column, for the navigation history to come back to.
    pub(super) cursor_col: u32,
    pub(super) _blame: Task<()>,
    /// Blame shown by every line, in the gutter; made from `blame` once.
    pub(super) annotate: bool,
    pub(super) annotations: Option<Rc<gpui_kit::base::input::GutterNotes>>,
    /// The line whose annotation the pointer is on, and where it is.
    pub(super) blame_hover: Option<(usize, Point<Pixels>)>,
    /// Selections Ctrl+W grew from, for Ctrl+Shift+W to step back through.
    pub(super) expand_stack: Vec<std::ops::Range<usize>>,
    pub(super) last_expanded: Option<std::ops::Range<usize>>,
    /// What the language server is doing for this file, shown in the header.
    pub(super) lsp_status: SharedString,
    /// None while starting, then whether it works.
    pub(super) lsp_ok: Option<bool>,
    pub(super) lsp_client: Option<Arc<lsp::LspClient>>,
    pub(super) lsp_program: String,
    /// The server this file would have if it were installed.
    pub(super) lsp_missing: Option<&'static str>,
    /// Counts the times it asked for its server: an answer to an older one
    /// is not applied.
    pub(super) lsp_attach: u64,
    /// Its server runs without the licence that would give it more.
    pub(super) lsp_unlicensed: bool,
    /// How a file that can be drawn (Markdown, SVG, a table) is shown, and the drawing.
    pub(super) preview: super::preview::Preview,
    /// A `.sql` file's connection and results. Not in `preview`, whose task
    /// each edit replaces: typing must not stop a query.
    pub(super) sql: Option<super::sql::SqlFile>,
    /// Git ignores it: while presenting it is not shown until asked for.
    pub(super) veiled: bool,
    /// The text whose next occurrence was added to the selection last,
    /// and where that one ends.
    pub(super) occurrence: Option<(String, usize)>,
    /// The file as the last commit has it, for the gutter's marks of what
    /// changed since; None for a file that commit lacks.
    pub(super) committed: Option<Arc<str>>,
    pub(super) _marks: Task<()>,
    /// The changes the gutter marks, and the one opened from its mark.
    pub(super) change_runs: Rc<Vec<crate::edit::ChangeRun>>,
    pub(super) change_popup: Option<super::change_popup::ChangePopup>,
    /// The name F1 showed the documentation of: the popover goes when the cursor leaves it.
    pub(super) doc_shown: Option<std::ops::Range<usize>>,
    /// The count of the language server's reports when its problems were
    /// last put into the editor.
    pub(super) problems_seen: u64,
    /// The signature shown above the cursor: its text, the parameter being
    /// written, the line it was asked on.
    pub(super) signature: Option<(String, Option<std::ops::Range<usize>>, u32)>,
    pub(super) _signature: Task<()>,
    /// The declarations read for the sticky lines, after typing pauses.
    pub(super) _scopes: Task<()>,
    pub(super) _poll: Task<()>,
}

/// How many 150 ms waits a task out of view lets pass before it refreshes.
pub(super) const HIDDEN_REFRESH_WAITS: usize = 20;

pub(super) fn mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    if crate::hosts::is_remote(path) {
        let stat = crate::hostfs::stat(path).ok()?;
        return Some(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(u64::try_from(stat.modified).ok()?));
    }
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A text as it was saved, without the text: its length and a hash. The
/// editor has the text already; a second copy of every open file is not
/// kept to tell whether it was edited.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Print {
    len: usize,
    hash: u64,
}

impl Print {
    fn add(hash: u64, bytes: &[u8]) -> u64 {
        // FNV-1a: the same whatever pieces the text comes in.
        bytes.iter().fold(hash, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
    }

    pub(super) fn of(text: &str) -> Self {
        Self { len: text.len(), hash: Self::add(0xcbf2_9ce4_8422_2325, text.as_bytes()) }
    }

    /// Whether the editor holds that text. Read through only when it is as
    /// long: while typing it seldom is.
    pub(super) fn is(&self, rope: &Rope) -> bool {
        rope.len() == self.len && rope.chunks().fold(0xcbf2_9ce4_8422_2325, |hash, chunk| Self::add(hash, chunk.as_bytes())) == self.hash
    }
}

/// Whether the editor holds exactly `text`, without copying it out.
pub(super) fn same_text(rope: &Rope, text: &str) -> bool {
    let mut rest = text.as_bytes();
    rope.len() == rest.len()
        && rope.chunks().all(|chunk| {
            let (head, tail) = rest.split_at(chunk.len());
            rest = tail;
            head == chunk.as_bytes()
        })
}

/// The file was written by another since it was last read: not written over.
#[derive(Debug, PartialEq)]
pub(super) struct ChangedOnDisk;

/// The last write made of a file, and the file's time after it.
pub(super) type WriteGate = std::sync::Mutex<(u64, Option<std::time::SystemTime>)>;

/// Writes of one file, in the order they were asked for: a write that
/// starts after a later one has been made is dropped. Gives the file's
/// time once written. A file that is not as `known`, nor as the last write
/// here left it, was changed by another (the agent, git) and no refresh
/// has seen it yet: it is left as it is.
pub(super) fn write_in_order(
    gate: &WriteGate,
    seq: u64,
    path: &std::path::Path,
    known: Option<std::time::SystemTime>,
    bytes: &[u8],
) -> std::io::Result<Result<Option<std::time::SystemTime>, ChangedOnDisk>> {
    let mut latest = gate.lock().unwrap();
    if seq > latest.0 {
        let now = mtime(path);
        if now.is_some() && now != known && now != latest.1 {
            return Ok(Err(ChangedOnDisk));
        }
        crate::hostfs::write(path, bytes)?;
        *latest = (seq, mtime(path));
    }
    Ok(Ok(mtime(path)))
}

/// The task's file watcher ([`ideae_core::watch`]): here, or for a remote
/// task its daemon's, there.
pub(super) enum Watching {
    Here(ideae_core::watch::Watch),
    /// The task's key, the id its daemon tells of its changes by (its own,
    /// not another view's of the task), and what it was last asked to watch.
    #[cfg(unix)]
    There { root: PathBuf, id: String, sent: Option<(Arc<BTreeSet<PathBuf>>, Vec<PathBuf>)> },
}

impl Watching {
    /// A remote task's: told of its changes by its daemon, as `on_change`.
    #[cfg(unix)]
    pub(super) fn there(root: &std::path::Path, on_change: async_channel::Sender<()>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = format!("{}#watch-{}", root.display(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        if let Some(daemon) = crate::daemon::client::for_path(root) {
            daemon.on_change(&id, on_change);
        }
        Watching::There { root: root.to_path_buf(), id, sent: None }
    }
}

impl Watching {
    pub(super) fn sync(&mut self, watch: (Arc<BTreeSet<PathBuf>>, Vec<PathBuf>)) {
        match self {
            Watching::Here(here) => here.sync(watch.0, watch.1),
            #[cfg(unix)]
            Watching::There { root, id, sent } => {
                // The same folders as last time: nothing to tell.
                if sent.as_ref().is_some_and(|(flat, deep)| (Arc::ptr_eq(flat, &watch.0) || *flat == watch.0) && *deep == watch.1) {
                    return;
                }
                if let Some(daemon) = crate::daemon::client::for_path(root) {
                    let there = |paths: &mut dyn Iterator<Item = &PathBuf>| paths.map(|p| crate::hosts::there(p)).collect::<Vec<_>>();
                    daemon.watch(id, there(&mut watch.0.iter()), there(&mut watch.1.iter()));
                }
                *sent = Some(watch);
            }
        }
    }
}

#[cfg(unix)]
impl Drop for Watching {
    fn drop(&mut self) {
        if let Watching::There { root, id, .. } = self {
            if let Some(daemon) = crate::daemon::client::for_path(root) {
                daemon.unwatch(id);
            }
        }
    }
}

/// A file found changed on disk: the time the view knew, the time now,
/// and what is in it.
pub(super) struct DiskChange {
    pub(super) known: Option<std::time::SystemTime>,
    pub(super) now: std::time::SystemTime,
    pub(super) content: String,
}

/// Off the UI thread: of the open files, those whose time on disk is not
/// the one known, read.
pub(super) fn changed_on_disk(known: Vec<(PathBuf, Option<std::time::SystemTime>)>) -> Vec<Option<DiskChange>> {
    known
        .into_iter()
        .map(|(path, known)| {
            let now = mtime(&path).filter(|now| Some(*now) != known)?;
            Some(DiskChange { known, now, content: crate::hostfs::read_to_string(&path).ok()? })
        })
        .collect()
}

impl FileView {
    /// The gutter's marks of what differs from the last commit: green by
    /// the lines added, blue by those changed, red where some are gone.
    /// After `typing`, once it pauses.
    pub(super) fn mark_changes(&mut self, typing: bool, cx: &mut Context<Self>) {
        use gpui_kit::base::input::LineMarks;
        let Some(committed) = self.committed.clone() else {
            // Nothing to compare with: git ignores it, or there is no repository.
            self.editor.update(cx, |state, cx| state.set_line_marks(LineMarks::default(), cx));
            self.change_runs = Rc::default();
            return;
        };
        self._marks = cx.spawn(async move |this, cx| {
            if typing {
                cx.background_executor().timer(Duration::from_millis(250)).await;
            }
            // Taken once typing pauses, not on every key: it is the whole file.
            let Ok(text) = this.update(cx, |this, cx| this.editor.read(cx).value()) else { return };
            let runs = cx.background_spawn(async move { crate::edit::change_runs(&committed, &text) }).await;
            let _ = this.update(cx, |this, cx| {
                let changes = crate::edit::LineChanges::of(&runs);
                this.change_runs = Rc::new(runs);
                let (added, changed, gone) = (hsla(0.33, 0.6, 0.45, 1.), hsla(0.58, 0.7, 0.55, 1.), hsla(0.0, 0.7, 0.55, 1.));
                let mut marks = LineMarks::default();
                marks.rows.extend(changes.added.into_iter().map(|row| (row, added)));
                marks.rows.extend(changes.changed.into_iter().map(|row| (row, changed)));
                marks.gone_before.extend(changes.gone_before.into_iter().map(|row| (row, gone)));
                this.editor.update(cx, |state, cx| state.set_line_marks(marks, cx));
            });
        });
    }

    pub(super) fn on_edit(&mut self, cx: &mut Context<Self>) {
        self.edit_version += 1;
        self.change_popup = None;
        self.want_schema(cx);
        self.mark_changes(true, cx);
        self.refresh_preview(true, cx);
        let typed = {
            let state = self.editor.read(cx);
            let (text, cursor) = (state.text(), state.cursor());
            // The character before the cursor starts one to four bytes back.
            (1..=4).find_map(|back| text.char_at(cursor.checked_sub(back)?))
        };
        if self.signature.is_some() || matches!(typed, Some('(' | ',')) {
            self.parameter_info(cx);
        }
        // Told on every change, not only when it turns dirty: the Structure
        // panel follows the text.
        let was_dirty = std::mem::replace(&mut self.dirty, !self.saved.is(self.editor.read(cx).text()));
        if was_dirty != self.dirty {
            if self.annotate {
                self.show_annotations(cx);
            }
            self.show_line_blame(cx);
        }
        cx.notify();
        self.read_scopes(Duration::from_millis(500), cx);
        // Tell the language server after typing pauses, so hover and
        // definitions see the edited text. The text is taken then, not on
        // every key: it is a copy of the whole file.
        let Some(client) = self.lsp_client.clone() else { return };
        self.lsp_version += 1;
        let path = self.path.clone();
        self._lsp_sync = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let Ok((version, text)) = this.update(cx, |f, cx| (f.lsp_version, f.editor.read(cx).value())) else { return };
            cx.background_spawn(crate::procs::on_thread(move || {
                let _ = client.did_change(&path, version, &text);
            }))
            .await;
        });
    }

    /// Saves edits unless the file changed on disk meanwhile: then the
    /// conflict bar asks, rather than silently overwriting the other side.
    pub fn autosave(&mut self, cx: &mut Context<Self>) {
        if self.dirty && !self.conflict && self.saving != Some(self.edit_version) {
            self.save(cx);
        }
    }

    /// Writes off the UI thread; the file counts as saved once it is written.
    pub(super) fn save(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value();
        self.save_seq += 1;
        self.saving = Some(self.edit_version);
        let (seq, path, gate, known) = (self.save_seq, self.path.clone(), self.write_gate.clone(), self.disk_mtime);
        self._save = cx.spawn(async move |this, cx| {
            let bytes = text.clone();
            let written = cx.background_spawn(async move { write_in_order(&gate, seq, &path, known, bytes.as_bytes()) }).await;
            let _ = this.update(cx, |this, cx| {
                if seq == this.save_seq {
                    this.saving = None;
                    this.written(text, written, cx);
                }
            });
        });
    }

    /// The task closes or the app quits: what is unsaved, or still being
    /// written, is on disk when this returns.
    pub fn save_now(&mut self, cx: &mut Context<Self>) {
        if self.saving.is_none() && !(self.dirty && !self.conflict) {
            return;
        }
        let text = self.editor.read(cx).value();
        self.save_seq += 1;
        self.saving = None;
        self._save = Task::ready(());
        let written = write_in_order(&self.write_gate, self.save_seq, &self.path, self.disk_mtime, text.as_bytes());
        self.written(text, written, cx);
    }

    pub(super) fn written(&mut self, text: SharedString, written: std::io::Result<Result<Option<std::time::SystemTime>, ChangedOnDisk>>, cx: &mut Context<Self>) {
        match written {
            // The bar asks which to keep; the next refresh reads what is there.
            Ok(Err(ChangedOnDisk)) => self.conflict = true,
            Ok(Ok(mtime)) => {
                self.dirty = !same_text(self.editor.read(cx).text(), &text);
                self.saved = Print::of(&text);
                self.disk_mtime = mtime;
                self.conflict = false;
                self.error = None;
                self.load_blame(cx);
                if let Some(client) = self.lsp_client.clone() {
                    let path = self.path.clone();
                    cx.background_spawn(crate::procs::on_thread(move || {
                        let _ = client.did_save(&path);
                    }))
                    .detach();
                }
            }
            Err(e) => self.error = Some(format!("Save failed: {e}")),
        }
        cx.notify();
    }

    /// The file's time as this view knows it, for the refresh to compare
    /// with the disk off the UI thread. None while a save is being written.
    pub(super) fn disk_known(&self) -> Option<(PathBuf, Option<std::time::SystemTime>)> {
        self.saving.is_none().then(|| (self.path.clone(), self.disk_mtime))
    }

    /// Called on every worktree refresh with what `changed_on_disk` read:
    /// picks up changes made on disk (by the agent, git, another editor).
    /// Clean buffers reload in place; edited ones get a choice instead of
    /// losing either side.
    pub(super) fn apply_disk(&mut self, change: DiskChange, window: &mut Window, cx: &mut Context<Self>) {
        // Saved or reloaded since the disk was read: the next refresh looks again.
        if self.saving.is_some() || self.disk_mtime != change.known {
            return;
        }
        self.disk_mtime = Some(change.now);
        if Print::of(&change.content) == self.saved {
            // Touched and no more: nothing to choose between.
            if std::mem::take(&mut self.conflict) {
                cx.notify();
            }
            return;
        }
        if self.dirty {
            self.conflict = true;
            cx.notify();
        } else {
            self.reload(change.content, window, cx);
        }
    }

    pub(super) fn reload(&mut self, content: String, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_version += 1;
        self.saved = Print::of(&content);
        self.conflict = false;
        let content_for_lsp = content.clone();
        self.editor.update(cx, |state, cx| {
            // What you were looking at stays where it went in the new text.
            let old = state.text().to_string();
            let shift = crate::edit::LineShift::of(&old, &content);
            let moved = |at: Position| Position::new(shift.row(at.line as usize) as u32, at.character);
            let cursor = moved(state.cursor_position());
            let selected = state.selected_range();
            let selection = (!selected.is_empty()).then(|| (moved(state.text().offset_to_position(selected.start)), moved(state.text().offset_to_position(selected.end))));
            let folded = !state.fold_starts().is_empty();
            let mut scroll = state.scroll_offset();
            if let Some(height) = state.line_height().filter(|h| *h > px(0.) && !folded) {
                let top = ((-scroll.y) / height).max(0.).floor() as usize;
                scroll.y -= height * (shift.row(top) as f32 - top as f32);
            }
            state.forget_folds_to_restore();
            // It was about a place of the text that went.
            state.clear_hover_state(cx);
            let (from, to) = crate::edit::changed_part(&old, &content);
            state.replace_from_outside(from, content.get(to).unwrap_or_default(), window, cx);
            // Not `set_cursor_position`, which takes the keyboard: the file may
            // be reloading in a task out of view, as its agent writes it.
            state.place_cursor(cursor, cx);
            if let Some((start, end)) = selection {
                let range = state.text().position_to_offset(&start)..state.text().position_to_offset(&end);
                state.set_selected_range(range, cx);
            }
            state.set_scroll_offset(scroll, cx);
        });
        self.dirty = false;
        self.load_blame(cx);
        self.read_scopes(Duration::ZERO, cx);
        // A change from outside tells no Change, and an agent writes in bursts.
        self.refresh_preview(true, cx);
        if let Some(client) = self.lsp_client.clone() {
            self.lsp_version += 1;
            let (path, version) = (self.path.clone(), self.lsp_version);
            cx.background_spawn(crate::procs::on_thread(move || {
                let _ = client.did_change(&path, version, &content_for_lsp);
            }))
            .detach();
        }
        cx.notify();
    }

    pub(super) fn reload_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A remote task's file comes from its host off the UI thread.
        if crate::hosts::is_remote(&self.path) {
            let path = self.path.clone();
            cx.spawn_in(window, async move |this, cx| {
                let read = cx.background_spawn(async move { (crate::hostfs::read_to_string(&path), mtime(&path)) }).await;
                if let (Ok(content), mtime) = read {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.disk_mtime = mtime;
                        this.reload(content, window, cx);
                    });
                }
            })
            .detach();
            return;
        }
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            self.disk_mtime = mtime(&self.path);
            self.reload(content, window, cx);
        }
    }

    pub fn load_blame(&mut self, cx: &mut Context<Self>) {
        let (root, path) = (self.root.clone(), self.path.clone());
        self._blame = cx.spawn(async move |this, cx| {
            let since_base = crate::state::settings().gutter_since_base;
            let (blame, committed) =
                cx.background_spawn(async move { (git::blame(&root, &path), git::committed_text(&root, &path, since_base)) }).await;
            let _ = this.update(cx, |this, cx| {
                this.blame = Rc::new(blame);
                this.annotations = None;
                this.show_annotations(cx);
                this.show_line_blame(cx);
                this.committed = committed.map(Arc::from);
                this.mark_changes(false, cx);
                cx.notify();
            });
        });
    }

    pub(super) fn render_blame(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.dirty || crate::state::with_settings(|s| s.blame_at_line_end) {
            return None;
        }
        let info = self.blame.get(self.cursor_line as usize)?.clone()?;
        let text = if info.uncommitted() {
            "not committed yet".to_string()
        } else {
            format!("{}, {} · {} · {}", info.author, git::ago(info.time), info.summary, info.hash.get(..8).unwrap_or(&info.hash))
        };
        let task = self.task.clone();
        let hash = info.hash.clone();
        let uncommitted = info.uncommitted();
        Some(
            div()
                .id("blame")
                .min_w_0()
                .flex_1()
                .truncate()
                .text_color(cx.theme().muted_foreground.opacity(0.8))
                .when(!uncommitted, |this| {
                    this.cursor_pointer()
                        .hover(|s| s.text_color(cx.theme().foreground))
                        .on_click(move |_, window, cx| {
                            let _ = task.update(cx, |t, cx| t.show_commit(&hash, window, cx));
                        })
                })
                .child(text)
                .into_any_element(),
        )
    }

    /// Ctrl+W: select the smallest syntax node larger than the selection (a
    /// word, an expression, a statement, a block…); without a grammar, the
    /// line, then the whole text.
    pub(super) fn expand_selection(&mut self, cx: &mut Context<Self>) {
        let (text, range) = {
            let state = self.editor.read(cx);
            (state.text().to_string(), state.selected_range())
        };
        // A selection moved by hand since the last step starts a new chain.
        if self.expand_stack.last().is_some_and(|_| self.last_expanded.as_ref() != Some(&range)) {
            self.expand_stack.clear();
        }
        let next = syntax_parent(&self.path, &text, range.clone()).unwrap_or_else(|| plain_parent(&text, range.clone()));
        if next == range {
            return;
        }
        self.expand_stack.push(range);
        self.last_expanded = Some(next.clone());
        self.editor.update(cx, |state, cx| state.set_selected_range(next, cx));
    }

    /// Ctrl+Shift+W: back to the selection before the last Ctrl+W.
    pub(super) fn shrink_selection(&mut self, cx: &mut Context<Self>) {
        let Some(previous) = self.expand_stack.pop() else { return };
        self.last_expanded = Some(previous.clone());
        self.editor.update(cx, |state, cx| state.set_selected_range(previous, cx));
    }

    /// Ctrl+/: comment the selected lines out, or back in when all are.
    pub(super) fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, range) = {
            let state = self.editor.read(cx);
            (state.text().to_string(), state.selected_range())
        };
        let Some((start, end, replacement)) = toggle_comment_lines(&self.path, &text, range) else { return };
        self.editor.update(cx, |state, cx| {
            state.set_selected_range(start..end, cx);
            state.replace(replacement.clone(), window, cx);
            state.set_selected_range(start..start + replacement.len(), cx);
        });
    }

    /// One edit of the text, undone as one: what `change` makes of the
    /// text and the selection.
    pub(super) fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>, change: impl FnOnce(&str, std::ops::Range<usize>) -> Option<crate::edit::Edit>) {
        let (text, selection) = {
            let state = self.editor.read(cx);
            (state.text().to_string(), state.selected_range())
        };
        let Some(edit) = change(&text, selection) else { return };
        self.editor.update(cx, |state, cx| {
            state.set_selected_range(edit.range, cx);
            state.replace(edit.text, window, cx);
            state.set_selected_range(edit.select, cx);
        });
    }

    /// The word under the cursor, then with every press one more place
    /// where the selected text is: all of them are typed over at once.
    pub(super) fn select_next_occurrence(&mut self, cx: &mut Context<Self>) {
        let (text, selection) = {
            let state = self.editor.read(cx);
            (state.text().to_string(), state.selected_range())
        };
        if selection.is_empty() {
            self.occurrence = None;
            if let Some(word) = crate::edit::word_at(&text, selection.start) {
                self.editor.update(cx, |state, cx| state.set_selected_range(word, cx));
            }
            return;
        }
        let needle = text[selection.clone()].to_string();
        // After the one added last, while the same text is being looked for.
        let from = self.occurrence.as_ref().filter(|(of, _)| *of == needle).map_or(selection.end, |(_, end)| *end);
        if let Some(found) = crate::edit::next_occurrence(&text, &needle, from, selection.start) {
            self.occurrence = Some((needle, found.end));
            self.editor.update(cx, |state, cx| state.add_selection(found, cx));
        }
    }

    /// Ctrl+P, and by itself after `(` and `,`: the signature of the call
    /// being written, from the language server.
    pub(super) fn parameter_info(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.lsp_client.clone() else { return };
        let path = self.path.clone();
        self._signature = cx.spawn(async move |this, cx| {
            // After the keys of a burst, not each of them; and the text as it is then.
            cx.background_executor().timer(std::time::Duration::from_millis(80)).await;
            let Ok((text, pos)) = this.update(cx, |file, cx| {
                let state = file.editor.read(cx);
                (state.text().clone(), state.cursor_position())
            }) else {
                return;
            };
            let uri = lsp::path_to_uri(&path);
            let help = cx
                .background_spawn(crate::procs::on_thread(move || {
                    // Made one string where there is time for it.
                    client.did_change(&path, 0, &text.to_string())?;
                    client.signature_help(&uri, pos)
                }))
                .await;
            let shown = help.ok().flatten().and_then(|help| lsp::signature_shown(&help)).map(|(label, active)| (label, active, pos.line));
            let _ = this.update(cx, |file, cx| {
                file.signature = shown;
                cx.notify();
            });
        });
    }

    /// The signature above the cursor, while the cursor is on the line it
    /// was asked for.
    pub(super) fn render_signature(&self, cx: &App) -> Option<AnyElement> {
        let (label, active, line) = self.signature.as_ref()?;
        let state = self.editor.read(cx);
        if state.cursor_position().line != *line {
            return None;
        }
        let cursor = state.cursor();
        // The character before the cursor starts one to four bytes back.
        let text = state.text();
        let before = (1..=4).find_map(|back| cursor.checked_sub(back).filter(|at| text.char_at(*at).is_some())).unwrap_or(cursor);
        let at = state.range_to_bounds(&(before..cursor)).or_else(|| state.range_to_bounds(&(cursor..cursor)))?;
        let strong = HighlightStyle { font_weight: Some(FontWeight::BOLD), color: Some(cx.theme().primary), ..Default::default() };
        let highlights: Vec<_> = active.clone().map(|range| (range, strong)).into_iter().collect();
        Some(
            deferred(
                anchored().position(at.origin).anchor(gpui_kit::Anchor::BottomLeft).snap_to_window().child(
                    div()
                        .mb_1()
                        .px_2()
                        .py_1()
                        .max_w(px(720.))
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().popover)
                        .shadow_md()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_sm()
                        .child(StyledText::new(label.clone()).with_highlights(highlights)),
                ),
            )
            .into_any_element(),
        )
    }

    /// What is folded, to fold again after a restart.
    pub(super) fn saved_folds(&self, cx: &App) -> Vec<crate::state::SavedFold> {
        let state = self.editor.read(cx);
        let text = state.text();
        state.fold_starts().into_iter().map(|line| crate::state::SavedFold { line: line as u32, text: fold_words(text, line) }).collect()
    }

    pub(super) fn restore_folds(&mut self, saved: &[crate::state::SavedFold], cx: &mut Context<Self>) {
        self.editor.update(cx, |state, _| {
            let starts = saved.iter().filter_map(|fold| place_fold(state.text(), fold)).collect();
            state.restore_folds(starts);
        });
    }

    pub(super) fn fold(&mut self, fold: bool, all: bool, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.fold(fold, all, cx);
        });
    }

    /// The language server's latest word on what is wrong in the file, into
    /// the editor: underlined there, with the fixes it offers.
    pub(super) fn show_problems(&mut self, cx: &mut Context<Self>) {
        let Some(client) = &self.lsp_client else { return };
        let Some((report, found)) = client.problems_since(&lsp::path_to_uri(&self.path), self.problems_seen) else { return };
        self.problems_seen = report;
        self.editor.update(cx, |state, cx| {
            let text = state.text().clone();
            if let Some(shown) = state.diagnostics_mut() {
                shown.reset(&text);
                shown.extend(found);
            }
            cx.notify();
        });
    }

    /// Every edit of a workspace edit: those of this file in the editor,
    /// undone there as one; those of others on disk, where their tabs pick
    /// them up, written off the UI thread: a name can be in hundreds of
    /// them. `then` is told how many files changed.
    pub(super) fn apply_workspace_edit(
        &mut self,
        edit: &lsp_types::WorkspaceEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(usize, &mut Window, &mut Context<Self>) + 'static,
    ) {
        // Another file of it open with edits not saved: the server placed its
        // changes by what is on disk, and written there they would be at odds
        // with the tab. Nothing is applied, rather than a part of it.
        let by_file = lsp::edits_by_file(edit);
        let this = cx.entity_id();
        let unsaved = self.task.read_with(cx, |task, cx| task.unsaved_files(this, cx)).unwrap_or_default();
        if let Some(open) = by_file.iter().map(|(path, _)| path).find(|path| unsaved.contains(path)) {
            let name = open.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            window.push_notification(Notification::info(format!("Not applied: {name} has edits that are not saved. Save it and ask again.")).title("Language server"), cx);
            return;
        }
        let mut here = 0;
        let mut others = Vec::new();
        for (path, edits) in by_file {
            if path == self.path {
                self.edit(window, cx, |text, selection| {
                    let rewritten = crate::edit::rewrite(text, &lsp::apply_edits(text, &edits), selection.start);
                    here += usize::from(rewritten.is_some());
                    rewritten
                });
            } else {
                others.push((path, edits));
            }
        }
        if others.is_empty() {
            return then(here, window, cx);
        }
        cx.spawn_in(window, async move |this, cx| {
            let written = cx
                .background_spawn(async move {
                    others
                        .into_iter()
                        .filter(|(path, edits)| {
                            let Ok(text) = std::fs::read_to_string(path) else { return false };
                            let new = lsp::apply_edits(&text, edits);
                            new != text && std::fs::write(path, new).is_ok()
                        })
                        .count()
                })
                .await;
            let _ = this.update_in(cx, |_, window, cx| then(here + written, window, cx));
        })
        .detach();
    }

    /// The server is told the text as it is now, then asked: the answer
    /// comes back to `then`, in the window.
    pub(super) fn ask_server<T: Send + 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        ask: impl FnOnce(&lsp::LspClient, &str, lsp_types::Position) -> anyhow::Result<T> + Send + 'static,
        then: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.ask_server_or(window, cx, ask, then, None)
    }

    /// As `ask_server`; with `without`, that is done when there is no
    /// answer to apply, and the stale answer is not told of.
    fn ask_server_or<T: Send + 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        ask: impl FnOnce(&lsp::LspClient, &str, lsp_types::Position) -> anyhow::Result<T> + Send + 'static,
        then: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        without: Option<fn(&mut Self, &mut Context<Self>)>,
    ) {
        let Some(client) = self.lsp_client.clone() else {
            match without {
                Some(without) => without(self, cx),
                None => window.push_notification(Notification::info("No language server runs for this file."), cx),
            }
            return;
        };
        let (path, text, pos) = {
            let state = self.editor.read(cx);
            (self.path.clone(), state.text().clone(), state.cursor_position())
        };
        let asked_of = self.edit_version;
        cx.spawn_in(window, async move |this, cx| {
            let uri = lsp::path_to_uri(&path);
            let answer = cx
                .background_spawn(crate::procs::on_thread(move || {
                    client.did_change(&path, 0, &text.to_string())?;
                    ask(&client, &uri, pos)
                }))
                .await;
            let _ = this.update_in(cx, |this, window, cx| match answer {
                // The answer is about the text as it was: put onto the one
                // typed since, its changes would land in the wrong places.
                Ok(_) if this.edit_version != asked_of => match without {
                    Some(without) => without(this, cx),
                    None => window.push_notification(Notification::info("The text changed while the language server was asked: nothing was applied. Ask again.").title("Language server"), cx),
                },
                Ok(answer) => then(this, answer, window, cx),
                Err(e) => {
                    window.push_notification(Notification::error(format!("{e:#}")).title("Language server"), cx);
                    if let Some(without) = without {
                        without(this, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Formats the file as its language server would.
    pub(super) fn format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.format_then(false, window, cx)
    }

    /// Reformats, and with `save` writes the file once that is done.
    fn format_then(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Asked to save, the file is saved whatever comes of the formatting: as it is, when the server fails or the text moved on.
        let without: Option<fn(&mut Self, &mut Context<Self>)> = if save { Some(|this, cx| this.save(cx)) } else { None };
        let then = move |this: &mut Self, edits: Vec<lsp_types::TextEdit>, window: &mut Window, cx: &mut Context<Self>| {
            this.edit(window, cx, |text, selection| crate::edit::rewrite(text, &lsp::apply_edits(text, &edits), selection.start));
            if save {
                this.save(cx);
            }
        };
        let indent = self.indent;
        self.ask_server_or(window, cx, move |client, uri, _| client.formatting(uri, indent.size as u32, !indent.tabs), then, without);
    }

    /// F1: what the language server says of the name at the cursor, in the
    /// popover a resting pointer brings; gone once the cursor leaves the name.
    pub(super) fn quick_documentation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_server(
            window,
            cx,
            |client, uri, pos| client.hover(uri, pos),
            |this, hover, window, cx| {
                let Some(hover) = hover else {
                    return window.push_notification(Notification::info("The language server says nothing of this."), cx);
                };
                let range = {
                    let state = this.editor.read(cx);
                    let (text, cursor) = (state.text(), state.cursor());
                    match &hover.range {
                        Some(range) => text.position_to_offset(&range.start)..text.position_to_offset(&range.end),
                        None => text.word_range(cursor).unwrap_or(cursor..cursor),
                    }
                };
                this.doc_shown = Some(range.clone());
                this.editor.update(cx, |state, cx| state.present_hover(range, hover, cx));
            },
        );
    }

    /// Ctrl+Alt+O: the language server's `source.organizeImports`, over the whole file.
    pub(super) fn optimize_imports(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let end = {
            let text = self.editor.read(cx).text();
            text.offset_to_position(text.len())
        };
        self.ask_server(
            window,
            cx,
            move |client, uri, _| client.source_action(uri, "source.organizeImports", lsp_types::Range::new(lsp_types::Position::new(0, 0), end)),
            |this, edits, window, cx| match edits {
                None => window.push_notification(Notification::info("The language server does not optimize imports here."), cx),
                Some(edits) if edits.iter().flat_map(lsp::edits_by_file).all(|(_, edits)| edits.is_empty()) => {
                    window.push_notification(Notification::info("The imports are in order."), cx)
                }
                Some(edits) => {
                    for edit in &edits {
                        this.apply_workspace_edit(edit, window, cx, |_, _, _| {});
                    }
                }
            },
        );
    }

    /// Asks for a new name of the symbol under the cursor and gives it that
    /// name everywhere the language server knows it.
    pub(super) fn rename_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let old = {
            let state = self.editor.read(cx);
            let text = state.text().to_string();
            crate::edit::word_at(&text, state.selected_range().start).map(|word| text[word].to_string()).unwrap_or_default()
        };
        if old.is_empty() {
            return;
        }
        let input = cx.new(|cx| gpui_kit::component::input::InputState::new(window, cx));
        input.update(cx, |input, cx| {
            input.set_value(old.clone(), window, cx);
            input.select_all(window, cx);
        });
        cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| {
            let InputEvent::PressEnter { .. } = event else { return };
            let name = input.read(cx).value().trim().to_string();
            window.close_dialog(cx);
            if name.is_empty() || name == old {
                return;
            }
            this.ask_server(
                window,
                cx,
                move |client, uri, pos| client.rename(uri, pos, &name),
                |this, edit, window, cx| {
                    let told = |files: usize, window: &mut Window, cx: &mut Context<Self>| {
                        let told = match files {
                            0 => "Nothing to rename here.".to_string(),
                            1 => "Renamed in 1 file.".to_string(),
                            n => format!("Renamed in {n} files."),
                        };
                        window.push_notification(Notification::info(told), cx);
                    };
                    match edit {
                        Some(edit) => this.apply_workspace_edit(&edit, window, cx, told),
                        None => told(0, window, cx),
                    }
                },
            );
        })
        .detach();
        let focused = Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, focused) = (input.clone(), focused.clone());
            dialog.title("Rename").w(px(420.)).content(move |content, window, cx| {
                if !focused.replace(true) {
                    let input = input.clone();
                    window.defer(cx, move |window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
                }
                content.child(gpui_kit::component::input::Input::new(&input))
            })
        });
    }

    /// Asks for a line, `120` or `120:8`, and puts the cursor there.
    pub(super) fn go_to_line_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let lines = self.editor.read(cx).text().to_string().lines().count().max(1);
        let input = cx.new(|cx| gpui_kit::component::input::InputState::new(window, cx).placeholder(format!("Line, or line:column, of {lines}")));
        cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
            let InputEvent::PressEnter { .. } = event else { return };
            let typed = input.read(cx).value().trim().to_string();
            let (line, column) = typed.split_once(':').unwrap_or((typed.as_str(), "1"));
            if let Ok(line) = line.trim().parse::<u32>() {
                window.close_dialog(cx);
                this.go_to_line(line.saturating_sub(1), column.trim().parse::<u32>().unwrap_or(1).saturating_sub(1), window, cx);
            }
        })
        .detach();
        let focused = Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, focused) = (input.clone(), focused.clone());
            dialog.title("Go to line").w(px(360.)).content(move |content, window, cx| {
                if !focused.replace(true) {
                    let input = input.clone();
                    window.defer(cx, move |window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
                }
                content.child(gpui_kit::component::input::Input::new(&input))
            })
        });
    }

    pub(super) fn go_to_line(&mut self, line: u32, col: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.jump(line, col, window, cx);
        self.editor.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// The declarations of the text, read off the UI thread once typing
    /// has paused for `after`, for the editor's sticky lines.
    pub(super) fn read_scopes(&mut self, after: Duration, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self._scopes = cx.spawn(async move |this, cx| {
            if !after.is_zero() {
                cx.background_executor().timer(after).await;
            }
            let Ok(text) = this.update(cx, |f, cx| f.editor.read(cx).text().clone()) else { return };
            let scopes = cx.background_spawn(async move { sticky_scopes(&path, &text.to_string()) }).await;
            let _ = this.update(cx, |f, cx| f.editor.update(cx, |state, cx| state.set_sticky_scopes(Rc::new(scopes), cx)));
        });
    }

    /// F2 / Shift+F2: the next or previous problem the language server
    /// reported, errors before warnings, round the end of the file; what it
    /// says is shown there.
    pub(super) fn go_to_problem(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.editor.read(cx);
        let Some(set) = state.diagnostics() else { return };
        let at = state.cursor_position();
        let at = (at.line, at.character);
        let worst = set.iter().map(|d| problem_rank(d.severity)).max();
        let Some(worst) = worst.filter(|&r| r > 0) else { return };
        let starts = |d: &&gpui_kit::component::input::DiagnosticEntry| (d.diagnostic.range.start.line, d.diagnostic.range.start.character);
        let of_rank = || set.iter().filter(|d| problem_rank(d.severity) == worst);
        let picked = if forward {
            of_rank().find(|d| starts(d) > at).or_else(|| of_rank().next())
        } else {
            of_rank().filter(|d| starts(d) < at).last().or_else(|| of_rank().last())
        };
        let Some(entry) = picked.cloned() else { return };
        let start = entry.diagnostic.range.start;
        self.editor.update(cx, |state, cx| {
            state.jump_to_position(start, JUMP_AT, window, cx);
            state.present_diagnostic(entry, cx);
        });
        self.editor.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// As `go_to_line`, the keyboard left where it is.
    pub(super) fn jump(&mut self, line: u32, col: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.jump_to_position(Position::new(line, col), JUMP_AT, window, cx);
        });
    }
}

impl FileView {
    pub(super) fn is_busy(&self) -> bool {
        self.lsp_client.as_ref().is_some_and(|c| c.busy_for(&self.path).is_some())
    }

    /// The server runs, but its import of the build failed.
    pub(super) fn lsp_problem(&self) -> Option<String> {
        self.lsp_client.as_ref().and_then(|c| c.problem())
    }

    pub(super) fn status_text(&self) -> String {
        // SQL has no server to offer: the line over its text says what it is connected to.
        if self.sql.is_some() && self.lsp_program.is_empty() {
            return String::new();
        }
        let busy = self.lsp_client.as_ref().and_then(|c| c.busy_for(&self.path));
        match (self.lsp_program.is_empty(), self.lsp_ok, busy) {
            (true, _, _) => self.lsp_status.to_string(),
            (false, Some(true), Some(work)) => format!("{}: {work}…", self.lsp_program),
            (false, Some(true), None) if self.lsp_problem().is_some() => {
                let problem = self.lsp_problem().unwrap_or_default();
                let short: String = problem.chars().take(90).collect();
                format!("{}: {short}{}", self.lsp_program, if short.len() < problem.len() { "…" } else { "" })
            }
            (false, Some(true), None) if self.lsp_status == "ready" => format!("{} ●", self.lsp_program),
            (false, _, _) => format!("{}: {}", self.lsp_program, self.lsp_status),
        }
    }
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let veil = self.veiled && crate::workspace::present::on() && crate::state::with_settings(|s| s.present_veil);
        let warning = hsla(0.08, 0.9, 0.55, 1.);
        v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| {
                // Saved by hand, and asked to be tidied first: saved when the server has answered, or as it is when there is none.
                if crate::state::with_settings(|s| s.format_on_save) && this.lsp_client.is_some() {
                    return this.format_then(true, window, cx);
                }
                this.save(cx)
            }))
            .on_action(cx.listener(|this, _: &ExpandSelection, _, cx| this.expand_selection(cx)))
            .on_action(cx.listener(|this, _: &ShrinkSelection, _, cx| this.shrink_selection(cx)))
            .on_action(cx.listener(|this, _: &ToggleComment, window, cx| this.toggle_comment(window, cx)))
            .on_action(cx.listener(|this, _: &DeleteLine, window, cx| this.edit(window, cx, |text, selection| Some(crate::edit::delete_lines(text, selection)))))
            .on_action(cx.listener(|this, _: &DuplicateLine, window, cx| this.edit(window, cx, |text, selection| Some(crate::edit::duplicate(text, selection)))))
            .on_action(cx.listener(|this, _: &MoveLineUp, window, cx| this.edit(window, cx, |text, selection| crate::edit::move_lines(text, selection, true))))
            .on_action(cx.listener(|this, _: &MoveLineDown, window, cx| this.edit(window, cx, |text, selection| crate::edit::move_lines(text, selection, false))))
            .on_action(cx.listener(|this, _: &JoinLines, window, cx| this.edit(window, cx, crate::edit::join_lines)))
            .on_action(cx.listener(|this, _: &ToggleCase, window, cx| this.edit(window, cx, crate::edit::toggle_case)))
            .on_action(cx.listener(|this, _: &NextOccurrence, _, cx| this.select_next_occurrence(cx)))
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| this.go_to_line_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &RenameSymbol, window, cx| this.rename_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &NextProblem, window, cx| this.go_to_problem(true, window, cx)))
            .on_action(cx.listener(|this, _: &PrevProblem, window, cx| this.go_to_problem(false, window, cx)))
            .on_action(cx.listener(|this, _: &FormatFile, window, cx| {
                if this.sql.is_some() && this.lsp_client.is_none() {
                    return this.format_sql(window, cx);
                }
                this.format(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ParameterInfo, _, cx| this.parameter_info(cx)))
            .on_action(cx.listener(|this, _: &QuickDocumentation, window, cx| this.quick_documentation(window, cx)))
            .on_action(cx.listener(|this, _: &OptimizeImports, window, cx| this.optimize_imports(window, cx)))
            .on_action(cx.listener(|this, _: &Fold, _, cx| this.fold(true, false, cx)))
            .on_action(cx.listener(|this, _: &Unfold, _, cx| this.fold(false, false, cx)))
            .on_action(cx.listener(|this, _: &FoldAll, _, cx| this.fold(true, true, cx)))
            .on_action(cx.listener(|this, _: &UnfoldAll, _, cx| this.fold(false, true, cx)))
            .on_action(cx.listener(|this, _: &ToggleBlame, _, cx| this.toggle_blame(cx)))
            .on_action(cx.listener(|this, _: &CycleLook, window, cx| this.cycle_look(window, cx)))
            .on_action(cx.listener(|this, _: &super::sql::RunStatement, window, cx| this.run_statement(window, cx)))
            .on_action(cx.listener(|this, _: &super::sql::RunFile, window, cx| this.run_file(window, cx)))
            .on_action(cx.listener(|this, _: &super::sql::ExplainStatement, window, cx| this.explain_statement(window, cx)))
            .on_action(cx.listener(|this, _: &super::sql::SqlDefinition, window, cx| this.sql_definition(window, cx)))
            .on_action(cx.listener(|this, _: &super::sql::ShowTableData, window, cx| this.show_table_data(window, cx)))
            .on_action(cx.listener(|this, _: &super::sql::ShowTableDefinition, _, cx| this.show_ddl_at_cursor(cx)))
            .on_action(cx.listener(|this, _: &super::sql::StopStatement, _, cx| this.stop_statement(cx)))
            .on_action(cx.listener(|this, _: &ToggleBookmark, _, cx| this.toggle_bookmark(cx)))
            .on_action(cx.listener(|this, _: &NextBookmark, window, cx| this.go_to_bookmark(false, window, cx)))
            .on_action(cx.listener(|this, _: &PrevBookmark, window, cx| this.go_to_bookmark(true, window, cx)))
            .child(
                h_flex()
                    .justify_between()
                    .gap_4()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.45))
                            .truncate()
                            .child(crate::copying::text("file-path", if crate::db::scratch::is_scratch(&self.path) {
                                format!("Scratch: {}", self.path.file_name().unwrap_or_default().to_string_lossy())
                            } else {
                                self.path.strip_prefix(&self.root).unwrap_or(&self.path).display().to_string()
                            })),
                    )
                    .children(self.render_blame(cx))
                    .children(self.render_look_switch(cx))
                    .children(self.render_page_button())
                    .when(!crate::ide::installed().is_empty(), |header| {
                        let path = self.path.clone();
                        let line = self.cursor_line + 1;
                        header.child(
                            Button::new("open-in")
                                .xsmall()
                                .ghost()
                                .label("Open in ▾")
                                .dropdown_menu(move |mut menu, _, _| {
                                    for ide in crate::ide::installed() {
                                        let (ide, path) = (ide.clone(), path.clone());
                                        menu = menu.item(
                                            gpui_kit::component::menu::PopupMenuItem::new(ide.name)
                                                .on_click(move |_, _, _| ide.open(&path, line)),
                                        );
                                    }
                                    menu
                                }),
                        )
                    })
                    .children(crate::predict::badge(cx))
                    .child(
                        div()
                            .id("lsp-status")
                            .flex_none()
                            .when(self.lsp_ok == Some(true) && !self.is_busy(), |s| s.text_color(gpui_kit::green()))
                            // How to use it, where it takes no room.
                            .when(self.lsp_ok == Some(true) && self.lsp_problem().is_none(), |s| {
                                let told = format!(
                                    "The language server is ready: hover a name for what it is, {} or Ctrl+click to go to where it is declared",
                                    crate::keymap::label("definition")
                                );
                                s.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(told.clone()).build(window, cx))
                            })
                            .when(self.lsp_ok == Some(false) && self.lsp_status.contains("failed"), |s| {
                                s.text_color(gpui_kit::red())
                            })
                            .when_some(self.lsp_problem().filter(|_| !self.is_busy()), |s, problem| {
                                s.text_color(gpui_kit::red()).tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(problem.clone()).build(window, cx)
                                })
                            })
                            .child(if self.lsp_missing.is_some_and(crate::lsp_install::is_installing) {
                                format!("installing {}…", self.lsp_missing.unwrap_or_default())
                            } else {
                                self.status_text()
                            }),
                    )
                    .when(self.lsp_unlicensed && self.lsp_ok == Some(true), |header| {
                        header.child(
                            Button::new("lsp-licence")
                                .xsmall()
                                .ghost()
                                .label("no licence")
                                .text_color(hsla(0.12, 0.9, 0.5, 1.))
                                .tooltip(
                                    "intelephense runs without a licence: no rename, go to implementation, inlay hints or code actions. \
                                     Enter a licence key in Settings, or choose phpactor for PHP there.",
                                )
                                .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::LanguageServerSettings), cx)),
                        )
                    })
                    .when_some(self.lsp_missing.filter(|p| !crate::lsp_install::is_installing(p)), |header, program| {
                        let task = self.task.clone();
                        header.when(crate::lsp_install::recipe(program).is_some(), |header| {
                            header.child(Button::new("lsp-install").xsmall().ghost().label("Install…").on_click(
                                move |_, window, cx| {
                                    let _ = task.update(cx, |t, cx| t.confirm_lsp_install(program, window, cx));
                                },
                            ))
                        })
                    }),
            )
            .when(self.conflict, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .bg(warning.opacity(0.15))
                        .child(div().flex_1().child("Changed on disk while you have unsaved edits."))
                        .child(
                            Button::new("reload")
                                .xsmall()
                                .ghost()
                                .label("Load disk version")
                                .on_click(cx.listener(|this, _, window, cx| this.reload_from_disk(window, cx))),
                        )
                        .child(
                            Button::new("keep")
                                .xsmall()
                                .ghost()
                                .label("Keep mine")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.conflict = false;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(div().px_3().py_1().child(crate::copying::message("file-error", error, gpui_kit::red(), cx)))
            })
            .when(veil, |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .text_color(cx.theme().muted_foreground)
                        .child("Git ignores this file: it is not shown while presenting.")
                        .child(Button::new("unveil").small().outline().label("Show it anyway").on_click(cx.listener(|this, _, _, cx| {
                            this.veiled = false;
                            cx.notify();
                        }))),
                )
            })
            .when(!veil, |this| {
                let look = if self.preview_kind().is_some() { self.preview.look } else { super::preview::Look::Code };
                let preview = (look != super::preview::Look::Code).then(|| self.render_preview(window, cx));
                if look == super::preview::Look::Preview {
                    return this.children(preview);
                }
                let kind = self.preview_kind();
                let share = kind.filter(|_| look == super::preview::Look::Split).map(super::preview::split_share);
                let is_sql = self.sql.is_some();
                let sql_share = self.sql_panel_open().then(super::sql::text_share);
                let sql_results = self.render_sql_results(cx);
                this.children(self.render_sql_bar(cx))
                .child(h_flex().id("file-split").test_support().flex_1().min_h_0().items_stretch()
                .when(sql_share.is_some(), |split| split.flex_col())
                .on_drag_move(cx.listener(|this, event: &DragMoveEvent<super::preview::SplitDrag>, _, cx| this.drag_split(event, cx)))
                .on_drag_move(cx.listener(|this, event: &DragMoveEvent<super::sql::SqlSplitDrag>, _, cx| this.drag_sql_split(event, cx)))
                .child(
                div()
                    .id("file-editor")
                    .test_support()
                    .when(is_sql, |editor| editor.key_context(super::sql::SQL_CONTEXT))
                    .relative()
                    .flex_1()
                    .when_some(share, |editor, share| editor.flex_none().w(relative(share)))
                    .when_some(sql_share, |editor, share| editor.flex_none().w_full().h(relative(share)))
                    .min_w_0()
                    .min_h_0()
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !hovered && this.blame_hover.take().is_some() {
                            cx.notify();
                        }
                    }))
                    .children(self.render_signature(cx))
                    .children(self.render_blame_hover(cx))
                    .children(self.render_change_popup(cx))
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if this.change_popup.is_some() && event.keystroke.key == "escape" {
                            this.close_change(cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(crate::zoom::wheel(crate::zoom::Kind::Editor)).child(
                    Editor::new(&self.editor)
                        // Replaces the built-in menu, so its items are listed again.
                        .context_menu({
                            let (editor, annotating, sql_file, server) = (self.editor.clone(), self.annotate, self.sql.is_some(), self.lsp_client.is_some());
                            move |menu, _, cx| {
                            let icon = crate::sidebar::menu_icon;
                            // On the line numbers: what is about the lines.
                            if editor.read(cx).gutter_press().is_some() {
                                let blame = if annotating { "Close annotations" } else { "Annotate with git blame" };
                                return menu.menu_with_icon(blame, icon("history"), Box::new(ToggleBlame)).menu_with_icon("Toggle bookmark", icon("bookmark"), Box::new(ToggleBookmark));
                            }
                            use gpui_kit::component::input::{Copy, Cut, GoToDefinition, Paste, SelectAll, ToggleCodeActions};
                            let fixes = editor.read(cx).context_menu_capabilities().has_code_actions();
                            // What a name is in SQL: there are no implementations to list.
                            let menu = if sql_file {
                                menu.menu_with_icon("Run", icon("play"), Box::new(super::sql::RunStatement))
                                    .menu("Explain", Box::new(super::sql::ExplainStatement))
                                    .menu_with_icon("Format", icon("wand-sparkles"), Box::new(FormatFile))
                                    .separator()
                                    .menu("Go to definition or schema", Box::new(super::sql::SqlDefinition))
                                    .menu("Find usages", Box::new(ShowUsages))
                                    .menu("Show table data", Box::new(super::sql::ShowTableData))
                                    .menu("Show table DDL", Box::new(super::sql::ShowTableDefinition))
                            } else {
                                // Ctrl+B on a declaration lists its usages.
                                let menu = menu.menu("Go to declaration or usages", Box::new(GoToDefinition)).menu("Implementations and usages", Box::new(ShowUsages));
                                // What only a language server can do: shown where there is one.
                                if server {
                                    menu.separator()
                                        .menu_with_icon("Rename…", icon("pencil"), Box::new(RenameSymbol))
                                        .menu_with_icon_disabled("Quick fixes", icon("lightbulb"), !fixes, Box::new(ToggleCodeActions))
                                        .menu_with_icon("Reformat file", icon("wand-sparkles"), Box::new(FormatFile))
                                } else {
                                    menu
                                }
                            };
                            menu.separator()
                                .menu_with_icon("Send to agent", icon("send"), Box::new(SendToAgent))
                                .separator()
                                .menu_with_icon("Cut", icon("scissors"), Box::new(Cut))
                                .menu_with_icon("Copy", icon("copy"), Box::new(Copy))
                                .menu_with_icon("Paste", icon("clipboard-paste"), Box::new(Paste))
                                .menu("Select all", Box::new(SelectAll))
                                .separator()
                                .menu("Comment / uncomment", Box::new(ToggleComment))
                                .menu_with_icon("File structure", icon("list-tree"), Box::new(ShowOutline))
                                .separator()
                                .menu("Copy reference", Box::new(CopyReference))
                                .menu("Copy path", Box::new(CopyFilePath))
                                .menu_with_icon("Show in Files", icon("locate-fixed"), Box::new(ShowFileInFiles))
                                .separator()
                                .menu_with_icon("Compare with branch, tag or commit…", icon("git-compare"), Box::new(CompareFileWith))
                        }})
                        .bordered(false)
                        .p_0()
                        .h_full()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(crate::zoom::size(crate::zoom::Kind::Editor, cx)),
                ),
                )
                .when_some(kind.zip(preview), |split, (kind, preview)| {
                    split.child(super::preview::split_handle(kind, cx)).child(div().flex_1().min_w_0().flex().child(preview))
                })
                .when_some(sql_results, |split, results| {
                    split.child(super::sql::sql_handle(cx)).child(div().flex_1().min_h_0().min_w_0().w_full().child(results))
                })
                .children(self.scroll_watch(cx)))
            })
    }
}

/// Bridges the editor's hover and definition hooks to a language server.
pub(super) struct LspBridge {
    pub(super) client: Arc<lsp::LspClient>,
    pub(super) uri: String,
    pub(super) path: PathBuf,
    pub(super) file: WeakEntity<FileView>,
}

impl CompletionProvider for LspBridge {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: lsp_types::CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<lsp_types::CompletionResponse>> {
        let pos = text.offset_to_position(offset);
        // The rope is shared, not copied: asked on every letter, and the
        // text is made of it where the server is told.
        let (client, uri, path, text) = (self.client.clone(), self.uri.clone(), self.path.clone(), text.clone());
        cx.spawn(async move |cx| {
            // The next letter drops this before it has cost anything.
            cx.background_executor().timer(Duration::from_millis(60)).await;
            cx.background_spawn(crate::procs::on_thread(move || {
                // The server answers about the text it has: this one, typed a moment ago.
                client.did_change(&path, 0, &text.to_string())?;
                let items = match client.completion(&uri, pos, trigger.trigger_character)? {
                    lsp_types::CompletionResponse::Array(items) => items,
                    lsp_types::CompletionResponse::List(list) => list.items,
                };
                let typed: String = text.chars_at(offset).reversed().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                let typed: String = typed.chars().rev().collect();
                Ok(lsp_types::CompletionResponse::Array(lsp::arrange_completions(items, &typed)))
            }))
            .await
        })
    }

    /// What the server adds to an item taken from the menu: what it left
    /// out of the list (asked for now), the edits beside it (an import),
    /// then its command, whose edits and caret the server asks back for.
    fn completion_inserted(
        &self,
        item: &lsp_types::CompletionItem,
        before: &Rope,
        range: std::ops::Range<usize>,
        after: &Rope,
        window: &mut Window,
        cx: &mut App,
    ) {
        let resolve = item.additional_text_edits.is_none() && self.client.resolves_completions();
        if !resolve && item.additional_text_edits.as_ref().is_none_or(Vec::is_empty) && item.command.is_none() {
            return;
        }
        let inserted = after.len() + range.len() - before.len();
        let (client, path, file, item) = (self.client.clone(), self.path.clone(), self.file.clone(), item.clone());
        let (before, after) = (before.clone(), after.clone());
        let uri = lsp::path_to_uri(&path);
        window
            .spawn(cx, async move |cx| {
                let item = if resolve {
                    let (client, asked) = (client.clone(), item.clone());
                    let resolved = cx.background_spawn(crate::procs::on_thread(move || client.resolve_completion(&uri, &asked))).await;
                    // Some servers answer with the item alone, without what it had.
                    resolved.map(|r| lsp_types::CompletionItem { command: r.command.clone().or(item.command.clone()), ..r }).unwrap_or(item)
                } else {
                    item
                };
                if let Some(edits) = item.additional_text_edits.filter(|e| !e.is_empty()) {
                    let unchanged = after.clone();
                    let new = cx
                        .background_spawn(async move {
                            lsp::apply_edits_around(&before.to_string(), range, inserted, &after.to_string(), &edits)
                        })
                        .await;
                    file.update_in(cx, |file, window, cx| {
                        if *file.editor.read(cx).text() == unchanged {
                            file.edit(window, cx, |text, selection| crate::edit::rewrite(text, &new, selection.start));
                        }
                    })?;
                }
                let Some(command) = item.command else { return anyhow::Ok(()) };
                let text = file.read_with(cx, |file, cx| file.editor.read(cx).text().clone())?;
                let done = cx
                    .background_spawn(crate::procs::on_thread(move || {
                        client.did_change(&path, 0, &text.to_string())?;
                        client.execute_command(&command)
                    }))
                    .await?;
                file.update_in(cx, |file, window, cx| {
                    for edit in &done.edits {
                        file.apply_workspace_edit(edit, window, cx, |_, _, _| {});
                    }
                    let Some((uri, selection)) = done.show else { return };
                    if lsp::uri_to_path(&uri).as_deref() != Some(file.path.as_path()) {
                        return;
                    }
                    let text = file.editor.read(cx).text().to_string();
                    let (start, end) = (lsp::offset_of(&text, &selection.start), lsp::offset_of(&text, &selection.end));
                    file.editor.update(cx, |state, cx| state.set_selected_range(start..end, cx));
                })
            })
            .detach();
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        let Some(last) = new_text.chars().last() else { return false };
        // Off: only when asked for, which does not come this way.
        if !crate::state::with_settings(|s| s.complete_as_typed) {
            return false;
        }
        last.is_alphanumeric() || last == '_' || self.client.completion_triggers().iter().any(|t| new_text.ends_with(t.as_str()))
    }
}

impl DocumentRangeSemanticTokensProvider for LspBridge {
    fn legend(&self) -> lsp_types::SemanticTokensLegend {
        let token_types = self.client.semantic_legend().into_iter().map(lsp_types::SemanticTokenType::from).collect();
        lsp_types::SemanticTokensLegend { token_types, token_modifiers: Vec::new() }
    }

    fn semantic_tokens(
        &self,
        text: &Rope,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<lsp_types::SemanticTokens>> {
        if !crate::state::with_settings(|s| s.semantic_colors) {
            return Task::ready(Ok(lsp_types::SemanticTokens::default()));
        }
        let range = lsp_types::Range::new(text.offset_to_position(range.start), text.offset_to_position(range.end));
        let (client, uri, path, text) = (self.client.clone(), self.uri.clone(), self.path.clone(), text.clone());
        cx.background_spawn(crate::procs::on_thread(move || {
            client.did_change(&path, 0, &text.to_string())?;
            let tokens = client.semantic_tokens(&uri, range);
            if std::env::var_os("IDEAE_DEBUG").is_some() {
                eprintln!("semantic tokens {uri} -> {:?}", tokens.as_ref().map(|t| t.data.len()));
            }
            tokens
        }))
    }
}

impl InlayHintsProvider for LspBridge {
    /// The server has the text already: it is told of each edit before the
    /// editor asks, after a longer pause.
    fn inlay_hints(&self, text: &Rope, range: std::ops::Range<usize>, _: &mut Window, cx: &mut App) -> Task<anyhow::Result<Vec<lsp_types::InlayHint>>> {
        if !crate::state::with_settings(|s| s.inlay_hints) {
            return Task::ready(Ok(Vec::new()));
        }
        let range = lsp_types::Range::new(text.offset_to_position(range.start), text.offset_to_position(range.end));
        let (client, uri) = (self.client.clone(), self.uri.clone());
        cx.background_spawn(crate::procs::on_thread(move || client.inlay_hints(&uri, range)))
    }
}

impl CodeActionProvider for LspBridge {
    fn id(&self) -> SharedString {
        "lsp".into()
    }

    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<lsp_types::CodeAction>>> {
        let (text, range) = {
            let text = state.read(cx).text();
            (text.clone(), lsp_types::Range::new(text.offset_to_position(range.start), text.offset_to_position(range.end)))
        };
        let (client, uri, path) = (self.client.clone(), self.uri.clone(), self.path.clone());
        cx.background_spawn(crate::procs::on_thread(move || {
            client.did_change(&path, 0, &text.to_string())?;
            client.code_actions(&uri, range)
        }))
    }

    fn perform_code_action(
        &self,
        _: Entity<EditorState>,
        action: lsp_types::CodeAction,
        _: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let (client, file, uri) = (self.client.clone(), self.file.clone(), lsp::path_to_uri(&self.path));
        window.spawn(cx, async move |cx| {
            let action = match action.edit {
                Some(_) => action,
                None => cx.background_spawn(crate::procs::on_thread(move || client.resolve_code_action(&uri, &action))).await?,
            };
            let Some(edit) = action.edit else { return Ok(()) };
            file.update_in(cx, |file, window, cx| {
                file.apply_workspace_edit(&edit, window, cx, |_, _, _| {});
            })
        })
    }
}

impl HoverProvider for LspBridge {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<lsp_types::Hover>>> {
        let pos = text.offset_to_position(offset);
        if std::env::var_os("IDEAE_DEBUG").is_some() {
            eprintln!("hover requested at {pos:?}");
        }
        let (client, uri) = (self.client.clone(), self.uri.clone());
        cx.background_spawn(crate::procs::on_thread(move || {
            let result = client.hover(&uri, pos).inspect_err(|e| eprintln!("hover: {e:#}"));
            if std::env::var_os("IDEAE_DEBUG").is_some() {
                eprintln!("hover {uri} {pos:?} -> {}", if matches!(result, Ok(Some(_))) { "some" } else { "none/err" });
            }
            result
        }))
    }
}

impl DefinitionProvider for LspBridge {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<lsp_types::LocationLink>>> {
        let pos = text.offset_to_position(offset);
        let (client, uri) = (self.client.clone(), self.uri.clone());
        cx.background_spawn(crate::procs::on_thread(move || {
            let result = client.definition(&uri, pos).inspect_err(|e| eprintln!("definition: {e:#}"));
            if std::env::var_os("IDEAE_DEBUG").is_some() {
                eprintln!("definition {uri} {pos:?} -> {result:?}");
            }
            result
        }))
    }
}

/// Errors first, then warnings; hints and notes are no stops for F2.
fn problem_rank(severity: gpui_kit::component::input::DiagnosticSeverity) -> u8 {
    use gpui_kit::component::input::DiagnosticSeverity as S;
    match severity {
        S::Error => 2,
        S::Warning => 1,
        _ => 0,
    }
}

/// A line's words, enough of them to know it again.
fn fold_words(text: &Rope, line: usize) -> String {
    if line >= text.lines_len() {
        return String::new();
    }
    text.slice_line(line).to_string().trim().chars().take(120).collect()
}

/// The line a saved fold is on now: where it was, or the nearest with its
/// words; none when they are gone, not to fold something else.
fn place_fold(text: &Rope, fold: &crate::state::SavedFold) -> Option<usize> {
    const NEAR: usize = 200;
    let line = fold.line as usize;
    let here = |row: usize| fold_words(text, row) == fold.text && !fold.text.is_empty();
    (0..=NEAR).find_map(|away| {
        if here(line + away) {
            Some(line + away)
        } else {
            line.checked_sub(away).filter(|row| here(*row))
        }
    })
}

/// Line ranges of what spans lines and is worth keeping in sight, by first
/// line: every tag or key would take the few sticky rows from what matters.
fn sticky_scopes(path: &std::path::Path, text: &str) -> Vec<std::ops::Range<usize>> {
    let mut scopes: Vec<_> = super::outline(path, text).into_iter().filter(|i| i.end > i.line && !matches!(i.kind.as_str(), "tag" | "key")).map(|i| i.line as usize..i.end as usize).collect();
    scopes.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
    scopes
}

#[cfg(test)]
mod preview_tests {
    use gpui_kit::{ImageSource, Resource, SharedUri};

    #[test]
    fn a_preview_s_picture_is_beside_the_file_unless_it_has_a_scheme() {
        let dir = std::path::Path::new("/repo");
        let of = |link: &str| super::super::preview::preview_image(dir, &SharedUri::from(link.to_string()));
        assert!(matches!(of("docs/images/a.png?raw=1"), ImageSource::Resource(Resource::Path(p)) if p.as_ref() == std::path::Path::new("/repo/docs/images/a.png")));
        assert!(matches!(of("https://example.com/a.png"), ImageSource::Resource(Resource::Uri(_))));
    }
}

#[cfg(test)]
mod sticky_tests {
    #[test]
    fn declarations_that_span_lines_are_the_sticky_scopes() {
        let text = "impl A {\n    fn one() {\n        x();\n    }\n    fn two() {}\n}\nconst N: u8 = 1;\n";
        assert_eq!(super::sticky_scopes(std::path::Path::new("a.rs"), text), [0..5, 1..3]);
    }

    #[test]
    fn regions_stick_and_tags_and_keys_do_not() {
        let sql = "#region A\nselect 1;\n#endregion\n";
        assert_eq!(super::sticky_scopes(std::path::Path::new("a.sql"), sql), [0..2]);
        let html = "<div>\n<p>\nx\n</p>\n</div>\n";
        assert!(super::sticky_scopes(std::path::Path::new("a.html"), html).is_empty());
        assert!(super::sticky_scopes(std::path::Path::new("a.yaml"), "a:\n  b: 1\n").is_empty());
    }

    #[test]
    fn a_saved_fold_follows_its_line_or_is_dropped() {
        use crate::state::SavedFold;
        let text = super::Rope::from("new\nnew\n  #region A\nx\n#endregion\n");
        let fold = |line, text: &str| SavedFold { line, text: text.into() };
        assert_eq!(super::place_fold(&text, &fold(2, "#region A")), Some(2));
        assert_eq!(super::place_fold(&text, &fold(0, "#region A")), Some(2));
        assert_eq!(super::place_fold(&text, &fold(9, "#region A")), Some(2));
        assert_eq!(super::place_fold(&text, &fold(2, "#region B")), None);
    }
}
