use super::*;

// ---------------------------------------------------------------- changes

#[derive(Clone, Copy, PartialEq)]
pub enum DiffScope {
    /// The worktree against HEAD.
    Uncommitted,
    /// The worktree against where the branch forked from its base.
    Branch,
}

pub struct ChangesView {
    pub(super) root: PathBuf,
    pub diff: Entity<DiffState>,
    pub(super) scope: DiffScope,
    /// File counts for both scopes, and the base branch, for the toggle.
    pub(super) counts: (usize, usize),
    pub(super) base: Option<String>,
    /// First changed line per file in the diff shown, for "open".
    pub(super) first_lines: Rc<HashMap<String, u32>>,
    /// The single changes of the diff shown: each can be taken back or
    /// staged without the rest of its file.
    pub(super) hunks: Rc<Vec<git::Hunk>>,
    /// Where each file's are in `hunks`: looked up for every hunk drawn.
    hunks_of: Rc<HashMap<String, Vec<usize>>>,
    /// Those of them that are staged, by `staged_key`.
    pub(super) staged: Rc<HashSet<u64>>,
    pub(super) selection: Option<DiffLineRange>,
    pub(super) task: WeakEntity<TaskView>,
    /// The branches to compare with, for the menu: listed off the UI thread.
    pub(super) branches: Rc<std::cell::RefCell<Vec<String>>>,
    /// Review comments on lines of the diff, sent to the agent as one message.
    pub(super) review: Vec<crate::state::ReviewNote>,
    /// The text of the last selection: a click in the context menu clears
    /// the selection before its action runs.
    pub(super) selected_code: String,
    /// Each shown file's part of the patch, hashed: what the selection was made in.
    pub(super) shown_keys: Rc<HashMap<String, u64>>,
    /// Where the keyboard review is: the file, and in it the change (its
    /// `git::Hunk` line and side) the keys act on.
    pub(super) review_file: Option<SharedString>,
    pub(super) review_change: Option<(u32, bool)>,
}

actions!(ideae_review, [NextChange, PrevChange, NextChangedFile, PrevChangedFile, ToggleFileViewed, StageChange, RevertChange, CommentOnChange, JumpToSource]);

/// Lists the repository's branches, newest first, into `known`.
pub(super) fn load_branches<T: 'static>(root: PathBuf, known: Rc<std::cell::RefCell<Vec<String>>>, cx: &mut Context<T>) {
    cx.spawn(async move |_, cx| {
        let found = cx.background_spawn(async move { git::branches(&root).into_iter().take(40).collect::<Vec<_>>() }).await;
        *known.borrow_mut() = found;
    })
    .detach();
}

