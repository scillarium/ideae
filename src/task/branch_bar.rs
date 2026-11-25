use super::*;

impl TaskView {
    pub(super) fn render_branch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let b = &self.branch_info;
        h_flex()
            .id("branch")
            .gap_2()
            .px_3()
            .py_1()
            .text_xs()
            // Narrow, what does not fit is cut at the right, not drawn over the panel beside.
            .overflow_hidden()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .child(
                        Button::new("nav-back")
                            .xsmall()
                            .ghost()
                            .icon(IconName::ArrowLeft)
                            .disabled(!self.can_navigate(true))
                            .tooltip("Back (Alt+Left, mouse Back)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.navigate(true, window, cx);
                                crate::learn::used("back", window, cx);
                            })),
                    )
                    .child(
                        Button::new("nav-forward")
                            .xsmall()
                            .ghost()
                            .icon(IconName::ArrowRight)
                            .disabled(!self.can_navigate(false))
                            .tooltip("Forward (Alt+Right, mouse Forward)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.navigate(false, window, cx);
                                crate::learn::used("forward", window, cx);
                            })),
                    ),
            )
            // Narrow, the side panel is over the task: from here, or Alt+1/2/3.
            .when(self.narrow, |bar| {
                bar.child(
                    Button::new("side-open")
                        .xsmall()
                        .ghost()
                        .label("◨")
                        .selected(self.side_over)
                        .tooltip(format!("Changes, Files, Structure, Run, Database  {}", crate::keymap::label("changes_panel")))
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.side_over = !this.side_over;
                            // With no panel held there is nothing to bring: Changes comes.
                            if this.side_over && !this.side_in_view() {
                                this.open_panel(RightPane::Changes, cx);
                            }
                            cx.notify();
                        })),
                )
            })
            .when(b.plain, |bar| {
                bar.child(div().text_color(cx.theme().muted_foreground).child("No git here: tasks share this folder, with no diffs and no undo of an agent's edits")).child(
                    Button::new("make-repository")
                        .xsmall()
                        .outline()
                        .label("Make it one")
                        .tooltip("git init, nothing committed: after your first commit tasks get worktrees of their own")
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.run_action("Make repository", |root| git::make_repository(&root), false, window, cx)
                        })),
                )
            })
            .when(!b.plain, |bar| {
                bar.child(
                    div()
                        .id("branch-name")
                        .font_semibold()
                        .cursor_pointer()
                        .hover(|s| s.text_color(cx.theme().primary))
                        .child(format!("⎇ {}", if b.head.is_empty() { "…" } else { &b.head }))
                        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("The repository's branches: open one as a task, compare, delete").build(window, cx))
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.branches_dialog(window, cx)
                        })),
                )
            })
            .when_some(b.upstream.clone(), |this, up| {
                this.child(div().text_color(cx.theme().muted_foreground).child(format!("→ {up}")))
                    .when(b.ahead > 0, |this| this.child(div().text_color(gpui_kit::green()).child(format!("↑{}", b.ahead))))
                    .when(b.behind > 0, |this| this.child(div().text_color(gpui_kit::red()).child(format!("↓{}", b.behind))))
            })
            .when(b.upstream.is_none() && !b.head.is_empty(), |this| {
                this.child(div().text_color(cx.theme().muted_foreground).child("no upstream"))
            })
            .when_some(b.base.clone().filter(|_| b.behind_base > 0 && self.off_default().is_none()), |this, base| {
                let busy = self.busy.is_some();
                let clash = !b.base_conflicts.is_empty();
                this.child(
                    Button::new("rebase")
                        .xsmall()
                        .ghost()
                        .disabled(busy)
                        .label(format!("{base} ↓{} · Rebase{}", b.behind_base, if clash { " ⚠" } else { "" }))
                        .when(clash, |button| button.text_color(gpui_kit::yellow()))
                        .tooltip(if clash {
                            format!("{base} has changed the same lines as this branch in {}: a rebase would stop there. A click asks the agent to do it and resolve them.", b.base_conflicts.join(", "))
                        } else {
                            format!("Rebase this branch onto {base}. On a conflict nothing changes, and the agent's prompt gets a request to do it.")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.rebase_onto_base(base.clone(), window, cx)
                        })),
                )
            })
            .child(
                div()
                    .id("branch-last-commit")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .cursor_pointer()
                    .when(!b.last_hash.is_empty(), |line| line.child(format!("· {} {}", b.last_hash, b.last_subject)))
                    .context_menu({
                        let (head, hash, subject) = (b.head.clone(), b.last_hash.clone(), b.last_subject.clone());
                        move |menu, _, _| {
                            menu.when(!hash.is_empty(), |menu| menu.item(crate::copying::item("Copy hash", hash.clone())).item(crate::copying::item("Copy subject", subject.clone())))
                                .item(crate::copying::item("Copy branch name", head.clone()))
                        }
                    }),
            )
            .when(!b.last_hash.is_empty(), |bar| {
                bar.child(
                    Button::new("take-back")
                        .xsmall()
                        .ghost()
                        .child(svg().path("side-icons/undo-2.svg").size(px(14.)).text_color(cx.theme().muted_foreground))
                        .tooltip(if b.head_pushed { "Revert the last commit: it is pushed, a new commit undoes it" } else { "Undo the last commit: its changes are staged again" })
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.take_back_last_commit(window, cx)
                        })),
                )
            })
            .children(self.render_auto_badges(cx))
            .children(self.render_pr_feedback(cx))
            .when(!b.plain, |bar| {
                bar.child(
                    Button::new("log")
                        .xsmall()
                        .ghost()
                        .label("History")
                        .tooltip("The branch's history (Ctrl+3)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.show_tab(|k| matches!(k, TabKind::History), window, cx)
                        })),
                )
            })
            .when(b.upstream.is_some(), |bar| {
                let busy = self.busy.is_some();
                bar.child(
                    Button::new("fetch")
                        .xsmall()
                        .ghost()
                        .disabled(busy)
                        .label("Fetch")
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.git_command(GitCommand::Fetch, window, cx)
                        })),
                )
                .child(
                    Button::new("pull")
                        .xsmall()
                        .ghost()
                        .disabled(busy)
                        .label(match (b.behind, b.pull_conflicts.is_empty()) {
                            (0, _) => "Pull".to_string(),
                            (behind, true) => format!("Pull ↓{behind}"),
                            (behind, false) => format!("Pull ↓{behind} ⚠"),
                        })
                        .when(!b.pull_conflicts.is_empty(), |button| button.text_color(gpui_kit::yellow()))
                        .tooltip(if b.pull_conflicts.is_empty() {
                            "Fast-forward, or rebase local commits; not tried when it would conflict".to_string()
                        } else {
                            format!("Would conflict with your work in {}: not tried, a click asks the agent to take it in", b.pull_conflicts.join(", "))
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.git_command(GitCommand::Pull, window, cx);
                            crate::learn::used("pull", window, cx);
                        })),
                )
            })
            // The branch's last commit, in History.
            .on_click(cx.listener(|this, _, window, cx| {
                let hash = this.branch_info.last_hash.clone();
                this.show_commit(&hash, window, cx)
            }))
    }

    /// The main thread on another branch than the default one, unless it was
    /// asked to stay there.
    fn off_default(&self) -> Option<&git::OffDefault> {
        let b = &self.branch_info;
        b.off_default.as_ref().filter(|_| self.main_thread && !self.kept_branches.contains(&b.head))
    }

    /// Says the main thread is not on the default branch, and what fixes it:
    /// a switch when nothing of the branch would be lost, else the branch
    /// moved to a task of its own.
    pub(super) fn render_off_default(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let off = self.off_default()?.clone();
        let head = self.branch_info.head.clone();
        let theme = cx.theme();
        let busy = self.busy.is_some();
        let dirty = !self.changed_paths.is_empty();
        let said = if off.contained {
            format!("The main thread is on {head}, not {}: {} has all of {head}.", off.default, off.base)
        } else {
            format!("The main thread is on {head}, not {}: {head} has work {} lacks.", off.default, off.base)
        };
        let switch = {
            let (off, head) = (off.clone(), head.clone());
            Button::new("off-default-switch")
                .xsmall()
                .when(off.contained, |b| b.primary())
                .when(!off.contained, |b| b.outline())
                .disabled(busy)
                .label(format!("Switch to {}", off.default))
                .tooltip(if off.contained {
                    format!("git switch {}, brought up to {}; {head} is deleted, nothing of it is lost", off.default, off.base)
                } else {
                    format!("git switch {}, brought up to {}; {head} stays, in the list of branches", off.default, off.base)
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    let (off, head) = (off.clone(), head.clone());
                    this.mind_questions(move |this, window, cx| this.run_action("Switch", move |root| git::switch_to_default(&root, &off, &head), false, window, cx), window, cx)
                }))
        };
        let to_task = (!off.contained || dirty).then(|| {
            let (off, head) = (off.clone(), head.clone());
            Button::new("off-default-move")
                .xsmall()
                .when(!off.contained, |b| b.primary())
                .when(off.contained, |b| b.outline())
                .disabled(busy)
                .label(format!("Move {head} to a task"))
                .tooltip(format!("{head} gets a worktree of its own, with what is not committed here, and shows up as a task; the main thread goes to {}", off.default))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    let (off, head) = (off.clone(), head.clone());
                    let copy_env = crate::state::settings().copy_env;
                    this.mind_questions(move |this, window, cx| this.run_action("Move", move |root| git::move_to_task(&root, &off, &head, copy_env).map(|dir| format!("{head} is a task in {}", dir.display())), false, window, cx), window, cx)
                }))
        });
        let keep = Button::new("off-default-keep").xsmall().ghost().label(format!("Keep {head} here")).tooltip("Not told again while the main thread is on this branch").on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.kept_branches.push(head.clone());
            cx.emit(TaskEvent::KeepBranch(head.clone()));
            cx.notify();
        }));
        Some(
            h_flex()
                .id("off-default")
                .flex_none()
                .gap_2()
                .px_3()
                .py_1()
                .text_xs()
                .bg(theme.muted)
                .border_b_1()
                .border_color(theme.border)
                .child(div().flex_1().min_w_0().truncate().child(said))
                .children(to_task)
                .child(switch)
                .child(keep)
                .into_any_element(),
        )
    }

    pub fn set_own_branch(&mut self, own: String, off: Option<String>, cx: &mut Context<Self>) {
        if self.own_branch != own || self.off_own != off {
            self.own_branch = own;
            self.off_own = off;
            cx.notify();
        }
    }

    /// Says the task's worktree is not on the task's branch, and puts it
    /// back: an agent detached it, or went to another branch.
    /// Not while its agent works: a checkout or a rebase of a moment is
    /// its own business, and a button then would undo it under it.
    pub(super) fn render_off_own(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let off = self.off_own.clone().filter(|_| !self.main_thread && !self.agent_busy())?;
        let own = self.own_branch.clone();
        let theme = cx.theme();
        let doing = ["rebasing", "merging", "cherry-picking", "reverting", "bisecting"].iter().any(|d| off.starts_with(d));
        // On a branch of its own: maybe meant, so kept as the task's at a click.
        let other = off.strip_prefix("on ").map(str::to_string);
        let said = if doing {
            format!("The worktree is {off}: finish or abort it, and the task is on {own} again.")
        } else {
            format!("The worktree is {off}; the task's branch is {own}.")
        };
        let keep = other.map(|other| {
            Button::new("off-own-keep")
                .xsmall()
                .ghost()
                .label(format!("Keep {other} as the task's"))
                .tooltip(format!("The task goes on with {other}: its PR, archive and Changes follow it; {own} is left as it is"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(TaskEvent::OwnBranch(other.clone()));
                }))
        });
        let back = (!doing).then(|| {
            let own = own.clone();
            Button::new("off-own-back")
                .xsmall()
                .outline()
                .disabled(self.busy.is_some())
                .label(format!("Back on {own}"))
                .tooltip(format!("With all of {own} here, {own} is moved here; else the worktree goes to {own}, and commits here on no branch get one"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    let own = own.clone();
                    this.mind_questions(move |this, window, cx| this.run_action("Switch", move |root| git::back_on_branch(&root, &own), false, window, cx), window, cx)
                }))
        });
        Some(
            h_flex()
                .id("off-own")
                .flex_none()
                .gap_2()
                .px_3()
                .py_1()
                .text_xs()
                .bg(theme.muted)
                .border_b_1()
                .border_color(theme.border)
                .child(div().flex_1().min_w_0().truncate().child(said))
                .children(keep)
                .children(back)
                .into_any_element(),
        )
    }
}
