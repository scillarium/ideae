use super::*;

#[derive(Clone, Copy, PartialEq)]
pub enum GitCommand {
    Commit,
    WriteMessage,
    Stash,
    Push,
    PullRequest,
    Fetch,
    Pull,
    Rebase,
    /// The stash put away last comes back, and is dropped.
    Unstash,
}

impl GitCommand {
    pub const ALL: [GitCommand; 9] = [Self::Commit, Self::WriteMessage, Self::Stash, Self::Push, Self::PullRequest, Self::Fetch, Self::Pull, Self::Rebase, Self::Unstash];
}

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

/// The ways to ignore `path`: in `.gitignore`, which the team shares, or
/// only in this clone (`info/exclude`).
pub(super) fn ignore_submenu(view: WeakEntity<TaskView>, path: String, folder: bool) -> impl Fn(gpui_kit::component::menu::PopupMenu, &mut Window, &mut Context<gpui_kit::component::menu::PopupMenu>) -> gpui_kit::component::menu::PopupMenu + 'static {
    use gpui_kit::component::menu::PopupMenuItem;
    move |mut menu, _, _| {
        let patterns = git::ignore_patterns(&path, folder);
        let places = [
            (git::IgnoreIn::Shared, "In .gitignore, for everyone"),
            (git::IgnoreIn::Local, "Just for me, in this repository"),
            (git::IgnoreIn::Everywhere, "In all my repositories"),
        ];
        for (ix, (place, heading)) in places.into_iter().enumerate() {
            if ix > 0 {
                menu = menu.separator();
            }
            menu = menu.label(heading);
            // A path of one repository means nothing in the others.
            for (label, pattern, _) in patterns.iter().filter(|(_, _, this_only)| !(*this_only && place == git::IgnoreIn::Everywhere)) {
                let (view, pattern) = (view.clone(), pattern.clone());
                menu = menu.item(PopupMenuItem::new(label.clone()).on_click(move |_, window, cx| {
                    let pattern = pattern.clone();
                    let _ = view.update(cx, |t, cx| t.run_action("Ignore", move |root| git::ignore(&root, &pattern, place), false, window, cx));
                }));
            }
        }
        menu
    }
}

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
        let amend = self.amend.is_some();
        let (push_label, push_tip) = if !b.has_origin {
            ("Push".to_string(), "No remote named origin")
        } else if b.upstream.is_none() {
            ("Publish".to_string(), "Push the branch to origin and track it")
        } else if b.ahead > 0 {
            (format!("Push ↑{}", b.ahead), "Push the commits to the upstream")
        } else {
            ("Push".to_string(), "Nothing to push")
        };
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
                        // All of it: what git says first is seldom why.
                        .child(div().flex_1().min_w_0().child(crate::copying::message("git-action-message", message.trim().to_string(), if ok { gpui_kit::green() } else { gpui_kit::red() }, cx)))
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
                    // Under Write, where the buttons below have no room for it.
                    .child(v_flex().flex_none().items_start().gap_1().child(
                        Button::new("suggest-message")
                            .small()
                            .ghost()
                            .label("✦ Write")
                            .disabled(!self.git_can(GitCommand::WriteMessage))
                            .tooltip(if staged > 0 {
                                "Have the agent for quick questions write the commit message for the staged changes, into the box"
                            } else {
                                "Have the agent for quick questions write the commit message for all the changes, into the box"
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.suggest_message(window, cx))),
                    ).when(self.can_amend(), |column| {
                        let view = cx.weak_entity();
                        column.child(
                            gpui_kit::component::checkbox::Checkbox::new("amend")
                                .small()
                                .label("Amend")
                                .checked(amend)
                                .disabled(busy)
                                .tooltip("Fold this commit into the last one, which is not pushed yet")
                                .on_click(move |_, window, cx| {
                                    let _ = view.update(cx, |this, cx| this.toggle_amend(window, cx));
                                }),
                        )
                    })),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("commit")
                            .small()
                            .outline()
                            .disabled(!self.git_can(GitCommand::Commit))
                            .label(match (amend, staged) {
                                (true, _) => "Amend".to_string(),
                                (false, 0) => "Commit all".to_string(),
                                (false, staged) => format!("Commit {staged}"),
                            })
                            .when(amend, |button| {
                                button.tooltip(if staged > 0 {
                                    "Replace the last commit: its changes, the staged ones and the message in the box"
                                } else {
                                    "Replace the last commit: its changes, all the changes here and the message in the box"
                                })
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.commit(window, cx))),
                    )
                    .child(
                        Button::new("stash")
                            .small()
                            .ghost()
                            .disabled(!self.git_can(GitCommand::Stash))
                            .label("Stash")
                            .tooltip("Put everything not committed away, to bring back later; the message box names it")
                            .on_click(cx.listener(|this, _, window, cx| this.git_command(GitCommand::Stash, window, cx))),
                    )
                    .child(
                        Button::new("push")
                            .small()
                            .ghost()
                            .disabled(!self.git_can(GitCommand::Push))
                            .label(push_label)
                            .tooltip(push_tip)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.git_command(GitCommand::Push, window, cx);
                                crate::learn::used("push", window, cx);
                            })),
                    )
                    .child(
                        Button::new("pr")
                            .small()
                            .ghost()
                            .disabled(!self.git_can(GitCommand::PullRequest))
                            .label("PR")
                            .tooltip(if b.has_origin {
                                "Open this branch's pull request, creating it if needed"
                            } else {
                                "No remote named origin"
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.git_command(GitCommand::PullRequest, window, cx))),
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
                        .on_click(cx.listener(move |this, _, window, cx| this.stage_all(stage, window, cx))),
                )
        };
        let rows = Rc::new(self.change_rows(&staged, &unstaged));
        let rows_len = rows.len();
        // Only the rows in view are laid out: an agent can leave hundreds of files changed.
        let item = cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
            range
                .map(|at| {
                    let row = match &rows[at] {
                        ChangeRow::Head { staged: true, title } => section("unstage-all", title.clone(), "Unstage all", false, cx).into_any_element(),
                        ChangeRow::Head { staged: false, title } => section("stage-all", title.clone(), "Stage all", true, cx).into_any_element(),
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

    /// Stages every change, or takes every one out of the index.
    pub(super) fn stage_all(&mut self, stage: bool, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<String> = self
            .files
            .iter()
            .filter(|f| {
                let state = self.state_of(&f.path);
                if stage { state.unstaged } else { state.staged }
            })
            .map(|f| f.path.to_string())
            .collect();
        self.stage_paths(paths, stage, window, cx)
    }

    /// The rows of the Changes panel, top to bottom.
    fn change_rows(&self, staged: &[usize], unstaged: &[usize]) -> Vec<ChangeRow> {
        let mut rows = Vec::new();
        // The two are told apart when there are both: one alone is what the panel's title says.
        let both = !staged.is_empty() && !unstaged.is_empty();
        if !staged.is_empty() {
            if both {
                rows.push(ChangeRow::Head { staged: true, title: format!("STAGED ({})", staged.len()) });
            }
            self.change_tree_rows(&mut rows, staged, true);
        }
        if self.files.is_empty() {
            rows.push(ChangeRow::Nothing);
        } else if !unstaged.is_empty() {
            if both {
                rows.push(ChangeRow::Head { staged: false, title: format!("NOT STAGED ({})", unstaged.len()) });
            }
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
                let what = stash.what.to_string();
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
                                .child(act("stash-apply", "Apply", "Bring it back and keep it in the list", stash.clone(), cx))
                                .child(act("stash-pop", "Pop", "Bring it back and take it off the list", stash.clone(), cx))
                                .child(act("stash-drop", "Drop", "Throw it away", stash.clone(), cx)),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.show_stash(name.clone(), window, cx)))
                        .context_menu(move |menu, _, _| menu.item(crate::copying::item("Copy message", what.clone())))
                )
                .into_any_element()
        }
    }

    /// What a stash holds, as a diff in a tab.
    pub(super) fn show_stash(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let what = self.stashes.iter().find(|s| s.name == name).map(|s| s.what.clone()).unwrap_or_default();
        let (label, title) = (name.clone(), format!("{name} · {what}"));
        let source: DiffSource = std::sync::Arc::new(move || (title.clone(), git::stash_patch(&root, &name)));
        self.show_diff_tab(label, source, window, cx);
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
        for node in changes_shown(&paths) {
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

    /// A file marked viewed folds to its header, once: unfolded by hand it
    /// stays so. Changed after (and so no longer viewed), it unfolds again.
    pub(super) fn fold_viewed(&mut self, diff: &Entity<DiffState>, cx: &mut Context<Self>) {
        let paths: Vec<SharedString> = diff.read(cx).files().iter().map(|file| file.path().clone()).collect();
        let mut fold = Vec::new();
        for path in paths {
            let viewed = self.files.iter().find(|f| f.path == path).map(|f| f.hash).filter(|hash| self.viewed.get(path.as_ref()) == Some(hash));
            let done = self.folded_viewed.get(path.as_ref()).copied();
            match (viewed, done) {
                (Some(hash), Some(Some(folded))) if hash == folded => {}
                (Some(hash), _) => {
                    fold.push((path.clone(), true));
                    self.folded_viewed.insert(path.to_string(), Some(hash));
                }
                (None, Some(Some(_))) => {
                    fold.push((path.clone(), false));
                    self.folded_viewed.insert(path.to_string(), None);
                }
                _ => {}
            }
        }
        if !fold.is_empty() {
            diff.update(cx, |state, cx| {
                for (path, folded) in fold {
                    state.set_file_collapsed(&path, folded, cx);
                }
            });
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
        let diff = self.changes.read(cx).diff.clone();
        self.fold_viewed(&diff, cx);
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
        let untracked = self.file_states.get(file.path.as_ref()).is_some_and(|state| state.change == git::Change::Untracked);
        let (status, status_tip) = match file.status {
            _ if untracked => ("U", "Untracked (not added)"),
            DiffFileStatus::Added => ("A", "Added"),
            DiffFileStatus::Deleted => ("D", "Deleted"),
            DiffFileStatus::Renamed => ("R", "Renamed"),
            _ => ("M", "Modified"),
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
        let keyboard = self.side_has_focus.get() && self.keyed == RightPane::Changes && ix == self.side_selected;
        let viewed = self.is_viewed(ix);
        let menu_path = file.path.to_string();
        let view = cx.weak_entity();
        h_flex()
            .id(("file", id))
            .test_support()
            // Always the room of the mark, so the row does not move when it shows.
            .border_l_2()
            .border_color(if keyboard { cx.theme().primary } else { gpui_kit::transparent_black() })
            .group("change-row")
            .relative()
            .pl(px(10. + 14. * depth as f32))
            .pr_3()
            .py_0p5()
            .gap_2()
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().accent))
            .child(
                div()
                    .id(("file-status", id))
                    .w(px(12.))
                    .text_color(cx.theme().muted_foreground)
                    .child(status)
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(status_tip).build(window, cx)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1p5()
                    .child(div().min_w_0().flex_shrink_0().max_w_full().truncate().when(viewed, |d| d.text_color(cx.theme().muted_foreground)).child(marked_label(name.into(), Some(mark), viewed, cx)))
                    // As a list, a file has its folder after its name: there is no tree to tell it.
                    .when_some(folder_shown(&file.path), |row, folder| row.child(div().flex_1().min_w_0().truncate().text_xs().text_color(cx.theme().muted_foreground).child(folder))),
            )
            .children(self.agent_mark(&file.path))
            .when(viewed, |row| row.child(div().text_xs().text_color(gpui_kit::green()).child("✓")))
            .child(div().text_xs().text_color(gpui_kit::green()).child(format!("+{}", file.additions)))
            .child(div().text_xs().text_color(gpui_kit::red()).child(format!("-{}", file.deletions)))
            // Stage, over the row's end while the pointer or the keyboard is on it: it keeps no room of its own.
            // Viewed is marked in the diff, and from the row's menu.
            .child(
                h_flex()
                    .absolute()
                    .right_0()
                    .top_0()
                    .bottom_0()
                    .pl_2()
                    .pr_1()
                    .bg(cx.theme().accent)
                    .when(!keyboard, |actions| actions.invisible().group_hover("change-row", |s| s.visible()))
                    .child(
                        // A sign to hit, not a letter of the row's size.
                        div()
                            .id(("stage", id))
                            .w(px(26.))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .text_size(px(18.))
                            .line_height(px(18.))
                            .font_semibold()
                            .cursor_pointer()
                            .hover(|sign| sign.bg(cx.theme().secondary))
                            .child(if in_staged { "−" } else { "+" })
                            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(if in_staged { "Unstage the file" } else { "Stage the file" }).build(window, cx))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.stage_paths(vec![stage_path.clone()], !in_staged, window, cx)
                            })),
                    ),
            )
            .on_drag(DraggedFiles::new(vec![open_path.clone()], !deleted, self.root.clone()), |dragged, _, _, cx| {
                start_drag(Drag::Files);
                cx.new(|_| dragged.clone())
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.side_selected = ix;
                this.keyed = RightPane::Changes;
                this.show_changed_file(path.clone(), window, cx);
            }))
            .context_menu(move |menu, window, cx| {
                use gpui_kit::component::menu::PopupMenuItem;
                let icon = crate::sidebar::menu_icon;
                let (open_view, open_path) = (view.clone(), open_path.clone());
                let (stage_view, stage_path) = (view.clone(), menu_path.clone());
                let (viewed_view, viewed_path) = (view.clone(), menu_path.clone());
                let (discard_view, discard_path) = (view.clone(), menu_path.clone());
                let (absolute, relative) = (open_path.to_string_lossy().to_string(), menu_path.clone());
                let (system_path, reveal_path, shown_path, shown_view) = (open_path.clone(), open_path.clone(), open_path.clone(), view.clone());
                let (terminal_view, here) = (view.clone(), open_path.parent().map(PathBuf::from).unwrap_or_default());
                let (compare_view, compare_path) = (view.clone(), menu_path.clone());
                menu.item(PopupMenuItem::new(if in_staged { "Unstage" } else { "Stage" }).icon(icon(if in_staged { "minus" } else { "plus" })).on_click(move |_, window, cx| {
                    let path = stage_path.clone();
                    stage_view.update(cx, |this, cx| this.stage_paths(vec![path], !in_staged, window, cx)).ok();
                }))
                .item(PopupMenuItem::new(if viewed { "Mark not viewed" } else { "Mark viewed" }).icon(icon("eye")).on_click(move |_, _, cx| {
                    viewed_view.update(cx, |this, cx| this.toggle_viewed(&viewed_path, cx)).ok();
                }))
                .when(!deleted, |menu| {
                    menu.item(PopupMenuItem::new("Open").on_click(move |_, window, cx| {
                        let path = open_path.clone();
                        open_view.update(cx, |this, cx| this.open_file(path, open_line, window, cx)).ok();
                    }))
                    .item(PopupMenuItem::new("Open in the system app").icon(icon("external-link")).on_click(move |_, _, cx| cx.open_with_system(&system_path)))
                })
                .separator()
                .item(crate::copying::item("Copy path", absolute).icon(icon("copy")))
                .item(crate::copying::item("Copy relative path", relative))
                .when(!deleted, |menu| {
                    menu.item(PopupMenuItem::new("Show in Files").icon(icon("locate-fixed")).on_click(move |_, _, cx| {
                        let _ = shown_view.update(cx, |task, cx| task.show_in_files(&shown_path, cx));
                    }))
                    .item(PopupMenuItem::new("Reveal in file manager").icon(icon("folder-open")).on_click(move |_, _, cx| super::file_tree::reveal(reveal_path.clone(), false, cx)))
                    .item(PopupMenuItem::new("Open terminal here").icon(icon("square-terminal")).on_click(move |_, window, cx| {
                        let here = here.clone();
                        let _ = terminal_view.update(cx, |task, cx| task.new_terminal_in(here, window, cx));
                    }))
                })
                .when(!deleted || untracked, |menu| menu.separator())
                .when(!deleted, |menu| {
                    menu.item(PopupMenuItem::new("Compare with…").icon(icon("git-compare")).on_click(move |_, window, cx| {
                        let path = compare_path.clone();
                        let _ = compare_view.update(cx, |task, cx| task.compare_with_dialog(Some(path), window, cx));
                    }))
                })
                .when(untracked, |menu| menu.submenu("Add to .gitignore", window, cx, ignore_submenu(view.clone(), menu_path.clone(), false)))
                .when(!in_staged, |menu| {
                    menu.separator().item(PopupMenuItem::new("Discard changes").icon(icon("trash")).on_click(move |_, window, cx| {
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

    /// The last commit is here alone: amending it rewrites nothing anyone has.
    pub(super) fn can_amend(&self) -> bool {
        !self.branch_info.head_pushed && !self.branch_info.last_hash.is_empty()
    }

    /// Amend on puts the last commit's message in an empty box; off, takes
    /// it out again if it is still as put.
    pub(super) fn toggle_amend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(filled) = self.amend.take() {
            if !filled.is_empty() && self.commit_input.read(cx).value().trim() == filled.trim() {
                self.commit_input.update(cx, |input, cx| input.set_value("", window, cx));
            }
            return cx.notify();
        }
        self.amend = Some(String::new());
        cx.notify();
        if !self.commit_input.read(cx).value().trim().is_empty() {
            return;
        }
        let root = self.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let message = cx.background_spawn(async move { git::last_message(&root) }).await.unwrap_or_default();
            let _ = this.update_in(cx, |this, window, cx| {
                if this.amend.as_ref().is_some_and(String::is_empty) && this.commit_input.read(cx).value().trim().is_empty() {
                    this.commit_input.update(cx, |input, cx| input.set_value(message.clone(), window, cx));
                    this.amend = Some(message);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// One of the commit box's or the branch line's actions, from a button
    /// or the menu bar; where it has nothing to do, it says so.
    pub fn git_command(&mut self, what: GitCommand, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(why) = self.git_blocked(what) {
            self.action = Some((false, why));
            return cx.notify();
        }
        let b = &self.branch_info;
        let upstream = b.upstream.is_some();
        let base = b.base.clone().filter(|_| b.behind_base > 0);
        match what {
            // As JetBrains' Commit…: to the message, to look before committing.
            GitCommand::Commit => {
                let input = self.commit_input.clone();
                window.defer(cx, move |window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
            }
            GitCommand::WriteMessage => self.suggest_message(window, cx),
            GitCommand::Stash => {
                let message = self.commit_input.read(cx).value().trim().to_string();
                self.run_action("Stash", move |root| git::stash(&root, &message), false, window, cx)
            }
            GitCommand::Push => self.push(window, cx),
            GitCommand::PullRequest => self.run_action("PR", move |root| git::pull_request(&root, upstream), true, window, cx),
            GitCommand::Fetch => self.run_action("Fetch", |root| git::fetch(&root), false, window, cx),
            GitCommand::Pull => {
                // Known to conflict: not tried, the agent is asked to merge it.
                let files = self.branch_info.pull_conflicts.clone();
                if !files.is_empty() {
                    let list = files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
                    let upstream = self.branch_info.upstream.clone().unwrap_or_else(|| "the upstream".into());
                    let told = self.ask_about_conflict(
                        format!("Take in {upstream} (fetch, then rebase or merge as this branch does). With the work here, committed or not, it conflicts in:\n{list}\n\nResolve the conflicts keeping what both sides meant, and check that it builds and the tests pass."),
                        window,
                        cx,
                    );
                    self.action = Some((false, format!("Pull: {} file(s) would conflict, nothing changed{told}", files.len())));
                    return cx.notify();
                }
                self.run_action("Pull", |root| git::pull_safe(&root), false, window, cx)
            }
            GitCommand::Unstash => {
                if let Some(stash) = self.own_stash().cloned() {
                    self.run_action("Unstash", move |root| git::stash_apply(&root, &stash, true), false, window, cx)
                }
            }
            GitCommand::Rebase => {
                if let Some(base) = base {
                    self.rebase_onto_base(base, window, cx)
                }
            }
        }
    }

    /// Why it cannot be done now: what greys its button and its menu item.
    pub fn git_blocked(&self, what: GitCommand) -> Option<String> {
        match self.busy {
            Some(busy) => Some(format!("{busy} is still running")),
            None => self.git_nothing(what).map(String::from),
        }
    }

    /// For the buttons and the menu bar: nothing is written.
    pub fn git_can(&self, what: GitCommand) -> bool {
        self.busy.is_none() && self.git_nothing(what).is_none()
    }

    /// The newest stash made on this task's branch: the list is the whole
    /// repository's, every worktree adds to it.
    fn own_stash(&self) -> Option<&git::Stash> {
        let head = &self.branch_info.head;
        self.stashes.iter().find(|s| {
            let on = s.what.strip_prefix("WIP on ").or_else(|| s.what.strip_prefix("On "));
            on.and_then(|rest| rest.split_once(':')).is_some_and(|(branch, _)| branch == head)
        })
    }

    fn git_nothing(&self, what: GitCommand) -> Option<&'static str> {
        let b = &self.branch_info;
        let why = match what {
            // Amend changes the last commit's message alone, as the commit box's button does.
            GitCommand::Commit if self.files.is_empty() && self.amend.is_none() => "Nothing has changed",
            GitCommand::WriteMessage | GitCommand::Stash if self.files.is_empty() => "Nothing has changed",
            GitCommand::Push | GitCommand::PullRequest if !b.has_origin => "No remote named origin",
            GitCommand::Push if b.upstream.is_some() && b.ahead == 0 => "Nothing to push",
            GitCommand::Pull if b.upstream.is_none() => "The branch has no upstream",
            GitCommand::Unstash if self.own_stash().is_none() => "Nothing is stashed on this branch",
            GitCommand::Rebase if b.base.is_none() || b.behind_base == 0 => "The branch is not behind its base",
            _ => return None,
        };
        Some(why)
    }

    pub(super) fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.commit_input.read(cx).value().trim().to_string();
        if message.is_empty() {
            self.action = Some((false, "Write a commit message first".into()));
            let input = self.commit_input.clone();
            window.defer(cx, move |window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
            cx.notify();
            return;
        }
        if !self.conflict_marked.is_empty() && !std::mem::replace(&mut self.markers_accepted, true) {
            self.action = Some((false, "Conflict markers are left in the files above. Commit again to commit them as they are.".into()));
            cx.notify();
            return;
        }
        let amend = self.amend.is_some() && self.can_amend();
        self.run_action(if amend { "Amend" } else { "Commit" }, move |root| git::commit(&root, &message, amend), false, window, cx);
    }

    /// Pushes; turned down for commits the remote has, asks whether to
    /// pull them first or overwrite them.
    pub(super) fn push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Push");
        self.action = None;
        self.discard_undo = None;
        cx.notify();
        let (root, upstream) = (self.root.clone(), self.branch_info.upstream.is_some());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(crate::procs::on_thread(move || git::push_or_tell(&root, upstream))).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(out) => this.action = Some((true, out.lines().next().filter(|l| !l.is_empty()).unwrap_or("Push: done").to_string())),
                    Err(Ok(diverged)) => {
                        this.action = Some((false, format!("Push: {} has commits this branch has not", diverged.target.tracking())));
                        this.diverged_dialog(diverged, window, cx);
                    }
                    Err(Err(error)) => this.action = Some((false, format!("Push: {error}"))),
                }
                let _ = this.refresh_tx.try_send(());
                cx.notify();
            });
        })
        .detach();
    }

    /// A push turned down: pull and push again, or, for a branch of one's
    /// own, overwrite what the remote has.
    fn diverged_dialog(&mut self, diverged: git::Diverged, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let pull = {
            let this = this.clone();
            move |window: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |task, cx| {
                    let upstream = task.branch_info.upstream.is_some();
                    let asked = "Pull the remote branch into this one with a rebase (`git pull --rebase`), then push.".to_string();
                    task.run_or_ask_agent("Pull and push", move |root| git::pull_and_push(&root, upstream), asked, window, cx)
                });
            }
        };
        let theirs = diverged.theirs.len();
        let tracking = diverged.target.tracking();
        let said = format!(
            "{tracking} has {theirs} commit{} this branch has not: someone pushed to it, or this branch was rebased since it was pushed.",
            if theirs == 1 { "" } else { "s" }
        );
        window.open_dialog(cx, move |dialog, _, cx| {
            let (pull, ok_pull, this, diverged) = (pull.clone(), pull.clone(), this.clone(), diverged.clone());
            let muted = cx.theme().muted_foreground;
            let listed: Vec<String> = diverged.theirs.iter().take(6).cloned().chain((diverged.theirs.len() > 6).then(|| format!("and {} more", diverged.theirs.len() - 6))).collect();
            let keep = diverged.keep.clone();
            let footer = h_flex()
                .w_full()
                .gap_2()
                .justify_end()
                .when(keep.is_none(), |row| {
                    let (this, diverged) = (this.clone(), diverged.clone());
                    row.child(Button::new("push-overwrite").small().ghost().label("Overwrite…").tooltip("Replace the remote branch with this one").on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let diverged = diverged.clone();
                        let _ = this.update(cx, |task, cx| task.overwrite_dialog(diverged, window, cx));
                    }))
                })
                .child(Button::new("push-cancel").small().ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                .child(Button::new("push-pull").small().primary().label("Pull and push").on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    pull(window, cx);
                }));
            dialog
                .title("Push turned down")
                .w(px(520.))
                .content({
                    let said = said.clone();
                    move |content, _, _| {
                        content
                            .child(div().text_sm().child(said.clone()))
                            .child(v_flex().pt_2().gap_0p5().text_xs().text_color(muted).children(listed.iter().map(|line| div().truncate().child(line.clone()))))
                            .child(div().pt_2().text_xs().text_color(muted).child(match &keep {
                                Some(why) => format!("Pull and push rebases this branch's commits onto them. It is not overwritten: {why}."),
                                None => "Pull and push rebases this branch's commits onto them; a conflict stops it and changes nothing.".to_string(),
                            }))
                    }
                })
                .on_ok(move |_, window, cx| {
                    ok_pull(window, cx);
                    true
                })
                .footer(footer)
        });
    }

    /// Asked once more, naming what goes.
    fn overwrite_dialog(&mut self, diverged: git::Diverged, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let tracking = diverged.target.tracking();
        let theirs = diverged.theirs.len();
        let listed = diverged.theirs.iter().take(6).cloned().collect::<Vec<_>>().join("\n");
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (this, diverged) = (this.clone(), diverged.clone());
            dialog
                .confirm()
                .title(format!("Overwrite {tracking}?"))
                .description(format!(
                    "{} on {tracking} that {} not here will be lost:\n{listed}\n\nIf the remote branch moves again before this push, nothing is overwritten.",
                    if theirs == 1 { "The commit".to_string() } else { format!("The {theirs} commits") },
                    if theirs == 1 { "is" } else { "are" }
                ))
                .ok_text("Overwrite")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let diverged = diverged.clone();
                    let _ = this.update(cx, |task, cx| task.run_action("Overwrite", move |root| git::push_overwrite(&root, &diverged), false, window, cx));
                    true
                })
        });
    }

    /// The last commit is taken back, after asking. One that is nowhere but
    /// here is undone: its changes staged again, its message back in the
    /// box. One that is pushed stays in the history and is reverted by a
    /// commit of its opposite: taking it out would be a push that overwrites.
    pub(super) fn take_back_last_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (hash, subject, pushed) = (self.branch_info.last_hash.clone(), self.branch_info.last_subject.clone(), self.branch_info.head_pushed);
        if hash.is_empty() {
            return;
        }
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (this, hash) = (this.clone(), hash.clone());
            let (title, told, ok) = if pushed {
                ("Revert the last commit?", "It is pushed, so it stays in the history: a new commit undoes what it changed. On a conflict nothing changes and the agent is asked.", "Revert")
            } else {
                ("Undo the last commit?", "It is not pushed, so it is taken out: its changes are staged again and its message is back in the box. No file changes.", "Undo commit")
            };
            // Which commit, in a line: a subject can be a paragraph, and read as the dialog's own words.
            let mut named: String = subject.chars().take(90).collect();
            if named.len() < subject.len() {
                named.push('…');
            }
            let (title, told) = (title.to_string(), format!("{told}\n\nThe commit: {hash} · {named}"));
            dialog.confirm().title(title).description(told).ok_text(ok).on_ok(move |_, window, cx| {
                let hash = hash.clone();
                let _ = this.update(cx, |this, cx| {
                    if pushed {
                        this.commit_action(1, hash, window, cx)
                    } else {
                        this.run_action("Undo commit", move |root| git::undo_commit(&root, &hash), false, window, cx)
                    }
                });
                true
            })
        });
    }

    /// Runs a git/gh job off the UI thread, shows its outcome under the
    /// buttons, and refreshes. A successful commit clears the message.
    /// What History does with a commit: 0 takes it into this branch, 1
    /// commits its opposite, 2 starts a task from it. A conflict changes
    /// nothing and goes to the agent as a request.
    pub fn commit_action(&mut self, what: u8, hash: String, window: &mut Window, cx: &mut Context<Self>) {
        let short = hash.get(..8).unwrap_or(&hash).to_string();
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
                        if label == "Commit" || label == "Amend" {
                            this.commit_input.update(cx, |input, cx| input.set_value("", window, cx));
                            this.amend = None;
                        }
                        // The message of the commit taken back, to make it again: unless another is being written.
                        if let (true, Some((_, message))) = (label == "Undo commit", out.split_once('\n')) {
                            if this.commit_input.read(cx).value().trim().is_empty() {
                                let message = message.to_string();
                                this.commit_input.update(cx, |input, cx| input.set_value(message, window, cx));
                            }
                        }
                        // A worktree came, or the one here is on another branch: the list of tasks follows now.
                        if matches!(label, "Open" | "Switch" | "Move") {
                            cx.emit(TaskEvent::Worktrees);
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