impl ChangesView {
    pub(super) fn new(root: PathBuf, task: WeakEntity<TaskView>, cx: &mut Context<Self>) -> Self {
        let mode = if crate::state::with_settings(|s| s.diff_unified) { DiffMode::Unified } else { DiffMode::Split };
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(mode));
        cx.observe(&diff, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&diff, |this, _, event: &DiffEvent, cx| match event {
            DiffEvent::SelectionEnded(range) => {
                this.selection = Some(range.clone());
                this.selected_code = this.diff.read(cx).selected_text(cx);
                cx.notify();
            }
            // Not cleared on SelectionChanged(None): pressing the Open button
            // clears the diff selection, which would remove the button mid-click.
            _ => {}
        })
        .detach();
        let branches = Rc::default();
        load_branches(root.clone(), Rc::clone(&branches), cx);
        Self {
            root,
            diff,
            scope: DiffScope::Uncommitted,
            counts: (0, 0),
            base: None,
            first_lines: Rc::default(),
            selection: None,
            task,
            review: Vec::new(),
            selected_code: String::new(),
            shown_keys: Rc::default(),
            branches,
            hunks: Rc::default(),
            hunks_of: Rc::default(),
            staged: Rc::default(),
            review_file: None,
            review_change: None,
        }
    }

    /// The selected lines: where they are, their text, their language.
    pub(super) fn selected(&self, cx: &App) -> Option<(String, String, &'static str)> {
        let range = self.selected_range(cx)?;
        let mut code = self.diff.read(cx).selected_text(cx);
        if code.is_empty() {
            code = self.selected_code.clone();
        }
        if code.is_empty() {
            return None;
        }
        Some((location_of(&range), code, lsp::language_for(std::path::Path::new(range.path().as_ref()))))
    }

    pub(super) fn selected_range(&self, cx: &App) -> Option<DiffLineRange> {
        self.diff.read(cx).selected_lines().or(self.selection.clone())
    }

    /// A review note for `range`: the selected text when that is the range,
    /// else the lines read from the file (the changed side).
    pub(super) fn note_for(&self, range: &DiffLineRange, cx: &App) -> crate::state::ReviewNote {
        let modified = range.end_side() == DiffSide::Modified;
        let mut code = String::new();
        if self.selected_range(cx).as_ref() == Some(range) {
            code = self.diff.read(cx).selected_text(cx);
            if code.is_empty() {
                code = self.selected_code.clone();
            }
        }
        if code.is_empty() && modified {
            if let Ok(text) = std::fs::read_to_string(self.root.join(range.path().as_ref())) {
                let (a, b) = (range.start().min(range.end()), range.start().max(range.end()));
                code = text.lines().skip(a.saturating_sub(1)).take(b + 1 - a.max(1)).collect::<Vec<_>>().join("\n");
            }
        }
        crate::state::ReviewNote {
            location: location_of(range),
            code,
            language: lsp::language_for(std::path::Path::new(range.path().as_ref())).to_string(),
            text: String::new(),
            path: range.path().to_string(),
            modified_side: modified,
            line: range.end(),
            ..Default::default()
        }
    }

    /// Asks for a comment on the selected lines and adds it to the review.
    pub(super) fn comment_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.selected_range(cx) else { return };
        let note = self.note_for(&range, cx);
        self.ask_comment(note, window, cx);
    }

    pub(super) fn ask_comment(&mut self, note: crate::state::ReviewNote, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx).auto_grow(3, 12).placeholder("What should change here?")
        });
        let view = cx.weak_entity();
        let focused = Rc::new(std::cell::Cell::new(false));
        let title = note.location.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, view, focused) = (input.clone(), view.clone(), focused.clone());
            let note = note.clone();
            let shown = input.clone();
            let add = move |window: &mut Window, cx: &mut App| {
                let text = input.read(cx).value().trim().to_string();
                if text.is_empty() {
                    return;
                }
                let comment = crate::state::ReviewNote { text, ..note.clone() };
                let _ = view.update(cx, |c, cx| {
                    c.review.push(comment);
                    cx.notify();
                });
                window.close_dialog(cx);
            };
            dialog
                .title(format!("Comment on {title}"))
                .w(px(560.))
                .content(move |content, window, cx| {
                    if !focused.replace(true) {
                        let input = shown.clone();
                        window.defer(cx, move |window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
                    }
                    content.child(gpui_kit::component::input::Textarea::new(&shown))
                })
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("review-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("review-add").primary().label("Add comment").on_click(move |_, window, cx| add(window, cx))),
                )
        });
    }

    /// The open review comments to the agent in one message, pasted for
    /// the user to send: new ones, and those sent before whose code is
    /// still as it was after the agent's turn. They stay, marked sent, to
    /// be looked at again when its turn ends ([`Self::recheck_review`]).
    pub(super) fn send_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let item = |n: usize, c: &crate::state::ReviewNote| format!("\n{n}. {}\n```{}\n{}\n```\n{}\n", c.location, c.language, c.code.trim_end_matches('\n'), c.text);
        let (new, again): (Vec<&crate::state::ReviewNote>, Vec<&crate::state::ReviewNote>) = self.review.iter().filter(|c| c.open()).partition(|c| c.sent == 0);
        if new.is_empty() && again.is_empty() {
            return;
        }
        let mut message = String::new();
        if !new.is_empty() {
            message.push_str("Review comments on the changes:\n");
            for (i, c) in new.iter().enumerate() {
                message.push_str(&item(i + 1, c));
            }
        }
        if !again.is_empty() {
            message.push_str(if new.is_empty() { "" } else { "\n" });
            message.push_str("Still open from the review before: the code there has not changed.\n");
            for (i, c) in again.iter().enumerate() {
                message.push_str(&item(new.len() + i + 1, c));
            }
        }
        for c in self.review.iter_mut().filter(|c| c.open()) {
            c.sent += 1;
            c.changed = None;
        }
        cx.notify();
        // Once this view's own update is over: the task may show this view
        // a newer diff on the way, which reads it.
        let task = self.task.clone();
        window.defer(cx, move |window, cx| {
            let _ = task.update(cx, |t, cx| t.paste_to_agent(message, window, cx));
        });
    }

    /// The agent's turn ended (`again` false): each comment sent is looked
    /// for in its file, off the UI thread. Its code still there moves the
    /// comment to where it is now; gone or changed, the comment says to look
    /// again. `again`, the files changed: those looked at after a turn are
    /// looked at once more (you may have changed the code yourself); one
    /// just sent waits for the agent's turn.
    pub(super) fn recheck_review(&mut self, again: bool, cx: &mut Context<Self>) {
        let asked: Vec<(usize, crate::state::ReviewNote)> =
            self.review.iter().cloned().enumerate().filter(|(_, n)| n.sent > 0 && n.modified_side && n.changed.is_some() == again).collect();
        if asked.is_empty() {
            return;
        }
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    asked
                        .into_iter()
                        .map(|(i, note)| {
                            let at = std::fs::read_to_string(root.join(&note.path)).ok().and_then(|text| note.find_in(&text));
                            (i, note, at)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |c, cx| {
                for (i, before, at) in found {
                    // Not one removed or sent again meanwhile.
                    let Some(note) = c.review.get_mut(i).filter(|n| **n == before) else { continue };
                    if note.changed == Some(at.is_none()) && at.is_none_or(|end| end == note.line) {
                        continue;
                    }
                    note.changed = Some(at.is_none());
                    if let Some(end) = at {
                        let lines = before.code.trim_end_matches('\n').matches('\n').count();
                        note.line = end;
                        note.location = if lines == 0 { format!("{}:{end}", note.path) } else { format!("{}:{}-{end}", note.path, end - lines) };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_review(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.review.is_empty() {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        Some(
            v_flex()
                .px_3()
                .py_2()
                .gap_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary.opacity(0.5))
                .child(
                    h_flex()
                        .justify_between()
                        .child(div().text_xs().text_color(muted).child(format!("REVIEW ({})", self.review.len())))
                        .child(
                            h_flex()
                                .gap_1()
                                .when(self.review.iter().any(|c| c.changed == Some(true)), |row| {
                                    row.child(
                                        Button::new("review-done")
                                            .xsmall()
                                            .ghost()
                                            .label("Remove changed")
                                            .tooltip("The comments whose code the agent changed, once you have looked")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.review.retain(|c| c.changed != Some(true));
                                                cx.notify();
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("review-clear")
                                        .xsmall()
                                        .ghost()
                                        .label("Clear")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.review.clear();
                                            cx.notify();
                                        })),
                                )
                                .child({
                                    let open = self.review.iter().filter(|c| c.open()).count();
                                    Button::new("review-send")
                                        .xsmall()
                                        .primary()
                                        .disabled(open == 0)
                                        .label(if open == self.review.len() { "Send to agent".to_string() } else { format!("Send {open} to agent") })
                                        .tooltip("New comments, and those sent before whose code the agent has not changed")
                                        .on_click(cx.listener(|this, _, window, cx| this.send_review(window, cx)))
                                }),
                        ),
                )
                .children(self.review.iter().enumerate().map(|(i, c)| {
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(div().flex_none().font_family(cx.theme().mono_font_family.clone()).text_xs().text_color(muted).child(c.location.clone()))
                        .child(div().flex_1().min_w_0().truncate().child(c.text.clone()))
                        .children(note_state(c, cx).map(|(state, color)| div().flex_none().text_xs().text_color(color).child(state)))
                        .child(
                            Button::new(("review-remove", i))
                                .xsmall()
                                .ghost()
                                .label("×")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if i < this.review.len() {
                                        this.review.remove(i);
                                    }
                                    cx.notify();
                                })),
                        )
                }))
                .into_any_element(),
        )
    }

    /// The lines selected by their numbers, to the agent with their location.
    pub(super) fn send_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((location, code, language)) = self.selected(cx) else { return };
        let task = self.task.clone();
        window.defer(cx, move |window, cx| {
            let _ = task.update(cx, |t, cx| t.send_to_agent(location, code, language, window, cx));
        });
    }

    pub(super) fn set_hunks(&mut self, hunks: Rc<Vec<git::Hunk>>, cx: &mut Context<Self>) {
        let mut of: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, hunk) in hunks.iter().enumerate() {
            of.entry(hunk.path.clone()).or_default().push(i);
        }
        self.hunks_of = Rc::new(of);
        self.hunks = hunks;
        // The change at hand, staged or reverted, is marked no more.
        let current = self.review_hunk(cx).map(|hunk| change_range(&hunk));
        self.diff.update(cx, |d, cx| d.set_current_lines(current, cx));
    }

    fn hunks_in(&self, path: &str) -> &[usize] {
        self.hunks_of.get(path).map_or(&[], Vec::as_slice)
    }

    pub(super) fn set_scope(&mut self, scope: DiffScope, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        // Read from the task rather than asking it to push: it would read this
        // view back while it is being updated.
        let Ok((files, lines, hunks)) = self.task.read_with(cx, |task, _| match scope {
            DiffScope::Uncommitted => (task.uncommitted_diff.clone(), task.uncommitted_lines.clone(), task.uncommitted_hunks.clone()),
            DiffScope::Branch => (task.branch_diff.clone(), task.branch_lines.clone(), task.branch_hunks.clone()),
        }) else {
            return;
        };
        self.first_lines = lines;
        self.set_hunks(hunks, cx);
        self.diff.update(cx, |d, cx| d.set_files(files, cx));
        // The other scope's files: those viewed fold there too.
        let diff = self.diff.clone();
        let task = self.task.clone();
        cx.defer(move |cx| {
            let _ = task.update(cx, |task, cx| {
                task.folded_viewed.clear();
                task.fold_viewed(&diff, cx);
            });
        });
        cx.notify();
    }

    pub(super) fn open_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.selection.clone() else { return };
        self.open_at(range.path().clone(), range.start().saturating_sub(1) as u32, window, cx);
    }

    /// F4, as JetBrains' Jump to Source: the file at the lines selected, else
    /// at the change J/K came to, else at its top.
    fn jump_to_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (path, line) = if let Some(range) = self.diff.read(cx).selected_lines() {
            (range.path().clone(), range.start().saturating_sub(1) as u32)
        } else if let Some(hunk) = self.review_hunk(cx).filter(|hunk| hunk.modified) {
            (hunk.path.into(), hunk.line.saturating_sub(1))
        } else if let Some(range) = self.selection.clone() {
            (range.path().clone(), range.start().saturating_sub(1) as u32)
        } else if let Some(file) = self.review_file(cx) {
            (file, 0)
        } else {
            return;
        };
        self.open_at(path, line, window, cx);
    }

    fn open_at(&mut self, path: SharedString, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.root.join(path.as_ref());
        let task = self.task.clone();
        window.defer(cx, move |window, cx| {
            let _ = task.update(cx, |task, cx| task.open_file(path, line, window, cx));
        });
    }
}

