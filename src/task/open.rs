use super::*;

impl TaskView {
    pub fn open_file(&mut self, path: PathBuf, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_at(path, line, 0, window, cx)
    }

    /// Opens `path` (or its tab) with the cursor at 0-based `line`, `col`,
    /// in the preview tab.
    pub fn open_file_at(&mut self, path: PathBuf, line: u32, col: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_as(path, line, col, OpenAs::Preview, window, cx)
    }

    pub fn open_file_as(&mut self, path: PathBuf, line: u32, col: u32, how: OpenAs, window: &mut Window, cx: &mut Context<Self>) {
        // A remote file read in the background, back here: taken whatever comes of it, so
        // none is kept to be used later in place of a fresh read; recorded the first time.
        let fetched = self.remote_texts.remove(&path);
        if fetched.is_none() {
            self.record_nav(cx);
        }
        if let Some(ix) = self.tabs.iter().position(|t| match &t.kind {
            TabKind::File(f) => f.read(cx).path == path,
            TabKind::Binary(b) => b.read(cx).path == path,
            _ => false,
        }) {
            if how != OpenAs::Preview {
                self.tabs[ix].preview = false;
            }
            self.activate(ix, window, cx);
            if let TabKind::File(file) = &self.tabs[ix].kind {
                let visible = self.visible;
                file.update(cx, |f, cx| if visible { f.go_to_line(line, col, window, cx) } else { f.jump(line, col, window, cx) });
            }
            return;
        }

        if let Some((kind, _)) = crate::image_diff::kind(&path) {
            let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let view = cx.new(|cx| BinaryView::image(path.clone(), kind, cx));
            self.place_tab(TabItem::new(title, TabKind::Binary(view)), how, window, cx);
            return;
        }
        // A remote task's file comes from its host off the UI thread: opened once it is here.
        let (read, disk_mtime) = match fetched {
            Some((read, mtime)) => (read, Some(mtime)),
            None if crate::hosts::is_remote(&path) => {
                let at = path.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let file = at.clone();
                    let read = cx.background_spawn(async move { (read_text(&file), super::file::mtime(&file)) }).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.remote_texts.insert(at.clone(), read);
                        this.open_file_as(at, line, col, how, window, cx);
                    });
                })
                .detach();
                return;
            }
            None => (read_text(&path), None),
        };
        let content = match read {
            Ok(content) => content,
            Err(reason) => {
                let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let view = cx.new(|_| BinaryView { path: path.clone(), reason, picture: None });
                self.place_tab(TabItem::new(title, TabKind::Binary(view)), how, window, cx);
                return;
            }
        };
        let language = lsp::language_for(&path);
        let veiled = self.ignores(&path);
        let indent = crate::indent::of(&path, &content, &self.root);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .tab_size(gpui_kit::component::input::TabSize { tab_size: indent.size, hard_tabs: indent.tabs })
                .language(language)
                .line_number(true)
                .indent_guides(true)
                .soft_wrap(false)
        });
        editor.update(cx, |state, cx| {
            state.set_value(content.clone(), window, cx);
            state.jump_to_position(Position::new(line, col), JUMP_AT, window, cx);
        });
        let predict = crate::predict::provider(super::sql_schema::completions(&path), &path, &self.root, editor.read(cx).text().clone(), cx);
        editor.update(cx, |state, _| {
            let lsp = state.lsp_mut();
            lsp.completion_provider = Some(predict);
            // Kotlin's and Java's names with their types do not fit in 320 px.
            lsp.completion_menu.max_width = px(560.);
        });

        let saved = super::file::Print::of(&content);
        let root = self.root.clone();
        let task = cx.weak_entity();
        let file = cx.new(|cx| {
            // The editor notifies on cursor moves too: follow the line for blame.
            cx.observe(&editor, |this: &mut FileView, editor, cx| {
                if this.sql.is_some() {
                    this.sql_cursor_moved(cx);
                }
                let Position { line, character } = editor.read(cx).cursor_position();
                this.cursor_col = character;
                if let Some(name) = &this.doc_shown {
                    let cursor = editor.read(cx).cursor();
                    if !(name.start..=name.end).contains(&cursor) {
                        this.doc_shown = None;
                        editor.update(cx, |state, cx| state.clear_hover_state(cx));
                    }
                }
                // The popup of a change follows its line as the editor scrolls.
                if line != this.cursor_line || this.change_popup.is_some() {
                    let moved = line != this.cursor_line;
                    this.cursor_line = line;
                    if moved {
                        this.show_line_blame(cx);
                    }
                    cx.notify();
                }
            })
            .detach();
            // A diagram is drawn in the theme's colors.
            cx.observe_global::<gpui_kit::component::Theme>(|this: &mut FileView, cx| {
                // An SVG's currentColor is the theme's text.
                if matches!(this.preview_kind(), Some(super::preview::Kind::Mermaid | super::preview::Kind::Svg)) {
                    this.refresh_preview(false, cx);
                }
            })
            .detach();
            cx.subscribe(&editor, |this: &mut FileView, _, event: &InputEvent, cx| {
                match event {
                InputEvent::Change => this.on_edit(cx),
                // Saved when you leave the editor, as JetBrains IDEs do.
                InputEvent::Blur => {
                    this.signature = None;
                    this.autosave(cx)
                }
                _ => {}
            }})
            .detach();
            FileView {
            path: path.clone(),
            editor: editor.clone(),
            indent,
            saved,
            // A remote file's, read with it off the UI thread.
            disk_mtime: disk_mtime.unwrap_or_else(|| mtime(&path)),
            dirty: false,
            conflict: false,
            error: None,
            lsp_version: 1,
            edit_version: 0,
            _lsp_sync: Task::ready(()),
            saving: None,
            save_seq: 0,
            write_gate: Arc::default(),
            _save: Task::ready(()),
            expand_stack: Vec::new(),
            last_expanded: None,
            root: root.clone(),
            task: task.clone(),
            blame: Rc::default(),
            cursor_line: line,
            cursor_col: col,
            _blame: Task::ready(()),
            annotate: false,
            annotations: None,
            blame_hover: None,
            lsp_status: SharedString::default(),
            lsp_ok: Some(false),
            lsp_client: None,
            lsp_program: String::new(),
            lsp_missing: None,
            lsp_attach: 0,
            lsp_unlicensed: false,
            label_room: Default::default(),
            preview: Default::default(),
            sql: super::sql::is_sql(&path).then(|| super::sql::SqlFile::new(&path)),
            http: super::http::is_http(&path).then(|| super::http::HttpFile::new(&path)),
            veiled,
            occurrence: None,
            committed: None,
            _marks: Task::ready(()),
            change_runs: Rc::default(),
            change_popup: None,
            doc_shown: None,
            problems_seen: 0,
            signature: None,
            _signature: Task::ready(()),
            _scopes: Task::ready(()),
            // Re-render while the server reports progress, so the header can
            // say "indexing" instead of a misleading "ready".
            _poll: cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
                let mut last = None;
                let mut ticks = 0u32;
                loop {
                    cx.background_executor().timer(Duration::from_millis(500)).await;
                    ticks += 1;
                    // Out of view, every few seconds is enough: nobody looks.
                    let Ok(busy) = this.update(cx, |f, cx| {
                        let hidden = f.task.upgrade().is_some_and(|t| !t.read(cx).visible);
                        if hidden && ticks % 8 != 0 {
                            return None;
                        }
                        f.show_problems(cx);
                        Some(f.lsp_client.as_ref().map(|c| (c.busy_for(&f.path), c.problem())))
                    }) else {
                        break;
                    };
                    let Some(busy) = busy else { continue };
                    if busy != last {
                        // Done with some work, indexing most of all: the server
                        // now knows names it could not tell a moment ago.
                        let learned = matches!((&last, &busy), (Some((Some(_), _)), Some((None, _))));
                        last = busy;
                        let told = this.update_in(cx, |f, window, cx| {
                            if learned {
                                f.editor.update(cx, |state, cx| {
                                    state.refresh_semantic_tokens(window, cx);
                                    state.refresh_inlay_hints(cx);
                                });
                            }
                            cx.notify()
                        });
                        if told.is_err() {
                            break;
                        }
                    }
                }
            }),
        }});
        // Debug aid for screenshots: IDEAE_LOOK=split or preview shows a file that can be drawn so.
        let asked = std::env::var("IDEAE_LOOK").ok().and_then(|look| super::preview::Look::named(&look));
        // A restored tab has its own, set by the session. Sent to a line, the text has to be in view.
        let remembered = (how != OpenAs::Restored).then(|| super::preview::remembered_look(&path)).flatten().map(|look| match look {
            super::preview::Look::Preview if line > 0 => super::preview::Look::Split,
            look => look,
        });
        if let Some(look) = asked.or(remembered) {
            file.update(cx, |f, cx| f.set_look(look, cx));
        }
        file.update(cx, |f, cx| f.load_http_envs(cx));
        // The tab label shows whether the file has unsaved edits.
        // The cursor moving a line is the file's own to draw, except for the
        // Structure panel, which marks where it is.
        cx.observe(&file, |this, file, cx| {
            let dirty = file.read(cx).dirty;
            // A file with a query under way is not replaced by the next one opened.
            let held = dirty || file.read(cx).sql_busy() || file.read(cx).http_busy() || file.read(cx).sql_holds(cx);
            let structure = this.panel_shown(RightPane::Structure);
            let Some(tab) = this.tabs.iter_mut().find(|t| matches!(&t.kind, TabKind::File(f) if *f == file)) else { return };
            let follows = structure && this.structure_file == Some(tab.id);
            let kept = held && std::mem::take(&mut tab.preview);
            if std::mem::replace(&mut tab.dirty, dirty) != dirty || follows || kept {
                cx.notify();
            }
        })
        .detach();
        // A click on the blame at the end of the cursor's line opens its commit.
        // Typed into, not reloaded from disk: a place for Last edit location.
        let edited = path.clone();
        cx.subscribe_in(&editor, window, move |this, editor, event: &InputEvent, window, cx| {
            if let InputEvent::InlayClick { id } = event
                && let Some(hash) = id.strip_prefix(super::annotate::LINE_BLAME_ID)
            {
                this.show_commit(hash, window, cx);
            }
            if let InputEvent::Change = event
                && editor.read(cx).focus_handle(cx).is_focused(window)
            {
                let Position { line, character } = editor.read(cx).cursor_position();
                this.record_edit(edited.clone(), line, character);
            }
        })
        .detach();
        let weak = file.downgrade();
        editor.update(cx, |state, _| {
            state.on_gutter(move |event, window, cx| {
                let _ = weak.update(cx, |f, cx| f.on_gutter(event, window, cx));
            });
            state.set_change_tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Show the change").build(window, cx));
        });
        self.give_bookmarks(&path, &editor, cx);
        file.update(cx, |f, cx| {
            f.load_blame(cx);
            f.read_scopes(Duration::ZERO, cx);
        });
        self.attach_lsp(&path, content, &editor, &file, cx);
        self.use_search_definitions(&path, &editor, false, cx);

        let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.place_tab(TabItem::new(title, TabKind::File(file)), how, window, cx);
    }

    /// A new file tab goes in the place of the pane's preview tab when it is
    /// one too, and the file tabs over the limit that were left longest
    /// close.
    pub(super) fn place_tab(&mut self, mut tab: TabItem, how: OpenAs, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.current;
        tab.pane = pane;
        tab.preview = how == OpenAs::Preview && crate::state::settings().preview_tabs;
        let id = tab.id;
        let replaced = tab.preview.then(|| self.tabs.iter().position(|t| t.preview && !t.pinned && t.pane == pane)).flatten();
        match replaced {
            Some(ix) => {
                self.tabs.insert(ix + 1, tab);
                self.activate(ix + 1, window, cx);
                self.remove_tab(ix, window, cx);
            }
            None => {
                self.tabs.push(tab);
                self.activate(self.tabs.len() - 1, window, cx);
            }
        }
        if how != OpenAs::Restored {
            self.close_over_limit(id, window, cx);
        }
    }

    fn close_over_limit(&mut self, opened: usize, window: &mut Window, cx: &mut Context<Self>) {
        let limit = crate::state::settings().tab_limit as usize;
        if limit == 0 {
            return;
        }
        loop {
            let files = self.tabs.iter().filter(|t| matches!(t.kind, TabKind::File(_) | TabKind::Binary(_)));
            if files.clone().count() <= limit {
                return;
            }
            let shown = [self.active[0], self.active[1], self.active[2], self.active[3], Some(opened)];
            let busy = |t: &TabItem| match &t.kind {
                TabKind::File(f) => f.read(cx).dirty || f.read(cx).saving.is_some() || f.read(cx).sql_busy() || f.read(cx).sql_holds(cx),
                _ => false,
            };
            let Some(oldest) = files
                .filter(|t| !t.pinned && !busy(t) && !shown.contains(&Some(t.id)))
                .min_by_key(|t| t.used)
                .map(|t| t.id)
            else {
                return;
            };
            let Some(ix) = self.ix_of(oldest) else { return };
            self.remember_closed(ix, cx);
            self.remove_tab(ix, window, cx);
        }
    }

    /// Gives an open file its language server: found and started off the
    /// UI thread (the project's directory looked for, a process started),
    /// put to use here once there. `content` is the file's text to open it
    /// with.
    pub(super) fn attach_lsp(
        &mut self,
        path: &PathBuf,
        content: String,
        editor: &Entity<EditorState>,
        file: &Entity<FileView>,
        cx: &mut Context<Self>,
    ) {
        // The newest asking wins: an answer to one before it is not applied.
        let asked = file.update(cx, |f, _| {
            f.lsp_unlicensed = false;
            f.lsp_attach += 1;
            f.lsp_attach
        });
        let (servers, root, at) = (self.lsp.clone(), self.root.clone(), path.clone());
        let (asleep, off) = (self.lsp_asleep, self.lsp_off);
        let found = cx.background_spawn(crate::procs::on_thread(move || {
            let missing = lsp::missing_server(&at);
            let found = match lsp::server_for(&at) {
                Err(reason) => Err(reason),
                Ok(_) if asleep => Err(ASLEEP.to_string()),
                Ok(_) if off => Err(LSP_OFF.to_string()),
                Ok(_) => servers.server_for(&root, &at),
            };
            (missing, found)
        }));
        let (path, editor, file) = (path.clone(), editor.clone(), file.clone());
        cx.spawn(async move |this, cx| {
            let (missing, found) = found.await;
            let _ = this.update(cx, |t, cx| {
                if file.read(cx).lsp_attach != asked {
                    return;
                }
                file.update(cx, |f, _| f.lsp_missing = missing);
                match found {
                    Err(reason) if reason == crate::lsp_share::STOPPED => {}
                    Err(reason) => file.update(cx, |f, cx| {
                        f.lsp_ok = Some(false);
                        f.lsp_status = reason.into();
                        cx.notify();
                    }),
                    // Let go of while it started (asleep, stopped): not this file's.
                    Ok(found) if !t.lsp.holds(&found.client) => {}
                    Ok(found) => t.use_lsp(&path, content, &editor, &file, found, cx),
                }
            });
        })
        .detach();
    }

    fn use_lsp(&mut self, path: &PathBuf, content: String, editor: &Entity<EditorState>, file: &Entity<FileView>, found: crate::lsp_share::Found, cx: &mut Context<Self>) {
        let crate::lsp_share::Found { client, language_id, program, project } = found;
        let shown_root = project.strip_prefix(&self.root).unwrap_or(&project).display().to_string();
        let shown_root = if shown_root.is_empty() { ".".to_string() } else { shown_root };
        file.update(cx, |f, cx| {
            f.lsp_ok = None;
            f.lsp_status = "starting…".into();
            f.lsp_client = Some(client.clone());
            f.lsp_program = if shown_root == "." { program.to_string() } else { format!("{program} @ {shown_root}") };
            cx.notify();
        });

        let open_client = client.clone();
        let open_path = path.clone();
        let weak_file = file.downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(crate::procs::on_thread(move || {
                    open_client.did_open(&open_path, language_id, &content).map(|()| lsp::without_licence(program, &open_client))
                }))
                .await;
            let (ok, unlicensed, status) = match result {
                Ok(unlicensed) => (true, unlicensed, "ready".to_string()),
                Err(error) => (false, false, format!("failed: {error:#}")),
            };
            let _ = weak_file.update(cx, |f, cx| {
                f.lsp_ok = Some(ok);
                f.lsp_unlicensed = unlicensed;
                f.lsp_status = status.into();
                cx.notify();
            });
        })
        .detach();

        let bridge = Rc::new(LspBridge { client, uri: lsp::path_to_uri(path), path: path.clone(), file: file.downgrade() });
        let task = cx.weak_entity();
        let this_path = path.clone();
        let completions = crate::predict::provider(Some(bridge.clone()), path, &self.root, editor.read(cx).text().clone(), cx);
        editor.update(cx, |state, cx| {
            let lsp = state.lsp_mut();
            lsp.hover_provider = Some(bridge.clone());
            lsp.completion_provider = Some(completions);
            lsp.code_action_providers = vec![bridge.clone()];
            lsp.semantic_tokens_provider = Some(bridge.clone());
            lsp.inlay_hints_provider = Some(bridge.clone());
            lsp.definition_provider = Some(UsagesAtDeclaration::wrap(bridge, &this_path));
            lsp.show_document = Some(show_document_handler(task, this_path));
            cx.notify();
        });
    }
}

