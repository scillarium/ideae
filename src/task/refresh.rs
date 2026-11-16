use super::*;

impl TaskView {
    pub(super) fn apply_patch(&mut self, parsed: ParsedPatch, cx: &mut Context<Self>) {
        let ParsedPatch { files, first_lines, hashes, hunks, marked } = parsed;
        self.uncommitted_hunks = Rc::new(hunks);
        // Never 0, which is no key to the diff view.
        self.uncommitted_keys = Rc::new(hashes.iter().map(|(path, hash)| (path.clone(), *hash | 1)).collect());
        if self.conflict_marked != marked {
            self.conflict_marked = marked;
            self.markers_accepted = false;
        }
        self.files = files
            .iter()
            .map(|f| ChangedFile {
                path: f.path().clone(),
                hash: hashes.get(f.path().as_ref()).copied().unwrap_or(0),
                first_line: first_lines.get(f.path().as_ref()).copied().unwrap_or(1),
                status: f.status(),
                additions: f.additions(),
                deletions: f.deletions(),
            })
            .collect();
        // A file that is no longer changed is no longer viewed either.
        let changed: HashSet<SharedString> = self.files.iter().map(|f| f.path.clone()).collect();
        self.viewed.retain(|path, _| changed.contains(path.as_str()));
        self.changed_paths = Rc::new(changed);
        self.uncommitted_diff = files;
        self.uncommitted_lines = Rc::new(first_lines);
        self.refresh_scope(DiffScope::Uncommitted, cx);
        cx.emit(TaskEvent::Changed);
        cx.notify();
    }

    /// The diff against `base`; `parsed` when it is another one than before.
    /// `shared`: the branch has no commit of its own, and its diff is the
    /// uncommitted one just applied, kept once for both.
    pub(super) fn apply_branch_patch(&mut self, base: Option<String>, parsed: Option<ParsedPatch>, shared: bool, cx: &mut Context<Self>) {
        let base_changed = self.changes.read(cx).base != base;
        if parsed.is_none() && !shared && !base_changed {
            return;
        }
        let paths = |files: &[DiffFile]| Rc::new(files.iter().map(|f| f.path().to_string()).filter(|path| !git::is_lockfile(path)).collect());
        if shared {
            self.branch_paths = paths(&self.uncommitted_diff);
            self.branch_diff = self.uncommitted_diff.clone();
            (self.branch_lines, self.branch_hunks, self.branch_keys) = (self.uncommitted_lines.clone(), self.uncommitted_hunks.clone(), self.uncommitted_keys.clone());
        } else if let Some(parsed) = parsed {
            self.branch_paths = paths(&parsed.files);
            self.branch_diff = parsed.files;
            self.branch_lines = Rc::new(parsed.first_lines);
            self.branch_hunks = Rc::new(parsed.hunks);
            self.branch_keys = Rc::new(parsed.hashes.into_iter().map(|(path, hash)| (path, hash | 1)).collect());
        }
        self.changes.update(cx, |c, _| c.base = base);
        self.refresh_scope(DiffScope::Branch, cx);
    }

    /// New data for `changed`: reload the diff only if that scope is on screen,
    /// so scrolling the other one is not reset.
    pub(super) fn refresh_scope(&mut self, changed: DiffScope, cx: &mut Context<Self>) {
        if self.changes.read(cx).scope == changed {
            self.diff_stale = true;
            self.show_diff_if_seen(cx);
        }
        let counts = (self.uncommitted_diff.len(), self.branch_diff.len());
        self.changes.update(cx, |c, cx| {
            if c.counts != counts {
                c.counts = counts;
                cx.notify();
            }
        });
    }

    /// The Changes tab gets a new diff when it is looked at: laying one out
    /// and colouring it for a tab that is closed, behind another, or in a
    /// task out of view would be done again on every change an agent makes.
    pub(super) fn show_diff_if_seen(&mut self, cx: &mut Context<Self>) {
        let seen = self.visible
            && self.tabs.iter().any(|t| matches!(t.kind, TabKind::Changes) && (t.id == self.left_active || Some(t.id) == self.right_active));
        if self.diff_stale && seen {
            self.diff_stale = false;
            self.show_scope(cx);
        }
    }