/// Reviewing by keys: J/K and F7 from change to change, N/P from file to
/// file, V, S, R and C on the one at hand.
impl ChangesView {
    /// The files in the order shown.
    fn shown_files(&self, cx: &App) -> Vec<SharedString> {
        self.diff.read(cx).files().iter().map(|f| f.path().clone()).collect()
    }

    /// The file the keys act on: the one moved to last, else the one at the top.
    fn review_file(&self, cx: &App) -> Option<SharedString> {
        let files = self.shown_files(cx);
        self.review_file.clone().filter(|f| files.contains(f)).or_else(|| self.diff.read(cx).top_file())
    }

    /// The changes in the order shown, those of folded files left out
    /// unless every file is folded.
    fn review_order(&self, cx: &App) -> Vec<usize> {
        let diff = self.diff.read(cx);
        let by_file = |open_only: bool| -> Vec<usize> {
            let mut order = Vec::new();
            for file in diff.files() {
                if open_only && diff.is_file_collapsed(file.path()) {
                    continue;
                }
                order.extend(self.hunks_in(file.path()).iter().copied());
            }
            order
        };
        let open = by_file(true);
        if open.is_empty() { by_file(false) } else { open }
    }

    /// The change the keys act on, if it is still there.
    fn review_hunk(&self, cx: &App) -> Option<git::Hunk> {
        let (line, modified) = self.review_change?;
        let file = self.review_file(cx)?;
        self.hunks_in(&file).iter().map(|&i| &self.hunks[i]).find(|h| h.line == line && h.modified == modified).cloned()
    }