pub(super) const LSP_OFF: &str = "language servers off for this project (Project settings)";

impl TaskView {
    /// Whether git ignores a file of this task: it is in the worktree and
    /// not among the files git lists. Not known before those are read.
    pub(super) fn ignores(&self, path: &std::path::Path) -> bool {
        let Ok(inside) = path.strip_prefix(&self.root) else { return false };
        let inside = inside.to_string_lossy().replace('\\', "/");
        // Before the files are known every one may be ignored: veiled, until the list says it is not.
        self.tree_files.is_empty() || !self.tree_files.iter().any(|file| *file == inside)
    }

    /// The file in the editor against a branch, tag or commit to choose.
    pub fn compare_open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(TabKind::File(file)) = self.focused_ix().map(|ix| &self.tabs[ix].kind) else { return };
        let Ok(file) = file.read(cx).path.strip_prefix(&self.root).map(|path| path.to_string_lossy().into_owned()) else { return };
        self.compare_with_dialog(Some(file), window, cx)
    }

    /// Marks the open files git ignores: while presenting they show a
    /// notice in place of what they hold, until asked for.
    pub fn veil_ignored(&mut self, cx: &mut Context<Self>) {
        self.hide_ignored_in_tree(cx);
        let files: Vec<Entity<FileView>> = self.tabs.iter().filter_map(|tab| match &tab.kind {
            TabKind::File(file) => Some(file.clone()),
            _ => None,
        }).collect();
        for file in files {
            let veiled = self.ignores(&file.read(cx).path);
            file.update(cx, |file, cx| {
                if file.veiled != veiled {
                    file.veiled = veiled;
                    cx.notify();
                }
            });
        }
    }
}

