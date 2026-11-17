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
                                this.navigate(true, window, cx)
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
                                this.navigate(false, window, cx)
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
                        .tooltip(format!("Changes, Files, Structure, Run  {}", crate::keymap::label("changes_panel")))
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.side_over = !this.side_over;
                            cx.notify();
                        })),
                )
            })
            .child(
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
            .when_some(b.upstream.clone(), |this, up| {
                this.child(div().text_color(cx.theme().muted_foreground).child(format!("→ {up}")))
                    .when(b.ahead > 0, |this| this.child(div().text_color(gpui_kit::green()).child(format!("↑{}", b.ahead))))
                    .when(b.behind > 0, |this| this.child(div().text_color(gpui_kit::red()).child(format!("↓{}", b.behind))))
            })
            .when(b.upstream.is_none() && !b.head.is_empty(), |this| {
                this.child(div().text_color(cx.theme().muted_foreground).child("no upstream"))
            })
            .when_some(b.base.clone().filter(|_| b.behind_base > 0), |this, base| {
                let busy = self.busy.is_some();
                this.child(
                    Button::new("rebase")
                        .xsmall()
                        .ghost()
                        .disabled(busy)
                        .label(format!("{base} ↓{} · Rebase", b.behind_base))
                        .tooltip(format!(
                            "Rebase this branch onto {base}. On a conflict nothing changes, and the agent's prompt gets a request to do it."
                        ))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.rebase_onto_base(base.clone(), window, cx)
                        })),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .cursor_pointer()
                    .child(format!("· {} {}", b.last_hash, b.last_subject)),
            )
            .children(self.render_auto_badges(cx))
            .children(self.render_pr_feedback(cx))
            .child(
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
                            this.git_command(GitCommand::Pull, window, cx)
                        })),
                )
            })
            // The branch's last commit, in History.
            .on_click(cx.listener(|this, _, window, cx| {
                let hash = this.branch_info.last_hash.clone();
                this.show_commit(&hash, window, cx)
            }))
    }
}