    fn step_change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let order = self.review_order(cx);
        if order.is_empty() {
            return;
        }
        let file = self.review_file(cx);
        let at = |i: usize| &self.hunks[order[i]];
        let in_file = |i: usize| file.as_ref().is_some_and(|f| at(i).path == f.as_ref());
        let current = self.review_change.and_then(|(line, modified)| (0..order.len()).find(|&i| in_file(i) && at(i).line == line && at(i).modified == modified));
        let next = match current {
            Some(i) if forward => (i + 1) % order.len(),
            Some(i) => (i + order.len() - 1) % order.len(),
            // Gone (reverted) or none yet: from where it was in its file, else
            // from the file at the top.
            None => {
                let near = self.review_change.and_then(|(line, _)| match forward {
                    true => (0..order.len()).find(|&i| in_file(i) && at(i).line >= line),
                    false => (0..order.len()).rev().find(|&i| in_file(i) && at(i).line < line),
                });
                near.or_else(|| (0..order.len()).find(|&i| in_file(i))).unwrap_or(0)
            }
        };
        let hunk = self.hunks[order[next]].clone();
        self.select_change(&hunk, true, cx);
    }

    /// The change of `path` at or after `line` (else its last), as the
    /// one at hand, in view.
    pub(super) fn show_change_at(&mut self, path: &str, line: u32, cx: &mut Context<Self>) -> bool {
        let found = {
            let hunks = self.hunks_in(path);
            hunks.iter().map(|&i| &self.hunks[i]).find(|h| h.line >= line).or_else(|| hunks.last().map(|&i| &self.hunks[i])).cloned()
        };
        match found {
            Some(hunk) => {
                self.select_change(&hunk, true, cx);
                true
            }
            None => false,
        }
    }

    /// Marks `hunk` as the change at hand, beside its lines: what is
    /// selected stays selected.
    fn select_change(&mut self, hunk: &git::Hunk, scroll: bool, cx: &mut Context<Self>) {
        let range = change_range(hunk);
        self.diff.update(cx, |d, cx| {
            if scroll {
                d.scroll_to_line_in_view(DiffLinePosition::new(range.path().clone(), range.side(), range.start()), cx);
            }
            d.set_current_lines(Some(range), cx);
        });
        self.review_file = Some(hunk.path.clone().into());
        self.review_change = Some((hunk.line, hunk.modified));
        cx.notify();
    }

    /// The next or previous file, or the next one not viewed (`unviewed`).
    fn step_file(&mut self, forward: bool, unviewed: bool, cx: &mut Context<Self>) {
        let files = self.shown_files(cx);
        if files.is_empty() {
            return;
        }
        let current = self.review_file(cx).and_then(|f| files.iter().position(|p| *p == f));
        let viewed = |path: &str| unviewed && self.task.upgrade().is_some_and(|t| t.read(cx).is_path_viewed(path));
        let count = files.len();
        let target = (1..=count)
            .map(|step| match (current, forward) {
                (Some(i), true) => (i + step) % count,
                (Some(i), false) => (i + count - step % count) % count,
                (None, _) => step - 1,
            })
            .find(|&i| !viewed(&files[i]));
        let Some(target) = target else { return };
        let path = files[target].clone();
        self.diff.update(cx, |d, cx| d.scroll_to_file(&path, cx));
        self.review_file = Some(path.clone());
        self.review_change = None;
        match self.hunks_in(&path).first().map(|&i| self.hunks[i].clone()) {
            Some(hunk) if !self.diff.read(cx).is_file_collapsed(&path) => self.select_change(&hunk, false, cx),
            _ => self.diff.update(cx, |d, cx| d.set_current_lines(None, cx)),
        }
        cx.notify();
    }

    /// V: the file viewed (it folds) and on to the next one not viewed; or
    /// not viewed any more, staying.
    fn toggle_file_viewed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.review_file(cx) else { return };
        self.review_file = Some(path.clone());
        let (task, view) = (self.task.clone(), cx.weak_entity());
        // The task updates this view as it folds the file.
        window.defer(cx, move |_, cx| {
            let Ok(viewed) = task.update(cx, |t, cx| {
                t.toggle_viewed(&path, cx);
                t.is_path_viewed(&path)
            }) else {
                return;
            };
            if viewed {
                let _ = view.update(cx, |c, cx| c.step_file(true, true, cx));
            }
        });
    }

    fn act_on_change(&mut self, revert: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hunk) = self.review_hunk(cx) else { return };
        if !revert && self.scope != DiffScope::Uncommitted {
            return;
        }
        let staged = self.staged.contains(&staged_key(&hunk.path, &hunk.changes));
        let task = self.task.clone();
        window.defer(cx, move |window, cx| {
            let _ = task.update(cx, |t, cx| match revert {
                true => t.revert_hunk(hunk, window, cx),
                false => t.stage_hunk(hunk, !staged, window, cx),
            });
        });
    }

    fn comment_on_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hunk) = self.review_hunk(cx) else { return self.comment_selection(window, cx) };
        let mut note = self.note_for(&change_range(&hunk), cx);
        // Its own lines, removed ones too, rather than the file's.
        let sign = if hunk.modified { '+' } else { '-' };
        note.code = hunk.changes.lines().filter_map(|l| l.strip_prefix(sign)).collect::<Vec<_>>().join("\n");
        self.ask_comment(note, window, cx);
    }
}