    pub fn set_base(&mut self, base: String) {
        *self.base_override.lock().unwrap() = Some(base);
        let _ = self.refresh_tx.try_send(());
    }

    /// Puts the selected scope's files into the Changes diff.
    pub(super) fn show_scope(&mut self, cx: &mut Context<Self>) {
        let counts = (self.uncommitted_diff.len(), self.branch_diff.len());
        let scope = self.changes.read(cx).scope;
        let (files, lines) = match scope {
            DiffScope::Uncommitted => (self.uncommitted_diff.clone(), self.uncommitted_lines.clone()),
            DiffScope::Branch => (self.branch_diff.clone(), self.branch_lines.clone()),
        };
        let hunks = match scope {
            DiffScope::Uncommitted => self.uncommitted_hunks.clone(),
            DiffScope::Branch => self.branch_hunks.clone(),
        };
        let keys = match scope {
            DiffScope::Uncommitted => self.uncommitted_keys.clone(),
            DiffScope::Branch => self.branch_keys.clone(),
        };
        let diff = self.changes.read(cx).diff.clone();
        // The same diff a moment later: it stays where it is read.
        diff.update(cx, |state, cx| {
            let keyed = files.into_iter().map(|file| {
                let key = keys.get(file.path().as_ref()).copied().unwrap_or(0);
                (file, key)
            });
            state.update_files(keyed, cx)
        });
        self.changes.update(cx, |c, cx| {
            // Lines picked in a file that is another patch now are not those lines any more.
            let key_of = |keys: &HashMap<String, u64>, range: &DiffLineRange| keys.get(range.path().as_ref()).copied();
            if c.selection.as_ref().is_some_and(|range| key_of(&c.shown_keys, range) != key_of(&keys, range)) {
                c.selection = None;
                c.selected_code.clear();
            }
            c.shown_keys = keys;
            c.counts = counts;
            c.first_lines = lines;
            c.hunks = hunks;
            cx.notify();
        });
    }

    /// The open files and the time each knows its file by, for
    /// `changed_on_disk`. One being saved is left out.
    pub(super) fn open_files_known(&self, cx: &App) -> (Vec<WeakEntity<FileView>>, Vec<(PathBuf, Option<std::time::SystemTime>)>) {
        self.tabs
            .iter()
            .filter_map(|t| match &t.kind {
                TabKind::File(f) => Some((f.downgrade(), f.read(cx).disk_known()?)),
                _ => None,
            })
            .unzip()
    }


    pub(super) fn apply_files(&mut self, files: Arc<Vec<String>>, cx: &mut Context<Self>) {
        // The list read is the one held unless a file came or went: nothing to compare then.
        if Arc::ptr_eq(&files, &self.tree_files) || *files == *self.tree_files {
            return;
        }
        // One list, shared by the tree, the finder and the lookups off the UI thread.
        *self.shared_files.write().unwrap() = files.clone();
        // Class names for terminal links: `Profit` in output links to Profit.kt.
        gpui_cn::terminal::add_known_names(files.iter().filter_map(|f| {
            let name = f.rsplit('/').next()?;
            let stem = name.split('.').next()?;
            (stem.len() > 2 && stem.chars().next()?.is_ascii_uppercase()).then(|| stem.to_string())
        }));
        self.tree_files = files;
        let _ = self.mark_tree();
        self.tree_stale = true;
        if crate::workspace::present::on() {
            self.veil_ignored(cx);
        }
        self.fill_tree(cx);
    }

    pub(super) fn apply_branch(&mut self, info: git::BranchInfo, cx: &mut Context<Self>) {
        if info == self.branch_info {
            return;
        }
        if !info.head.is_empty() {
            self.branch = info.head.clone().into();
        }
        self.history.update(cx, |h, cx| h.sync_head(&info.last_hash, cx));
        // A new commit changes who last touched the lines.
        if info.last_hash != self.branch_info.last_hash {
            for tab in &self.tabs {
                if let TabKind::File(file) = &tab.kind {
                    file.update(cx, |f, cx| f.load_blame(cx));
                }
            }
        }
        self.branch_info = info;
        cx.emit(TaskEvent::Changed);
        cx.notify();
    }
}
