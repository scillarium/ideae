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
        self.record_nav(cx);
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

        let content = match read_text(&path) {
            Ok(content) => content,
            Err(reason) => {
                let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let view = cx.new(|_| BinaryView { path: path.clone(), reason });
                self.place_tab(TabItem::new(title, TabKind::Binary(view)), how, window, cx);
                return;
            }
        };
        let language = lsp::language_for(&path);
        let veiled = self.ignores(&path);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .indent_guides(true)
                .soft_wrap(false)
        });
        editor.update(cx, |state, cx| {
            state.set_value(content.clone(), window, cx);
            state.jump_to_position(Position::new(line, col), JUMP_AT, window, cx);
        });

        let saved = super::file::Print::of(&content);
        let root = self.root.clone();
        let task = cx.weak_entity();
        let file = cx.new(|cx| {
            // The editor notifies on cursor moves too: follow the line for blame.
            cx.observe(&editor, |this: &mut FileView, editor, cx| {
                let Position { line, character } = editor.read(cx).cursor_position();
                this.cursor_col = character;
                if line != this.cursor_line {
                    this.cursor_line = line;
                    cx.notify();
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
            saved,
            disk_mtime: mtime(&path),
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
            preview: false,
            veiled,
            occurrence: None,
            committed: None,
            _marks: Task::ready(()),
            problems_seen: 0,
            signature: None,
            _signature: Task::ready(()),
            _scopes: Task::ready(()),
            // Re-render while the server reports progress, so the header can
            // say "indexing" instead of a misleading "ready".
            _poll: cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
                let mut last = None;
                loop {
                    cx.background_executor().timer(Duration::from_millis(500)).await;
                    let Ok(busy) = this.update(cx, |f, cx| {
                        f.show_problems(cx);
                        f.lsp_client.as_ref().map(|c| (c.busy(), c.problem()))
                    }) else {
                        break;
                    };
                    if busy != last {
                        // Done with some work, indexing most of all: the server
                        // now knows names it could not tell a moment ago.
                        let learned = matches!((&last, &busy), (Some((Some(_), _)), Some((None, _))));
                        last = busy;
                        let told = this.update_in(cx, |f, window, cx| {
                            if learned {
                                f.editor.update(cx, |state, cx| state.refresh_semantic_tokens(window, cx));
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
        // The tab label shows whether the file has unsaved edits.
        // The cursor moving a line is the file's own to draw, except for the
        // Structure panel, which marks where it is.
        cx.observe(&file, |this, file, cx| {
            let dirty = file.read(cx).dirty;
            let Some(tab) = this.tabs.iter_mut().find(|t| matches!(&t.kind, TabKind::File(f) if *f == file)) else { return };
            let follows = this.right == RightPane::Structure && this.structure_file == Some(tab.id);
            let kept = dirty && std::mem::take(&mut tab.preview);
            if std::mem::replace(&mut tab.dirty, dirty) != dirty || follows || kept {
                cx.notify();
            }
        })
        .detach();
        let weak = file.downgrade();
        editor.update(cx, |state, _| {
            state.on_gutter(move |event, window, cx| {
                let _ = weak.update(cx, |f, cx| f.on_gutter(event, window, cx));
            })
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
    fn place_tab(&mut self, mut tab: TabItem, how: OpenAs, window: &mut Window, cx: &mut Context<Self>) {
        let right = self.focus_right && self.is_split();
        tab.right = right;
        tab.preview = how == OpenAs::Preview && crate::state::settings().preview_tabs;
        let id = tab.id;
        let replaced = tab.preview.then(|| self.tabs.iter().position(|t| t.preview && !t.pinned && t.right == right)).flatten();
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
            let shown = [Some(self.left_active), self.right_active, Some(opened)];
            let busy = |t: &TabItem| match &t.kind {
                TabKind::File(f) => f.read(cx).dirty || f.read(cx).saving.is_some(),
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

    pub(super) fn attach_lsp(
        &mut self,
        path: &PathBuf,
        content: String,
        editor: &Entity<EditorState>,
        file: &Entity<FileView>,
        cx: &mut Context<Self>,
    ) {
        let set_status = |file: &Entity<FileView>, ok: Option<bool>, status: String, cx: &mut App| {
            file.update(cx, |f, cx| {
                f.lsp_ok = ok;
                f.lsp_status = status.into();
                cx.notify();
            });
        };
        let missing = lsp::missing_server(path);
        file.update(cx, |f, _| f.lsp_missing = missing);
        if self.lsp_asleep && lsp::server_for(path).is_ok() {
            return set_status(file, Some(false), ASLEEP.to_string(), cx);
        }
        let lsp::Server { language_id, program, command, args } = match lsp::server_for(path) {
            Ok(server) => server,
            Err(reason) => return set_status(file, Some(false), reason, cx),
        };
        let project = if path.starts_with(&self.root) {
            lsp::project_root(program, path, &self.root)
        } else {
            // A library file (std, the registry, a jump target): the task's own
            // server for the language reads it; one more would index it all again.
            self.lsp
                .keys()
                .find(|(p, _)| *p == program)
                .map(|(_, root)| root.clone())
                .unwrap_or_else(|| lsp::project_root(program, &self.root.join("_"), &self.root))
        };
        let key = (program, project.clone());
        let client = match self.lsp.get(&key) {
            Some(client) => client.clone(),
            None => match lsp::LspClient::spawn(program, &command, args, &project, crate::lsp_install::init_options(program, &command)) {
                Ok(client) => {
                    self.lsp.insert(key, client.clone());
                    client
                }
                Err(error) => {
                    // Not installed is the normal case for many languages, not an error.
                    let missing = error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound);
                    let status = if missing { format!("{program} not installed") } else { format!("{program} failed: {error:#}") };
                    return set_status(file, Some(false), status, cx);
                }
            },
        };
        set_status(file, None, "starting…".to_string(), cx);
        let shown_root = project.strip_prefix(&self.root).unwrap_or(&project).display().to_string();
        let shown_root = if shown_root.is_empty() { ".".to_string() } else { shown_root };
        file.update(cx, |f, _| {
            f.lsp_client = Some(client.clone());
            f.lsp_program = if shown_root == "." { program.to_string() } else { format!("{program} @ {shown_root}") };
        });

        let open_client = client.clone();
        let open_path = path.clone();
        let weak_file = file.downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(crate::procs::on_thread(move || { open_client.did_open(&open_path, language_id, &content) }))
                .await;
            let (ok, status) = match result {
                Ok(()) => (true, "ready · hover, Ctrl+click = definition".to_string()),
                Err(error) => (false, format!("failed: {error:#}")),
            };
            let _ = weak_file.update(cx, |f, cx| {
                f.lsp_ok = Some(ok);
                f.lsp_status = status.into();
                cx.notify();
            });
        })
        .detach();

        let bridge = Rc::new(LspBridge { client, uri: lsp::path_to_uri(path), path: path.clone(), file: file.downgrade() });
        let task = cx.weak_entity();
        let this_path = path.clone();
        editor.update(cx, |state, cx| {
            let lsp = state.lsp_mut();
            lsp.hover_provider = Some(bridge.clone());
            lsp.completion_provider = Some(bridge.clone());
            lsp.code_action_providers = vec![bridge.clone()];
            lsp.semantic_tokens_provider = Some(bridge.clone());
            lsp.definition_provider = Some(UsagesAtDeclaration::wrap(bridge, &this_path));
            lsp.show_document = Some(show_document_handler(task, this_path));
            cx.notify();
        });
    }
}

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