impl Render for ChangesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Split or unified is one choice for every task's Changes, kept over a restart.
        let wanted = if crate::state::with_settings(|s| s.diff_unified) { DiffMode::Unified } else { DiffMode::Split };
        if self.diff.read(cx).mode() != wanted {
            self.diff.update(cx, |d, cx| d.set_mode(wanted, cx));
        }
        let mode = wanted;
        let empty = self.diff.read(cx).files().is_empty();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(files_button("changes-files"))
                    .child(
                        Button::new("scope-uncommitted")
                            .small()
                            .ghost()
                            .label(format!("Uncommitted ({})", self.counts.0))
                            .selected(self.scope == DiffScope::Uncommitted)
                            .on_click(cx.listener(|this, _, _, cx| this.set_scope(DiffScope::Uncommitted, cx))),
                    )
                    .child(
                        Button::new("scope-branch")
                            .small()
                            .ghost()
                            .disabled(self.base.is_none())
                            .label(match &self.base {
                                Some(base) => format!("Branch vs {base} ({})", self.counts.1),
                                None => "Branch (no base found)".to_string(),
                            })
                            .selected(self.scope == DiffScope::Branch)
                            .on_click(cx.listener(|this, _, _, cx| this.set_scope(DiffScope::Branch, cx))),
                    )
                    .child({
                        let (root, task, view, known) = (self.root.clone(), self.task.clone(), cx.weak_entity(), self.branches.clone());
                        Button::new("pick-base")
                            .small()
                            .ghost()
                            .label("▾")
                            .tooltip("Compare with another branch")
                            .dropdown_menu(move |mut menu, _, cx| {
                                // As listed last; asked again for the next time it opens.
                                load_branches(root.clone(), known.clone(), cx);
                                let branches = known.borrow().clone();
                                let direct = task.clone();
                                menu = menu
                                    .item(gpui_kit::component::menu::PopupMenuItem::new("With a branch, tag or commit as it is…").on_click(move |_, window, cx| {
                                        let _ = direct.update(cx, |t, cx| t.compare_with_dialog(None, window, cx));
                                    }))
                                    .separator()
                                    .label("Since the branch left");
                                for branch in branches {
                                    let (task, view, name) = (task.clone(), view.clone(), branch.clone());
                                    menu = menu.item(gpui_kit::component::menu::PopupMenuItem::new(branch).on_click(
                                        move |_, _, cx| {
                                            let _ = task.update(cx, |t, _| t.set_base(name.clone()));
                                            let _ = view.update(cx, |c, cx| c.set_scope(DiffScope::Branch, cx));
                                        },
                                    ));
                                }
                                menu
                            })
                    })
                    .child(div().w(px(1.)).h(px(16.)).bg(cx.theme().border))
                    // Both named, the one in use lit: one word alone read as a label, not as a switch.
                    .children([("mode-split", "Split", DiffMode::Split, "Old and new side by side"), ("mode-unified", "Unified", DiffMode::Unified, "Old and new one under the other")].map(
                        |(id, label, to, tip)| {
                            Button::new(id).small().ghost().label(label).tooltip(tip).selected(mode == to).on_click(cx.listener(move |this, _, _, cx| {
                                let mut settings = crate::state::settings();
                                settings.diff_unified = to == DiffMode::Unified;
                                crate::state::set_settings(settings);
                                this.diff.update(cx, |d, cx| d.set_mode(to, cx));
                            }))
                        },
                    ))
                    .child(whitespace_button("changes-whitespace"))
                    .child(div().w(px(1.)).h(px(16.)).bg(cx.theme().border))
                    .children([("fold-all", "Collapse all", true, "Every file to its header"), ("unfold-all", "Expand all", false, "Every file open, those viewed too")].map(
                        |(id, label, folded, tip)| {
                            Button::new(id).small().ghost().label(label).tooltip(tip).on_click(cx.listener(move |this, _, _, cx| {
                                this.diff.update(cx, |d, cx| d.set_all_collapsed(folded, cx));
                            }))
                        },
                    ))
                    .child(
                        div()
                            .id("changes-hint")
                            .ml_auto()
                            .px_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("ⓘ")
                            .tooltip(|window, cx| {
                                let key = crate::keymap::label;
                                gpui_kit::component::tooltip::Tooltip::new(format!(
                                    "{} a line to open the file there. Select lines by their numbers to comment on them or send them to the agent (right-click).\n\n\
                                     In the diff:\n\
                                     {} / {}, {} / {}   the next / previous change\n\
                                     {} / {}   the next / previous file\n\
                                     {}   the file viewed, on to the next\n\
                                     {} stage, {} revert, {} comment on the change\n\
                                     {}   the file, at the line",
                                    crate::keys::label("Ctrl+Click"),
                                    key("review_next_change"),
                                    key("review_prev_change"),
                                    key("next_difference"),
                                    key("prev_difference"),
                                    key("review_next_file"),
                                    key("review_prev_file"),
                                    key("review_viewed"),
                                    key("review_stage"),
                                    key("review_revert"),
                                    key("review_comment"),
                                    key("review_open_file"),
                                ))
                                .build(window, cx)
                            }),
                    )
                    .when_some(self.selection.clone(), |this, range| {
                        this.child(
                            Button::new("open-at-line")
                                .small()
                                .primary()
                                .label(format!("Open {}:{}", range.path(), range.start()))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_selection(window, cx)
                                })),
                        )
                    }),
            )
            .children(self.render_review(cx))
            .child(div().id("changes-diff").key_context("Changes").relative().flex_1().min_h_0()
                .child(crate::zoom::wheel(crate::zoom::Kind::Changes))
                .on_action(cx.listener(|this, _: &NextChange, _, cx| this.step_change(true, cx)))
                .on_action(cx.listener(|this, _: &PrevChange, _, cx| this.step_change(false, cx)))
                .on_action(cx.listener(|this, _: &NextChangedFile, _, cx| this.step_file(true, false, cx)))
                .on_action(cx.listener(|this, _: &PrevChangedFile, _, cx| this.step_file(false, false, cx)))
                .on_action(cx.listener(|this, _: &ToggleFileViewed, window, cx| this.toggle_file_viewed(window, cx)))
                .on_action(cx.listener(|this, _: &StageChange, window, cx| this.act_on_change(false, window, cx)))
                .on_action(cx.listener(|this, _: &RevertChange, window, cx| this.act_on_change(true, window, cx)))
                .on_action(cx.listener(|this, _: &CommentOnChange, window, cx| this.comment_on_change(window, cx)))
                .on_action(cx.listener(|this, _: &JumpToSource, window, cx| this.jump_to_source(window, cx)))
                .on_action(cx.listener(|this, _: &SendDiffToAgent, window, cx| this.send_selection(window, cx)))
                .on_action(cx.listener(|this, _: &CommentOnDiff, window, cx| this.comment_selection(window, cx)))
                .context_menu(|menu, _, _| {
                    menu.menu("Copy", Box::new(gpui_kit::component::input::Copy))
                        .separator()
                        .menu("Comment on selected lines…", Box::new(CommentOnDiff))
                        .menu("Send selected lines to Agent", Box::new(SendDiffToAgent))
                })
                .map(|this| {
                if empty {
                    this.flex()
                        .items_center()
                        .justify_center()
                        .text_color(cx.theme().muted_foreground)
                        .child("No uncommitted changes")
                } else {
                    let notes = Rc::new(self.review.clone());
                    let view = cx.weak_entity();
                    let add_view = cx.weak_entity();
                    let staging = self.scope == DiffScope::Uncommitted;
                    let files = self.diff.clone();
                    this.child(with_file_list(
                        &files,
                        "changes-file-list",
                        Diff::new(&self.diff)
                            .binary_content({
                                // Uncommitted or the branch: the after side of a change not staged is the worktree's file.
                                let root = self.root.clone();
                                move |file, split, window, cx| crate::image_diff::render(file, split, &root, true, window, cx)
                            })
                            .annotations(
                                notes
                                    .iter()
                                    .enumerate()
                                    .map(|(i, n)| {
                                        let side = if n.modified_side { DiffSide::Modified } else { DiffSide::Original };
                                        DiffAnnotation::line(("review-note", i), DiffLinePosition::new(n.path.clone(), side, n.line))
                                    })
                                    .collect::<Vec<_>>(),
                            )
                            .render_annotation(move |annotation, _, cx| {
                                let i = match annotation.id() {
                                    ElementId::NamedInteger(_, i) => *i as usize,
                                    _ => usize::MAX,
                                };
                                let text = notes.get(i).map(|n| n.text.clone()).unwrap_or_default();
                                let state = notes.get(i).and_then(|n| note_state(n, cx));
                                let view = view.clone();
                                h_flex()
                                    .mx_2()
                                    .my_1()
                                    .px_3()
                                    .py_1p5()
                                    .gap_2()
                                    .rounded_md()
                                    .border_l_2()
                                    .border_color(cx.theme().primary)
                                    .bg(cx.theme().secondary)
                                    .text_sm()
                                    // What became of it under its words, which keep the width.
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(text)
                                            .children(state.map(|(state, color)| div().text_xs().text_color(color).child(state))),
                                    )
                                    .child(Button::new(("review-note-remove", i)).xsmall().ghost().label("×").tooltip("Done with it").on_click(move |_, _, cx| {
                                        let _ = view.update(cx, |c, cx| {
                                            if i < c.review.len() {
                                                c.review.remove(i);
                                            }
                                            cx.notify();
                                        });
                                    }))
                                    .into_any_element()
                            })
                            .on_add_annotation(move |range, window, cx| {
                                let _ = add_view.update(cx, |c, cx| {
                                    let note = c.note_for(range, cx);
                                    c.ask_comment(note, window, cx);
                                });
                            })
                            .code_font_size(crate::zoom::size(crate::zoom::Kind::Changes, cx))
                            .line_number(true)
                            .render_header_suffix({
                                let (root, task, lines) = (self.root.clone(), self.task.clone(), self.first_lines.clone());
                                move |file, _, cx| {
                                    h_flex()
                                        .gap_2()
                                        .when(staging, |row| row.child(viewed_checkbox(&task, file, cx)))
                                        .child(open_file_button(&root, &task, file, &lines))
                                }
                            })
                            .render_header_status({
                                let task = self.task.clone();
                                move |file, _, cx| {
                                    let task = task.upgrade()?;
                                    let untracked = task.read(cx).file_states.get(file.path().as_ref()).is_some_and(|s| s.change == git::Change::Untracked);
                                    untracked.then(|| SharedString::new_static("Untracked"))
                                }
                            })
                            // What can be done with each change alone, on the row of its hunk.
                            .render_hunk_suffix({
                                let (hunks, hunks_of, task, in_index) = (self.hunks.clone(), self.hunks_of.clone(), self.task.clone(), self.staged.clone());
                                move |file, original, modified, _, _| hunk_actions(file, original, modified, &hunks, &hunks_of, &task, &in_index, staging)
                            })
                            .on_line_click({
                                let (root, task) = (self.root.clone(), self.task.clone());
                                move |position, event, window, cx| {
                                    // Ctrl+click opens; a plain click stays in the review.
                                    if !event.modifiers().secondary(cx) {
                                        return;
                                    }
                                    let path = root.join(position.path().as_ref());
                                    let line = position.line().saturating_sub(1) as u32;
                                    let task = task.clone();
                                    // After the click is done with the diff, which
                                    // opening the file may give other files.
                                    window.defer(cx, move |window, cx| {
                                        let _ = task.update(cx, |task, cx| task.open_file(path, line, window, cx));
                                    });
                                }
                            })
                            // Unwrapped split puts the new side past the right edge
                            // when any line is long, so split always wraps.
                            .soft_wrap(mode == DiffMode::Split),
                        cx,
                    ))
                }
            }))
    }
}

