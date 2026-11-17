use super::*;

// ---------------------------------------------------------------- terminal

pub struct TermView {
    pub(super) id: SharedString,
    pub state: Entity<TerminalState>,
    /// Its session in the daemon, when the program runs there.
    pub(super) session: Option<String>,
    /// Numbers shells within the task, for their session after a restart.
    pub(super) seq: usize,
    /// The agent's terminal: a program other than the shell has run in it,
    /// so the shell at its prompt means the agent ended.
    pub(super) agent: bool,
    pub(super) ran: bool,
    pub(super) task: Option<WeakEntity<TaskView>>,
    /// The search bar (Ctrl+F), while open.
    pub(super) find: Option<Entity<gpui_kit::component::input::InputState>>,
    /// The `.ideae.toml` service it runs, if any.
    pub service: Option<String>,
}

impl TermView {
    pub(super) fn new(
        id: SharedString,
        state: Entity<TerminalState>,
        session: Option<String>,
        seq: usize,
        task: Option<WeakEntity<TaskView>>,
        service: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        // One size for every terminal: the shared one when this one starts and
        // whenever it changes, and this one's own zoom keys (Ctrl+=) set it.
        Self::follow_zoom(&state, cx);
        cx.observe_global::<crate::zoom::Zoom>(|this, cx| Self::follow_zoom(&this.state, cx)).detach();
        cx.subscribe(&state, |this, state, event: &TerminalEvent, cx| match event {
            TerminalEvent::FontSizeChanged => {
                let size = f32::from(state.read(cx).font_size(cx));
                // The size this view was just given is not one chosen in it: kept
                // as chosen, it would be made larger again, and again.
                let given = f32::from(crate::zoom::size(crate::zoom::Kind::Terminal, cx));
                if (size - given).abs() >= 0.5 {
                    crate::zoom::set(crate::zoom::Kind::Terminal, Some(size / crate::zoom::scale(cx)), cx);
                }
            }
            TerminalEvent::ForegroundChanged => {
                if state.read(cx).foreground().is_some_and(|p| !p.is_shell()) {
                    this.ran = true;
                }
                cx.notify();
            }
            _ => {}
        })
        .detach();
        let agent = task.is_some();
        let ran = state.read(cx).foreground().is_some_and(|p| !p.is_shell());
        Self { id, state, session, seq, agent, ran, task, find: None, service }
    }

    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::InputState;
        let input = match &self.find {
            Some(input) => input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in the terminal"));
                // What is selected is what you would look for.
                if let Some(text) = self.state.read(cx).selected_text().filter(|t| !t.contains('\n')) {
                    input.update(cx, |i, cx| i.set_value(text.clone(), window, cx));
                    self.state.update(cx, |s, cx| s.find(text, cx));
                }
                cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => {
                        let needle = input.read(cx).value().to_string();
                        this.state.update(cx, |s, cx| s.find(needle, cx));
                    }
                    InputEvent::PressEnter { shift, .. } => {
                        let older = !*shift;
                        this.state.update(cx, |s, cx| s.find_next(older, cx));
                    }
                    _ => {}
                })
                .detach();
                self.find = Some(input.clone());
                input
            }
        };
        input.update(cx, |i, cx| i.select_all(window, cx));
        input.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find = None;
        self.state.update(cx, |s, cx| s.find("", cx));
        self.state.read(cx).focus_handle().clone().focus(window, cx);
        cx.notify();
    }

    pub(super) fn render_find(&self, input: Entity<gpui_kit::component::input::InputState>, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx).find_state().cloned();
        let empty = input.read(cx).value().is_empty();
        let count = match state {
            _ if empty => String::new(),
            Some(f) if f.total > 0 => match f.selected {
                // Numbered from the oldest, as they read top to bottom.
                Some(ix) => format!("{} of {}", f.total - ix, f.total),
                None => format!("{} matches", f.total),
            },
            _ => "No matches".into(),
        };
        h_flex()
            .flex_none()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::Escape, window, cx| {
                cx.stop_propagation();
                this.close_find(window, cx);
            }))
            .child(div().w(px(260.)).child(gpui_kit::component::input::Input::new(&input).small()))
            .child(div().min_w(px(80.)).text_xs().text_color(cx.theme().muted_foreground).child(count))
            .child(
                Button::new("find-newer")
                    .xsmall()
                    .ghost()
                    .icon(IconName::ArrowUp)
                    .tooltip("Newer match (Shift+Enter)")
                    .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |s, cx| s.find_next(false, cx)))),
            )
            .child(
                Button::new("find-older")
                    .xsmall()
                    .ghost()
                    .icon(IconName::ArrowDown)
                    .tooltip("Older match (Enter)")
                    .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |s, cx| s.find_next(true, cx)))),
            )
            .child(div().flex_1())
            .child(
                Button::new("find-close")
                    .xsmall()
                    .ghost()
                    .label("×")
                    .tooltip("Close (Esc)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            )
            .into_any_element()
    }

    /// The agent's program is gone: its shell is back at the prompt, or the
    /// terminal itself ended.
    pub fn agent_ended(&self, cx: &App) -> bool {
        if !self.agent {
            return false;
        }
        let state = self.state.read(cx);
        Self::gone(state) || (self.ran && state.foreground().is_some_and(|p| p.is_shell()))
    }

    /// Nothing but the shell runs in the agent's terminal: so too after a
    /// restart of the window that attached to it with the agent long gone,
    /// which `agent_ended` does not tell, not having seen the agent run.
    pub fn agent_at_prompt(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        self.agent && (Self::gone(state) || state.foreground().is_some_and(|p| p.is_shell()))
    }

    /// The terminal's own program, the shell, is no more.
    pub(super) fn gone(state: &TerminalState) -> bool {
        use gpui_cn::terminal::TerminalStatus;
        state.is_closed() || matches!(state.status(), TerminalStatus::Exited(_) | TerminalStatus::Failed(_))
    }

    pub(super) fn render_agent_ended(&self, cx: &mut Context<Self>) -> AnyElement {
        let task = self.task.clone();
        h_flex()
            .flex_none()
            .gap_2()
            .px_3()
            .py_1()
            .text_xs()
            .bg(cx.theme().muted)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().text_color(cx.theme().muted_foreground).child("The agent has ended"))
            .child(
                Button::new("restart-agent")
                    .xsmall()
                    .outline()
                    .label("Restart")
                    .tooltip("Start the agent again, continuing its last conversation")
                    .on_click(move |_, window, cx| {
                        if let Some(task) = task.as_ref().and_then(|t| t.upgrade()) {
                            task.update(cx, |t, cx| t.restart_agent(window, cx));
                        }
                    }),
            )            .into_any_element()
    }

    /// Stops the terminal's program, and with it everything in its session.
    pub fn end(view: &Entity<TermView>, cx: &mut App) {
        let (state, session) = {
            let v = view.read(cx);
            (v.state.clone(), v.session.clone())
        };
        state.update(cx, |s, cx| s.close(cx));
        #[cfg(unix)]
        if let (Some(session), Some(daemon)) = (session, crate::daemon::client::get()) {
            daemon.kill(&session);
        }
        #[cfg(not(unix))]
        let _ = session;
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub(super) fn follow_zoom(state: &Entity<TerminalState>, cx: &mut App) {
        // The theme's size when none is chosen and nothing makes it larger.
        let own = crate::zoom::current(cx).terminal.is_some() || crate::zoom::scale(cx) != 1.;
        let size = crate::zoom::size(crate::zoom::Kind::Terminal, cx);
        state.update(cx, |state, cx| match own {
            true => state.set_font_size(size, cx),
            false => state.reset_font_size(cx),
        });
    }
}