/// The most a file can be to open in the editor: a build's binaries are a
/// click away in the tree, and a file is read whole, on the UI thread.
const MAX_TEXT: u64 = 16 << 20;

/// A file's text, or why it is not opened as text.
fn read_text(path: &std::path::Path) -> Result<String, String> {
    if crate::hosts::is_remote(path) {
        let size = crate::hostfs::stat(path).map_err(|e| format!("Cannot read: {e}"))?.len;
        if size > MAX_TEXT {
            return Err(format!("{} file: too large to open here", human_size(size)));
        }
        return match crate::hostfs::read(path).map(String::from_utf8) {
            Ok(Ok(content)) => Ok(content),
            Ok(Err(_)) => Err(format!("Binary file, {}: not shown as text", human_size(size))),
            Err(e) => Err(format!("Cannot read: {e}")),
        };
    }
    let size = std::fs::metadata(path).map_err(|e| format!("Cannot read: {e}"))?.len();
    if size > MAX_TEXT {
        return Err(format!("{} file: too large to open here", human_size(size)));
    }
    match std::fs::read(path).map(String::from_utf8) {
        Ok(Ok(content)) => Ok(content),
        Ok(Err(_)) => Err(format!("Binary file, {}: not shown as text", human_size(size))),
        Err(e) => Err(format!("Cannot read: {e}")),
    }
}

#[cfg(test)]
mod read_tests {
    #[test]
    fn large_and_binary_files_are_not_opened_as_text() {
        let dir = std::env::temp_dir().join(format!("ideae-read-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (text, binary, large) = (dir.join("a.txt"), dir.join("b.bin"), dir.join("c.log"));
        std::fs::write(&text, "hi\n").unwrap();
        std::fs::write(&binary, [0xff, 0xfe, 0]).unwrap();
        std::fs::File::create(&large).unwrap().set_len(super::MAX_TEXT + 1).unwrap();
        assert_eq!(super::read_text(&text).as_deref(), Ok("hi\n"));
        assert!(super::read_text(&binary).unwrap_err().starts_with("Binary file"));
        assert!(super::read_text(&large).unwrap_err().contains("too large"));
        assert!(super::read_text(&dir.join("none")).unwrap_err().starts_with("Cannot read"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