/// What a staged change is known by: its file and its lines, hashed. The
/// staged patch is not kept a second time for it.
pub(super) fn staged_key(path: &str, changes: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (path, changes).hash(&mut hasher);
    hasher.finish()
}

/// The first and last line a change covers on the side `line` is on.
/// The lines of a change, on the side it has them.
fn change_range(hunk: &git::Hunk) -> DiffLineRange {
    let (first, last) = change_lines(hunk);
    let side = if hunk.modified { DiffSide::Modified } else { DiffSide::Original };
    DiffLineRange::new(hunk.path.clone(), side, first as usize, last as usize)
}

pub(super) fn change_lines(hunk: &git::Hunk) -> (u32, u32) {
    let sign = if hunk.modified { '+' } else { '-' };
    let count = hunk.changes.lines().filter(|l| l.starts_with(sign)).count().max(1) as u32;
    (hunk.line.saturating_sub(count - 1), hunk.line)
}

/// Revert and Stage for the changes in one hunk of the diff: buttons for
/// the one change it has, menus naming their lines for several.
fn hunk_actions(
    file: &DiffFile,
    original: std::ops::Range<usize>,
    modified: std::ops::Range<usize>,
    hunks: &Rc<Vec<git::Hunk>>,
    hunks_of: &HashMap<String, Vec<usize>>,
    task: &WeakEntity<TaskView>,
    in_index: &Rc<HashSet<u64>>,
    staging: bool,
) -> Option<AnyElement> {
    use gpui_kit::component::menu::PopupMenuItem;
    let inside: Vec<usize> = hunks_of
        .get(file.path().as_ref())?
        .iter()
        .copied()
        .filter(|&i| {
            let h = &hunks[i];
            (if h.modified { &modified } else { &original }).contains(&(h.line as usize))
        })
        .collect();
    let &first = inside.first()?;
    let is_staged = |h: &git::Hunk| in_index.contains(&staged_key(&h.path, &h.changes));
    const REVERT_TIP: &str = "Take this change back; the rest of the file stays (Undo follows)";
    let mut row = h_flex().flex_none().gap_0p5();
    if inside.len() == 1 {
        let hunk = hunks[first].clone();
        let staged = is_staged(&hunk);
        let (revert, stage, staged_hunk) = (task.clone(), task.clone(), hunk.clone());
        row = row.child(Button::new(("hunk-revert", first)).xsmall().ghost().label("↺ Revert").tooltip(REVERT_TIP).on_click(move |_, window, cx| {
            let _ = revert.update(cx, |t, cx| t.revert_hunk(hunk.clone(), window, cx));
        }));
        if staging {
            let (label, tip) = if staged { ("− Unstage", "Take this change out of what is staged") } else { ("+ Stage", "Stage this change alone") };
            row = row.child(Button::new(("hunk-stage", first)).xsmall().ghost().label(label).tooltip(tip).on_click(move |_, window, cx| {
                let _ = stage.update(cx, |t, cx| t.stage_hunk(staged_hunk.clone(), !staged, window, cx));
            }));
        }
        return Some(row.into_any_element());
    }
    let named = |h: &git::Hunk| match change_lines(h) {
        (a, b) if a == b => format!("Line {b}"),
        (a, b) => format!("Lines {a}–{b}"),
    };
    let (revert_hunks, revert_task) = (hunks.clone(), task.clone());
    let revert_inside = inside.clone();
    row = row.child(Button::new(("hunk-revert", first)).xsmall().ghost().label("↺ Revert ▾").tooltip("Take one of the changes here back; the rest of the file stays (Undo follows)").dropdown_menu(
        move |mut menu, _, _| {
            for &i in &revert_inside {
                let (hunk, task) = (revert_hunks[i].clone(), revert_task.clone());
                menu = menu.item(PopupMenuItem::new(named(&hunk)).on_click(move |_, window, cx| {
                    let _ = task.update(cx, |t, cx| t.revert_hunk(hunk.clone(), window, cx));
                }));
            }
            menu
        },
    ));
    if staging {
        let (stage_hunks, stage_task, in_index) = (hunks.clone(), task.clone(), in_index.clone());
        row = row.child(Button::new(("hunk-stage", first)).xsmall().ghost().label("Stage ▾").tooltip("Stage or unstage one of the changes here alone").dropdown_menu(move |mut menu, _, _| {
            for &i in &inside {
                let (hunk, task) = (stage_hunks[i].clone(), stage_task.clone());
                let staged = in_index.contains(&staged_key(&hunk.path, &hunk.changes));
                let label = format!("{} {}", if staged { "Unstage" } else { "Stage" }, named(&hunk).to_lowercase());
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let _ = task.update(cx, |t, cx| t.stage_hunk(hunk.clone(), !staged, window, cx));
                }));
            }
            menu
        }));
    }
    Some(row.into_any_element())
}

/// What became of a comment sent to the agent, in the theme's colors: the
/// accent when it asks you to look.
fn note_state(note: &crate::state::ReviewNote, cx: &App) -> Option<(&'static str, Hsla)> {
    let (muted, look) = (cx.theme().muted_foreground, cx.theme().primary);
    match (note.sent, note.changed) {
        (0, _) => None,
        (_, None) => Some(("sent", muted)),
        (_, Some(true)) => Some(("changed: look again", look)),
        (_, Some(false)) => Some(("still open: goes with the next Send", muted)),
    }
}
