use super::*;

/// A row of the Changes panel. They are all as high as one another, so
/// that only those in view are laid out.
pub(super) enum ChangeRow {
    Head { staged: bool, title: String },
    Nothing,
    Dir { path: String, label: String, depth: usize, in_staged: bool, folded: bool },
    File { ix: usize, depth: usize, in_staged: bool },
    StashHead,
    Stash(usize),
}

/// A file's row with its buttons is 26.5 high.
pub(super) const CHANGE_ROW: f32 = 27.;

impl TaskView {
    pub(super) fn staged_paths(&self) -> Vec<String> {
        self.files.iter().filter(|f| self.state_of(&f.path).staged).map(|f| f.path.to_string()).collect()
    }

    pub(super) fn state_of(&self, path: &str) -> git::FileState {
        self.file_states.get(path).copied().unwrap_or(git::FileState { staged: false, unstaged: true, ..Default::default() })
    }

    pub(super) fn render_commit_box(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let staged = self.staged_paths().len();
        let b = &self.branch_info;
        let busy = self.busy.is_some();
        let (push_label, push_tip) = if !b.has_origin {
            ("Push".to_string(), "No remote named origin")
        } else if b.upstream.is_none() {
            ("Publish".to_string(), "Push the branch to origin and track it")
        } else if b.ahead > 0 {
            (format!("Push ↑{}", b.ahead), "Push the commits to the upstream")
        } else {
            ("Push".to_string(), "Nothing to push")
        };
        let can_push = b.has_origin && (b.upstream.is_none() || b.ahead > 0);
        // Status above the message box: the box is pinned to the bottom, so
        // nothing below it moves when a status shows up.
        v_flex()
            .flex_none()
            .gap_1p5()
            .p_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .children(self.conflict_marked.iter().map(|(path, line)| {
                let (open, line) = (self.root.join(path), line.saturating_sub(1));
                div()
                    .id(SharedString::from(format!("marked-{path}")))
                    .text_xs()
                    .text_color(gpui_kit::red())
                    .cursor_pointer()
                    .child(format!("⚠ conflict left in {path}:{}", line + 1))
                    .on_click(cx.listener(move |this, _, window, cx| this.open_file(open.clone(), line, window, cx)))
            }))
            .when_some(self.busy, |this, label| {
                this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{label}…")))
            })
            .when_some(self.action.clone().filter(|_| !busy), |this, (ok, message)| {
                this.child(
                    h_flex()
                        .gap_1()
                        .items_start()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(if ok { gpui_kit::green() } else { gpui_kit::red() })
                                .child(message.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().to_string()),
                        )
                        .when(self.discard_undo.is_some(), |row| {
                            row.child(
                                Button::new("undo-discard")
                                    .xsmall()
                                    .outline()
                                    .label("Undo")
                                    .on_click(cx.listener(|this, _, window, cx| this.undo_discard(window, cx))),
                            )
                        })
                        .child(
                            Button::new("dismiss-action")
                                .xsmall()
                                .ghost()
                                .label("×")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.action = None;
                                    this.discard_undo = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                h_flex()
                    .gap_1()
                    .items_start()
                    .child(div().flex_1().min_w_0().child(gpui_kit::component::input::Textarea::new(&self.commit_input).small()))
                    .child(
                        Button::new("suggest-message")
                            .small()
                            .ghost()
                            .label("Write")
                            .disabled(busy || self.files.is_empty())
                            .tooltip(if staged > 0 {
                                "Claude writes the message for the staged changes"
                            } else {
                                "Claude writes the message for all the changes"
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.suggest_message(window, cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("commit")
                            .small()
                            .outline()
                            .disabled(busy || self.files.is_empty())
                            .label(if staged > 0 { format!("Commit {staged}") } else { "Commit all".to_string() })
                            .on_click(cx.listener(|this, _, window, cx| this.commit(window, cx))),
                    )
                    .child(
                        Button::new("stash")
                            .small()
                            .ghost()
                            .disabled(busy || self.files.is_empty())
                            .label("Stash")
                            .tooltip("Put everything not committed away, to bring back later; the message box names it")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let message = this.commit_input.read(cx).value().trim().to_string();
                                this.run_action("Stash", move |root| git::stash(&root, &message), false, window, cx)
                            })),
                    )
                    .child(
                        Button::new("push")
                            .small()
                            .ghost()
                            .disabled(busy || !can_push)
                            .label(push_label)
                            .tooltip(push_tip)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let upstream = this.branch_info.upstream.is_some();
                                this.run_action("Push", move |root| git::push(&root, upstream), false, window, cx)
                            })),
                    )
                    .child(
                        Button::new("pr")
                            .small()
                            .ghost()
                            .disabled(busy || !b.has_origin)
                            .label("PR")
                            .tooltip(if b.has_origin {
                                "Open this branch's pull request, creating it if needed"
                            } else {
                                "No remote named origin"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                let upstream = this.branch_info.upstream.is_some();
                                this.run_action("PR", move |root| git::pull_request(&root, upstream), true, window, cx)
                            })),
                    ),
            )
    }

    pub(super) fn render_change_lists(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let staged: Vec<usize> = (0..self.files.len()).filter(|&i| self.state_of(&self.files[i].path).staged).collect();
        let unstaged: Vec<usize> = (0..self.files.len()).filter(|&i| self.state_of(&self.files[i].path).unstaged).collect();
        let section = |id: &'static str, title: String, all_label: &'static str, stage: bool, cx: &mut Context<Self>| {
            h_flex()
                .justify_between()
                .px_3()
                .pt_2()
                .pb_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(title)
                .child(
                    Button::new(id)
                        .xsmall()
                        .ghost()
                        .label(all_label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let paths: Vec<String> = this
                                .files
                                .iter()
                                .filter(|f| {
                                    let s = this.state_of(&f.path);
                                    if stage { s.unstaged } else { s.staged }
                                })
                                .map(|f| f.path.to_string())
                                .collect();
                            this.stage_paths(paths, stage, window, cx)
                        })),
                )
        };
        let rows = Rc::new(self.change_rows(&staged, &unstaged));
        let rows_len = rows.len();
        // Only the rows in view are laid out: an agent can leave hundreds of files changed.
        let item = cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
            range
                .map(|at| {
                    let row = match &rows[at] {
                        ChangeRow::Head { staged: true, title } => section("unstage-all", title.clone(), "unstage all", false, cx).into_any_element(),
                        ChangeRow::Head { staged: false, title } => section("stage-all", title.clone(), "stage all", true, cx).into_any_element(),
                        ChangeRow::Nothing => div().px_3().text_sm().text_color(cx.theme().muted_foreground).child("No uncommitted changes").into_any_element(),
                        ChangeRow::StashHead => div().px_3().text_xs().text_color(cx.theme().muted_foreground).child(format!("STASHES ({})", this.stashes.len())).into_any_element(),
                        ChangeRow::File { ix, depth, in_staged } if *ix < this.files.len() => this.render_change_row(*ix, *depth, *in_staged, cx),
                        ChangeRow::Dir { path, label, depth, in_staged, folded } => this.render_change_dir(path, label, *depth, *in_staged, *folded, cx),
                        ChangeRow::Stash(i) if *i < this.stashes.len() => this.render_stash_row(*i, cx),
                        _ => div().into_any_element(),
                    };
                    // As high as one another: the list places them by that.
                    div().h(px(CHANGE_ROW)).w_full().flex().flex_col().justify_end().overflow_hidden().child(row)
                })
                .collect::<Vec<_>>()
        });
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(uniform_list("changed-files-list", rows_len, item).size_full().track_scroll(&self.changes_scroll))
            .child(gpui_kit::component::scroll::Scrollbar::vertical(&self.changes_scroll))
    }

    /// The rows of the Changes panel, top to bottom.
    fn change_rows(&self, staged: &[usize], unstaged: &[usize]) -> Vec<ChangeRow> {
        let mut rows = Vec::new();
        if !staged.is_empty() {
            rows.push(ChangeRow::Head { staged: true, title: format!("STAGED ({})", staged.len()) });
            self.change_tree_rows(&mut rows, staged, true);
        }
        if self.files.is_empty() {
            rows.push(ChangeRow::Nothing);
        } else if !unstaged.is_empty() {
            let viewed = unstaged.iter().filter(|&&ix| self.is_viewed(ix)).count();
            let title = if viewed > 0 { format!("CHANGES ({}) · {viewed} viewed", unstaged.len()) } else { format!("CHANGES ({})", unstaged.len()) };
            rows.push(ChangeRow::Head { staged: false, title });
            self.change_tree_rows(&mut rows, unstaged, false);
        }
        if !self.stashes.is_empty() {
            rows.push(ChangeRow::StashHead);
            rows.extend((0..self.stashes.len()).map(ChangeRow::Stash));
        }
        rows
    }

    fn render_stash_row(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let stash = &self.stashes[i];
        let muted = cx.theme().muted_foreground;
        {
                let group = SharedString::from(format!("stash-{i}"));
                let act = |id: &'static str, label: &'static str, tip: &'static str, stash: git::Stash, cx: &mut Context<Self>| {
                    Button::new((id, i)).xsmall().ghost().label(label).tooltip(tip).on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        let stash = stash.clone();
                        match id {
                            "stash-apply" => this.run_action("Stash", move |root| git::stash_apply(&root, &stash, false), false, window, cx),
                            "stash-pop" => this.run_action("Stash", move |root| git::stash_apply(&root, &stash, true), false, window, cx),
                            _ => this.drop_stash_dialog(stash, window, cx),
                        }
                    }))
                };
                let name = stash.name.clone();
                (
                    h_flex()
                        .id(group.clone())
                        .group(group.clone())
                        .relative()
                        .px_3()
                        .py_0p5()
                        .gap_2()
                        .text_sm()
                        .cursor_pointer()
                        .hover(|s| s.bg(cx.theme().accent))
                        .child(div().flex_1().min_w_0().truncate().child(stash.what.clone()))
                        .child(div().flex_none().text_xs().text_color(muted).child(stash.when.clone()))
                        // Over the row's end while the pointer is on it: the name keeps its room.
                        .child(
                            h_flex()
                                .absolute()
                                .right_1()
                                .top_0()
                                .bottom_0()
                                .bg(cx.theme().accent)
                                .opacity(0.)
                                .group_hover(group, |s| s.opacity(1.))
                                .child(act("stash-apply", "apply", "Bring it back and keep it in the list", stash.clone(), cx))
                                .child(act("stash-pop", "pop", "Bring it back and take it off the list", stash.clone(), cx))
                                .child(act("stash-drop", "drop", "Throw it away", stash.clone(), cx)),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.show_stash(name.clone(), window, cx)))
                )
                .into_any_element()
        }
    }

    /// What a stash holds, as a diff in a dialog.
    pub(super) fn show_stash(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let what = self.stashes.iter().find(|s| s.name == name).map(|s| s.what.clone()).unwrap_or_default();
        cx.spawn_in(window, async move |_, cx| {
            let title = format!("{name} · {what}");
            let files = cx.background_spawn(async move { DiffFile::parse(&git::stash_patch(&root, &name)).map(with_languages).unwrap_or_default() }).await;
            let _ = cx.update(|window, cx| show_diff_dialog(title, files, window, cx));
        })
        .detach();
    }

    pub(super) fn drop_stash_dialog(&mut self, stash: git::Stash, window: &mut Window, cx: &mut Context<Self>) {
        let (name, what) = (stash.name.clone(), stash.what.clone());
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (this, stash) = (this.clone(), stash.clone());
            dialog
                .confirm()
                .title(format!("Drop {name}?"))
                .description(format!("{what}\n\nWhat it holds is gone for good."))
                .ok_text("Drop")
                .on_ok(move |_, window, cx| {
                    let stash = stash.clone();
                    let _ = this.update(cx, |this, cx| this.run_action("Stash", move |root| git::stash_drop(&root, &stash), false, window, cx));
                    true
                })
        });
    }

    /// The files as a folder tree; a folded folder hides what is under it.
    /// The files of a section as rows of a folder tree, what is under a folded folder left out.
    fn change_tree_rows(&self, rows: &mut Vec<ChangeRow>, files: &[usize], in_staged: bool) {
        let section = if in_staged { "staged" } else { "changes" };
        let paths: Vec<(usize, &str)> = files.iter().map(|&ix| (ix, self.files[ix].path.as_ref())).collect();
        let mut hidden_under: Option<String> = None;
        for node in change_tree(&paths) {
            let path = match &node {
                ChangeNode::Dir { path, .. } => path.as_str(),
                ChangeNode::File { ix, .. } => self.files[*ix].path.as_ref(),
            };
            if let Some(prefix) = &hidden_under {
                if path.starts_with(prefix.as_str()) {
                    continue;
                }
                hidden_under = None;
            }
            match node {
                ChangeNode::File { ix, depth } => rows.push(ChangeRow::File { ix, depth, in_staged }),
                ChangeNode::Dir { path, label, depth } => {
                    let folded = self.folded_changes.contains(&format!("{section}:{path}"));
                    if folded {
                        hidden_under = Some(format!("{path}/"));
                    }
                    rows.push(ChangeRow::Dir { path, label, depth, in_staged, folded });
                }
            }
        }
    }

    fn render_change_dir(&self, path: &str, label: &str, depth: usize, in_staged: bool, folded: bool, cx: &mut Context<Self>) -> AnyElement {
        let section = if in_staged { "staged" } else { "changes" };
        let (key, label) = (format!("{section}:{path}"), label.to_string());
        {
            {
                    let under = format!("{path}/");
                    let group = SharedString::from(format!("change-dir-{key}"));
                    (
                        h_flex()
                            .id(SharedString::from(format!("dir-{key}")))
                            .group(group.clone())
                            .pl(px(12. + 14. * depth as f32))
                            .pr_3()
                            .py_0p5()
                            .gap_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(div().w(px(12.)).text_xs().child(if folded { "▸" } else { "▾" }))
                            .child(div().flex_1().min_w_0().truncate().child(label))
                            .child(
                                div().opacity(0.).group_hover(group, |s| s.opacity(1.)).child(
                                    Button::new(SharedString::from(format!("dir-stage-{key}")))
                                        .xsmall()
                                        .ghost()
                                        .label(if in_staged { "−" } else { "+" })
                                        .tooltip(if in_staged { "Unstage the folder" } else { "Stage the folder" })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            // Looked up on the click, not for every folder on every frame.
                                            let dir_files = this
                                                .files
                                                .iter()
                                                .filter(|f| f.path.starts_with(under.as_str()) && {
                                                    let state = this.state_of(&f.path);
                                                    if in_staged { state.staged } else { state.unstaged }
                                                })
                                                .map(|f| f.path.to_string())
                                                .collect();
                                            this.stage_paths(dir_files, !in_staged, window, cx)
                                        })),
                                ),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.folded_changes.remove(&key) {
                                    this.folded_changes.insert(key.clone());
                                }
                                cx.notify();
                            }))
                    )
                    .into_any_element()
            }
        }
    }

    pub(super) fn is_viewed(&self, ix: usize) -> bool {
        let file = &self.files[ix];
        self.viewed.get(file.path.as_ref()) == Some(&file.hash)
    }

    pub(super) fn is_path_viewed(&self, path: &str) -> bool {
        self.files.iter().any(|f| f.path.as_ref() == path && self.viewed.get(path) == Some(&f.hash))
    }

    pub(super) fn toggle_viewed(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(file) = self.files.iter().find(|f| f.path.as_ref() == path) else { return };
        if self.viewed.get(path) == Some(&file.hash) {
            self.viewed.remove(path);
        } else {
            self.viewed.insert(path.to_string(), file.hash);
        }
        self.changes.update(cx, |_, cx| cx.notify());
        cx.emit(TaskEvent::Changed);
        cx.notify();
    }

    pub(super) fn render_change_row(&self, ix: usize, depth: usize, in_staged: bool, cx: &mut Context<Self>) -> AnyElement {
        let file = &self.files[ix];
        let path = file.path.clone();
        let stage_path = file.path.to_string();
        let open_path = self.root.join(file.path.as_ref());
        let open_line = file.first_line.saturating_sub(1);
        let deleted = matches!(file.status, DiffFileStatus::Deleted);
        // The tree shows the folders.
        let name = file.path.rsplit_once('/').map_or(file.path.to_string(), |(_, name)| name.to_string());
        let status = match file.status {
            DiffFileStatus::Added => "A",
            DiffFileStatus::Deleted => "D",
            DiffFileStatus::Renamed => "R",
            _ => "M",
        };
        // Colored as in the Files tree, and gray when gone.
        let mark = match (file.status, self.file_states.get(file.path.as_ref()).map(|state| state.change)) {
            (DiffFileStatus::Deleted, _) => Mark::Deleted,
            (_, Some(change)) => change.into(),
            (DiffFileStatus::Added, None) => Mark::Added,
            (DiffFileStatus::Conflicted, None) => Mark::Conflict,
            _ => Mark::Modified,
        };
        let id = if in_staged { ix * 2 + 1 } else { ix * 2 };
        let keyboard = self.side_has_focus.get() && self.right == RightPane::Changes && ix == self.side_selected;
        let viewed = self.is_viewed(ix);
        let view_path = file.path.to_string();
        let discard_path = file.path.to_string();
        let view = cx.weak_entity();
        h_flex()
            .id(("file", id))
            .test_support()
            // Always the room of the mark, so the row does not move when it shows.
            .border_l_2()
            .border_color(if keyboard { cx.theme().primary } else { gpui_kit::transparent_black() })
            .group("change-row")
            .pl(px(10. + 14. * depth as f32))
            .pr_3()
            .py_0p5()
            .gap_2()
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().accent))
            .child(div().w(px(12.)).text_color(cx.theme().muted_foreground).child(status))
            .child(div().flex_1().min_w_0().truncate().when(viewed, |d| d.text_color(cx.theme().muted_foreground)).child(marked_label(name.into(), Some(mark), viewed, cx)))
            .children(self.agent_mark(&file.path))
            .when(viewed, |row| row.child(div().text_xs().text_color(gpui_kit::green()).child("✓")))
            .child(div().text_xs().text_color(gpui_kit::green()).child(format!("+{}", file.additions)))
            .child(div().text_xs().text_color(gpui_kit::red()).child(format!("-{}", file.deletions)))
            // The actions show on the row under the pointer or the keyboard.
            .child(
                h_flex()
                    .gap_0p5()
                    .when(!keyboard, |actions| actions.invisible().group_hover("change-row", |s| s.visible()))
                    .child(
                        Button::new(("viewed", id))
                            .xsmall()
                            .ghost()
                            .label(if viewed { "✓" } else { "○" })
                            .tooltip(if viewed { "Viewed; click to unmark" } else { "Mark as viewed until it changes again" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_viewed(&view_path, cx)
                            })),
                    )
                    .child(
                        Button::new(("stage", id))
                            .xsmall()
                            .ghost()
                            .label(if in_staged { "−" } else { "+" })
                            .tooltip(if in_staged { "Unstage" } else { "Stage" })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.stage_paths(vec![stage_path.clone()], !in_staged, window, cx)
                            })),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.side_selected = ix;
                this.show_changed_file(path.clone(), window, cx)
            }))
            .context_menu(move |menu, _, _| {
                use gpui_kit::component::menu::PopupMenuItem;
                let (open_view, open_path) = (view.clone(), open_path.clone());
                let (discard_view, discard_path) = (view.clone(), discard_path.clone());
                menu.when(!deleted, |menu| {
                    menu.item(PopupMenuItem::new("Open").on_click(move |_, window, cx| {
                        let path = open_path.clone();
                        open_view.update(cx, |this, cx| this.open_file(path, open_line, window, cx)).ok();
                    }))
                })
                .when(!in_staged, |menu| {
                    menu.item(PopupMenuItem::new("Discard changes").on_click(move |_, window, cx| {
                        let path = discard_path.clone();
                        discard_view.update(cx, |this, cx| this.confirm_discard(path, window, cx)).ok();
                    }))
                })
            })
            .into_any_element()
    }

    /// Takes one change of a file back; what the file was is kept for Undo
    /// until the next git action, as when the whole file is discarded.
    pub(super) fn revert_hunk(&mut self, hunk: git::Hunk, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let kept: Arc<std::sync::Mutex<Option<git::FileSnapshot>>> = Arc::default();
        let slot = kept.clone();
        self.run_action(
            "Revert",
            move |root| {
                *slot.lock().unwrap() = Some(git::snapshot(&root, &hunk.path));
                git::apply_hunk(&root, &hunk.patch(), true, false).map(|_| format!("Took a change of {} back", hunk.path))
            },
            false,
            window,
            cx,
        );
        self.discard_undo = Some(kept);
    }

    pub(super) fn stage_hunk(&mut self, hunk: git::Hunk, stage: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.run_action(
            if stage { "Stage" } else { "Unstage" },
            move |root| {
                let done = if stage { "Staged" } else { "Unstaged" };
                git::stage_change(&root, &hunk, stage).map(|_| format!("{done} a change of {}", hunk.path))
            },
            false,
            window,
            cx,
        );
    }

    /// Discards at once; what was there is kept for Undo until the next
    /// git action.
    pub(super) fn confirm_discard(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let kept: Arc<std::sync::Mutex<Option<git::FileSnapshot>>> = Arc::default();
        let slot = kept.clone();
        self.run_action(
            "Discard",
            move |root| {
                *slot.lock().unwrap() = Some(git::snapshot(&root, &path));
                git::discard(&root, &path).map(|_| format!("Discarded {path}"))
            },
            false,
            window,
            cx,
        );
        self.discard_undo = Some(kept);
    }

    pub(super) fn undo_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.discard_undo.take().and_then(|kept| kept.lock().unwrap().take()) else { return };
        self.run_action("Undo", move |root| git::restore(&root, &snapshot), false, window, cx);
    }

    pub(super) fn stage_paths(&mut self, paths: Vec<String>, stage: bool, window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let label = if stage { "Stage" } else { "Unstage" };
        self.run_action(
            label,
            move |root| if stage { git::stage(&root, &paths) } else { git::unstage(&root, &paths) },
            false,
            window,
            cx,
        );
    }

    /// Write: Claude writes the message for what would be committed.
    pub(super) fn suggest_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.save_all_now(cx);
        self.busy = Some("Writing the message");
        self.action = None;
        cx.notify();
        let root = self.root.clone();
        let staged_only = !self.staged_paths().is_empty();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(crate::procs::on_thread(move || { git::suggest_message(&root, staged_only) })).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(message) => this.commit_input.update(cx, |i, cx| i.set_value(message, window, cx)),
                    Err(e) => this.action = Some((false, format!("Message: {e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.commit_input.read(cx).value().trim().to_string();
        if message.is_empty() {
            self.action = Some((false, "Write a commit message first".into()));
            cx.notify();
            return;
        }
        if !self.conflict_marked.is_empty() && !std::mem::replace(&mut self.markers_accepted, true) {
            self.action = Some((false, "Conflict markers are left in the files above. Commit again to commit them as they are.".into()));
            cx.notify();
            return;
        }
        self.run_action("Commit", move |root| git::commit(&root, &message), false, window, cx);
    }

    /// Runs a git/gh job off the UI thread, shows its outcome under the
    /// buttons, and refreshes. A successful commit clears the message.
    /// What History does with a commit: 0 takes it into this branch, 1
    /// commits its opposite, 2 starts a task from it. A conflict changes
    /// nothing and goes to the agent as a request.
    pub fn commit_action(&mut self, what: u8, hash: String, window: &mut Window, cx: &mut Context<Self>) {
        let short = hash[..hash.len().min(8)].to_string();
        match what {
            2 => {
                let copy_env = crate::state::settings().copy_env;
                self.run_action("Open", move |root| git::commit_worktree(&root, &hash, copy_env).map(|dir| format!("Opened in {}", dir.display())), false, window, cx)
            }
            revert => {
                let revert = revert == 1;
                let (label, asked) = if revert { ("Revert", format!("Revert commit {short} (`git revert --no-edit {short}`).")) } else { ("Cherry-pick", format!("Cherry-pick commit {short} into this branch (`git cherry-pick {short}`).")) };
                self.run_or_ask_agent(label, move |root| git::replay_commit(&root, &hash, revert), asked, window, cx)
            }
        }
    }

    /// Runs a git job that can meet a conflict. On one, what it did is
    /// undone and the agent's prompt gets the request to do it and resolve
    /// the conflicts: `asked` says what, the files that conflict are added.
    pub(super) fn run_or_ask_agent(
        &mut self,
        label: &'static str,
        job: impl FnOnce(PathBuf) -> Result<String, git::RebaseError> + Send + 'static,
        asked: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        // On disk before git reads the files: a save still on its way would
        // be left out of a commit, or come back after a discard.
        self.save_all_now(cx);
        self.busy = Some(label);
        self.action = None;
        cx.notify();
        let root = self.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(crate::procs::on_thread(move || job(root))).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                this.action = Some(match result {
                    Ok(out) => (true, out),
                    Err(git::RebaseError::Failed(e)) => (false, format!("{label}: {e}")),
                    Err(git::RebaseError::Conflicts(files)) => {
                        let list = files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
                        let told = this.ask_about_conflict(
                            format!("{asked} It conflicts in:\n{list}\n\nResolve the conflicts keeping what both sides meant, check that it builds and the tests pass, then finish it."),
                            window,
                            cx,
                        );
                        (false, format!("{label}: {} conflicting file(s), nothing changed{told}", files.len()))
                    }
                });
                let _ = this.refresh_tx.try_send(());
                cx.notify();
            });
        })
        .detach();
    }

    /// Hands a conflict to the agent as a request in its prompt, when that
    /// is wanted (Settings). What to add to what the user is told.
    pub(super) fn ask_about_conflict(&mut self, request: String, window: &mut Window, cx: &mut Context<Self>) -> &'static str {
        if !crate::state::with_settings(|s| s.conflicts_to_agent) {
            return "";
        }
        self.paste_to_agent(request, window, cx);
        "; the agent's prompt has a request"
    }

    /// Rebase onto the base; conflicts go to the agent as a request.
    pub(super) fn rebase_onto_base(&mut self, base: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.save_all_now(cx);
        self.busy = Some("Rebase");
        self.action = None;
        cx.notify();
        let root = self.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let job_base = base.clone();
            let result = cx.background_spawn(crate::procs::on_thread(move || { git::rebase_onto(&root, &job_base) })).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                this.action = Some(match result {
                    Ok(out) => (true, out),
                    Err(git::RebaseError::Failed(e)) => (false, format!("Rebase: {e}")),
                    Err(git::RebaseError::Conflicts(files)) => {
                        let list = files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
                        let told = this.ask_about_conflict(
                            format!(
                                "Rebase this branch onto {base} (`git fetch` first). It conflicts in:\n{list}\n\nResolve the conflicts keeping what both sides meant, check that it builds and the tests pass, then finish the rebase."
                            ),
                            window,
                            cx,
                        );
                        (false, format!("Rebase: {} conflicting file(s), nothing changed{told}", files.len()))
                    }
                });
                let _ = this.refresh_tx.try_send(());
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn run_action(
        &mut self,
        label: &'static str,
        job: impl FnOnce(PathBuf) -> Result<String, String> + Send + 'static,
        open_url: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        // On disk before git reads the files: a save still on its way would
        // be left out of a commit, or come back after a discard.
        self.save_all_now(cx);
        self.busy = Some(label);
        self.action = None;
        self.discard_undo = None;
        cx.notify();
        let root = self.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(crate::procs::on_thread(move || { job(root) })).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(out) => {
                        if open_url {
                            cx.open_url(&out);
                        }
                        if label == "Commit" {
                            this.commit_input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        let summary = out.lines().next().unwrap_or_default().to_string();
                        this.action = Some((true, if summary.is_empty() { format!("{label}: done") } else { summary }));
                    }
                    Err(error) => this.action = Some((false, format!("{label}: {error}"))),
                }
                let _ = this.refresh_tx.try_send(());
                cx.notify();
            });
        })
        .detach();
    }
}

/// A terminal title for a tab: shells report their working directory, of
/// which the last component says enough.
pub(super) fn short_title(title: &str) -> String {
    let title = title.trim();
    let tail = if title.contains('/') {
        title.trim_end_matches('/').rsplit('/').next().unwrap_or(title)
    } else {
        title
    };
    truncate(tail, 24)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}