impl Render for TermView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let highlight = cx.theme().primary.opacity(0.15);
        let ended = if self.agent_ended(cx) { Some(self.render_agent_ended(cx)) } else { None };
        let find = match self.find.clone() {
            Some(input) => Some(self.render_find(input, cx)),
            None => None,
        };
        v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &FindInTerminal, window, cx| this.open_find(window, cx)))
            // The menu bar's Edit items are the editor's actions: here they are the terminal's.
            .on_action(|_: &gpui_kit::component::input::Search, window, cx| window.dispatch_action(Box::new(FindInTerminal), cx))
            .on_action(|_: &gpui_kit::component::input::Copy, window, cx| window.dispatch_action(Box::new(gpui_cn::terminal::actions::Copy), cx))
            .on_action(|_: &gpui_kit::component::input::Paste, window, cx| window.dispatch_action(Box::new(gpui_cn::terminal::actions::Paste), cx))
            .on_action(|_: &gpui_kit::component::input::SelectAll, window, cx| window.dispatch_action(Box::new(gpui_cn::terminal::actions::SelectAll), cx))
            .children(ended)
            .children(find)
            .child(
        div()
            .id("terminal-drop")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            // Files dropped from a file manager: their paths, typed in as text.
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(highlight))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                cx.stop_propagation();
                let text = paths.paths().iter().map(|p| shell_quote(&p.display().to_string())).collect::<Vec<_>>().join(" ");
                this.state.update(cx, |s, cx| s.paste_text(format!("{text} "), cx));
                this.state.read(cx).focus_handle().clone().focus(window, cx);
            }))
            .child(crate::zoom::wheel(crate::zoom::Kind::Terminal))
            .child(Terminal::new(self.id.clone(), &self.state)),
        )
    }
}

/// Quoted for a shell only when it needs to be.
pub(super) fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:@,%".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
