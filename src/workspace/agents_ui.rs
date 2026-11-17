//! Choosing agents: which one a task, a variant or a project's new tasks
//! run, and a task carried on by another agent. Nothing of it shows while
//! there is one agent profile.

use std::{cell::RefCell, path::Path, rc::Rc};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    v_flex,
};
use gpui_kit::*;

use super::Workspace;
use crate::{agents, state};

/// A button with the chosen agent's name; its menu has the others.
pub fn picker(id: impl Into<ElementId>, chosen: Rc<RefCell<String>>) -> impl IntoElement {
    let profiles = state::with_settings(|s| s.agents.clone());
    let name = profiles.iter().find(|p| p.id == *chosen.borrow()).map_or_else(|| "Agent".to_string(), |p| p.name.clone());
    Button::new(id).xsmall().outline().label(format!("{name} ▾")).tooltip("The agent that runs it").dropdown_menu(move |mut menu, _, _| {
        for profile in &profiles {
            let (id, chosen) = (profile.id.clone(), chosen.clone());
            let checked = *chosen.borrow() == id;
            menu = menu.item(PopupMenuItem::new(profile.name.clone()).checked(checked).on_click(move |_, window, _| {
                *chosen.borrow_mut() = id.clone();
                window.refresh();
            }));
        }
        menu
    })
}

/// What the agent that carries a task on is told first: the task, what
/// the one before said last, and where to look for what was done.
pub fn handoff(title: &str, prompt: &str, before: &str, said: Option<&str>, base: Option<&str>) -> String {
    let mut text = format!(
        "You are continuing a task that another agent ({before}) started in this worktree; it has stopped. Its work so far is in this branch: read `git log` and `git diff {}` (and `git status` for what is not committed) to see what was done before you go on.\n\nThe task: {title}\n",
        base.unwrap_or("the branch's base")
    );
    if !prompt.trim().is_empty() && prompt.trim() != title.trim() {
        text.push_str(&format!("\nAs it was given:\n<task>\n{}\n</task>\n", prompt.trim()));
    }
    if let Some(said) = said.map(str::trim).filter(|s| !s.is_empty()) {
        text.push_str(&format!("\nWhat {before} said last:\n<previous>\n{said}\n</previous>\n"));
    }
    text.push_str("\nCarry on from where it stopped; say first, in a few lines, what you found done and what is left.");
    text
}

impl Workspace {
    /// The agent new tasks of a project run, by profile id: its own
    /// choice, else the default.
    pub fn project_agent(&self, pi: usize) -> String {
        let own = self.projects.get(pi).and_then(|p| p.agent.clone());
        state::with_settings(|s| own.filter(|id| s.profile(id).is_some()).or_else(|| s.default_profile().map(|p| p.id.clone())).unwrap_or_default())
    }

    pub fn set_project_agent(&mut self, pi: usize, agent: Option<String>, cx: &mut Context<Self>) {
        if let Some(p) = self.projects.get_mut(pi) {
            p.agent = agent;
            self.save_later(cx);
            cx.notify();
        }
    }

    /// Which agent a task runs now.
    pub fn task_profile(&self, path: &Path, cx: &App) -> Option<agents::Profile> {
        let (pi, ti) = self.find(path)?;
        self.task_profile_at(pi, ti, cx)
    }

    /// As `task_profile`, for a row the list has in hand.
    pub fn task_profile_at(&self, pi: usize, ti: usize, cx: &App) -> Option<agents::Profile> {
        let t = self.projects.get(pi)?.tasks.get(ti)?;
        if let Some(launch) = t.view.as_ref().and_then(|v| v.read(cx).launch.clone()) {
            return Some(launch.profile);
        }
        let last = t.sessions.last().map(|s| s.profile.clone()).unwrap_or_default();
        let project = self.projects[pi].agent.clone();
        state::with_settings(|s| agents::resolve(&s.agents, &[&last, &t.agent], project.as_deref(), &s.default_agent).cloned())
    }

    /// Asks which agent carries the task on.
    pub fn continue_with_dialog(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.task_profile(path, cx);
        let others: Vec<agents::Profile> = state::with_settings(|s| s.agents.iter().filter(|p| Some(&p.id) != current.as_ref().map(|c| &c.id)).cloned().collect());
        if others.is_empty() {
            return;
        }
        let chosen = Rc::new(RefCell::new(others[0].id.clone()));
        let (this, path) = (cx.weak_entity(), path.to_path_buf());
        let now = current.map_or_else(|| "No agent".to_string(), |c| c.name);
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let (chosen, this, path, now) = (chosen.clone(), this.clone(), path.clone(), now.clone());
            let picked = chosen.clone();
            dialog
                .title("Continue with another agent")
                .w(px(560.))
                .content(move |content, _, _| {
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(h_flex().gap_2().items_center().child(div().text_sm().child(format!("{now} stops; carrying on:"))).child(picker("continue-with", chosen.clone())))
                            .child(div().text_xs().text_color(muted).child(
                                "The new agent starts a session of its own in the same worktree. It is told the task, what the one before said last, and that git log and git diff show what was done. The earlier session stays recorded with the task.",
                            )),
                    )
                })
                .footer(h_flex().justify_end().child(Button::new("continue-with-start").primary().label("Continue").on_click(move |_, window, cx| {
                    let id = picked.borrow().clone();
                    window.close_dialog(cx);
                    let _ = this.update(cx, |w, cx| w.continue_with(&path, &id, window, cx));
                })))
        });
    }

    /// Stops the task's agent and starts the profile `id` in its place,
    /// in a new session, told where the work stands.
    pub fn continue_with(&mut self, path: &Path, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = state::with_settings(|s| s.profile(id).cloned()) else { return };
        let before = self.task_profile(path, cx);
        let Some((pi, ti)) = self.find(path) else { return };
        let t = &self.projects[pi].tasks[ti];
        let (title, prompt, base) = (t.label(), t.prompt.clone(), Some(t.base.clone()).filter(|b| !b.is_empty()));
        let said = t.view.as_ref().and_then(|v| v.read(cx).agent_said.as_ref().map(|s| s.whole.to_string()));
        let who = t.session_ref();
        let unasked = !t.group.is_empty();
        let before_name = before.map_or_else(|| "another agent".to_string(), |p| p.name);
        let root = path.to_path_buf();
        cx.spawn_in(window, async move |this, cx| {
            // Its last words: as heard, else from its session's file.
            let said = match said {
                Some(said) => Some(said),
                None => {
                    let root = root.clone();
                    cx.background_spawn(async move { who.and_then(|who| who.last_answer(&root)) }).await
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let text = handoff(&title, &prompt, &before_name, said.as_deref(), base.as_deref());
                let Some(entry) = this.find(&root).map(|(pi, ti)| &mut this.projects[pi].tasks[ti]) else { return };
                let session = profile.adapter().chooses_session_id().then(agents::uuid);
                entry.sessions.push(state::AgentSession { profile: profile.id.clone(), id: session.clone(), started: state::now() });
                let launch = agents::Launch { profile: profile.clone(), start: agents::Start::New(session), unasked };
                let view = match entry.view.clone() {
                    Some(view) => view,
                    None => {
                        // Not running: it starts with the new agent.
                        entry.fresh = true;
                        match this.load(&root, window, cx) {
                            Some(view) => view,
                            None => return,
                        }
                    }
                };
                let fresh = view.read(cx).launch.as_ref() != Some(&launch);
                view.update(cx, |t, cx| {
                    if fresh {
                        t.switch_agent(launch, window, cx);
                    }
                    t.queue_prompt(text, true, window, cx);
                });
                this.switch(&root, window, cx);
                window.push_notification(Notification::info(format!("{} carries the task on.", profile.name)), cx);
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::Workspace;
    use crate::state::{ProjectState, State};
    #[test]
    fn the_next_agent_is_told_where_the_work_stands() {
        let text = super::handoff("Fix login", "Fix the login form: it drops the password.", "Claude Code", Some("Fixed the form; tests still fail."), Some("abc123"));
        for part in ["another agent (Claude Code)", "git diff abc123", "The task: Fix login", "<task>\nFix the login form", "<previous>\nFixed the form; tests still fail.\n</previous>"] {
            assert!(text.contains(part), "{part} in {text}");
        }
        let bare = super::handoff("Fix login", "Fix login", "Codex", None, None);
        assert!(!bare.contains("<task>") && !bare.contains("<previous>") && bare.contains("git diff the branch's base"), "{bare}");
    }

    /// Two variants of a task with two agents, on a real repository with
    /// real terminals: A runs a stand-in for Claude Code (it writes down
    /// how it was started), B any other program (a stand-in that writes
    /// for a while, rings the bell and reads what it is given). A starts
    /// under a session id the app chose and resumes it; B's status is
    /// guessed from its terminal; A is then carried on by B's agent, told
    /// where the work stands.
    ///
    /// `cargo test -- --ignored two_agents` after changing agents.
    #[cfg(unix)]
    #[gpui_kit::test]
    #[ignore]
    fn two_agents_in_two_variants_and_one_carried_on(cx: &mut gpui_kit::TestAppContext) {
        use crate::agent::{AgentStatus, HookEvent};
        use crate::agents::{Kind, Profile};
        use gpui_kit::{AppContext as _, Bounds, Point, WindowBounds, WindowOptions, px, size};
        use std::os::unix::fs::PermissionsExt as _;
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("ideae-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let sh = |at: &std::path::Path, script: &str| {
            assert!(std::process::Command::new("sh").arg("-ec").arg(script).current_dir(at).status().unwrap().success(), "{script}");
        };
        sh(&dir, "git init -q -b main repo; cd repo; git config user.email t@t; git config user.name t; echo one > f.txt; git add f.txt; git commit -qm base");
        let repo = dir.join("repo").canonicalize().unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.join("bin").join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        // Claude Code's stand-in: how it was started, then it waits.
        let claude = script("claude", "echo \"$@\" > \"$PWD/.claude-args\"\nexec sleep 60");
        // Another agent: its account, then a few lines of work, the bell, and what it is told.
        let other = script("other", "echo \"$OTHER_ACCOUNT\" > \"$PWD/.other-env\"\nfor i in 1 2 3 4 5 6; do echo working $i; sleep 0.5; done\nprintf 'done\\a\\n'\nexec cat > \"$PWD/.other-told\"");

        // SAFETY: set before anything of the app reads them.
        unsafe {
            std::env::set_var("IDEAE_STATE", dir.join("state.json"));
            std::env::set_var("IDEAE_NO_DAEMON", "1");
        }
        let saved = State { projects: vec![ProjectState { path: repo.clone(), ..Default::default() }], ..Default::default() };
        std::fs::write(dir.join("state.json"), serde_json::to_string(&saved).unwrap()).unwrap();
        let mut settings = crate::state::Settings { notifications: false, pr_status: false, ..Default::default() };
        settings.agents = vec![
            Profile { command: claude.display().to_string(), ..Profile::claude_code() },
            Profile { id: "other".into(), name: "Other".into(), kind: Kind::Generic, command: other.display().to_string(), env: [("OTHER_ACCOUNT".to_string(), "second".to_string())].into() },
        ];
        crate::state::set_settings(settings);
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_cn::init(cx);
            crate::theme::init(cx);
        });
        let (handle, workspace) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1300.), px(800.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(Vec::new(), None, None, window, cx))).expect("a test window")
        });
        let step = |cx: &mut gpui_kit::TestAppContext, act: &mut dyn FnMut(&mut Workspace, &mut gpui_kit::Window, &mut gpui_kit::Context<Workspace>)| {
            cx.update_window(handle.into(), |_, window, cx| workspace.update(cx, |w, cx| act(w, window, cx))).unwrap();
            cx.run_until_parked();
        };
        let wait = |cx: &mut gpui_kit::TestAppContext, what: &str, done: &mut dyn FnMut(&mut gpui_kit::TestAppContext) -> bool| {
            for _ in 0..1500 {
                cx.run_until_parked();
                if done(cx) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                // The app's timers keep the time the test does.
                cx.executor().advance_clock(std::time::Duration::from_millis(10));
            }
            panic!("waited in vain for {what}");
        };

        step(cx, &mut |w, window, cx| w.create_task(0, "Fix login".into(), "Make the file.".into(), vec![String::new(), String::new()], vec!["claude".into(), "other".into()], window, cx));
        let paths: Vec<std::path::PathBuf> = workspace.update(cx, |w, _| w.projects[0].work().iter().map(|t| t.path.clone()).collect());
        assert_eq!(paths.len(), 2);
        let (a, b) = (paths[0].clone(), paths[1].clone());
        wait(cx, "both agents to start", &mut |_| a.join(".claude-args").is_file() && b.join(".other-env").is_file());

        // A: Claude Code under the id the app chose, unasked as variants are, with the hooks.
        let args = std::fs::read_to_string(a.join(".claude-args")).unwrap();
        let id = workspace.update(cx, |w, _| {
            let t = &w.projects[0].work()[0];
            assert_eq!((t.agent.as_str(), t.sessions.len()), ("claude", 1));
            t.sessions[0].id.clone().expect("an id chosen")
        });
        assert!(args.contains(&format!("--session-id {id}")) && args.contains("--permission-mode auto") && args.contains("--settings"), "{args}");
        // B: the other program, with its profile's environment.
        assert_eq!(std::fs::read_to_string(b.join(".other-env")).unwrap().trim(), "second");
        workspace.update(cx, |w, _| {
            let t = &w.projects[0].work()[1];
            assert_eq!((t.sessions[0].profile.as_str(), t.sessions[0].id.as_deref()), ("other", None));
        });

        // Claude Code says it runs another session (a /clear): a restart resumes that one.
        step(cx, &mut |w, window, cx| {
            let event = HookEvent { task: a.clone(), event: "SessionStart".into(), message: None, kind: None, file: None, said: None, replayed: None, session: Some("after-clear".into()), guessed: false };
            w.on_hook(event, window, cx);
        });
        workspace.update(cx, |w, cx| {
            let t = &w.projects[0].work()[0];
            assert_eq!(t.sessions[0].id.as_deref(), Some("after-clear"));
            let again = t.view.as_ref().unwrap().read(cx).launch.as_ref().unwrap().again().command();
            assert!(again.contains("--resume after-clear"), "{again}");
            assert_eq!(w.snapshot(cx).projects[0].tasks.iter().find(|t| !t.main).unwrap().sessions[0].id.as_deref(), Some("after-clear"), "kept in the state");
        });

        // B tells nothing: it is seen to work while it writes, and to be done at the bell.
        wait(cx, "B guessed at work", &mut |cx| workspace.update(cx, |w, cx| w.projects[0].work()[1].view.as_ref().unwrap().read(cx).agent_status == AgentStatus::Working));
        wait(cx, "B guessed done", &mut |cx| {
            workspace.update(cx, |w, cx| {
                let v = w.projects[0].work()[1].view.as_ref().unwrap().read(cx);
                v.agent_status == AgentStatus::Done && v.status_guessed
            })
        });

        // The marks: each task's agent, by its letter.
        cx.update_window(handle.into(), |_, window, cx| {
            use gpui_kit::test::TestWindowExt as _;
            window.render_frame(cx);
            assert!(window.try_find(gpui_kit::SharedString::from(format!("agent-mark-{}", a.display()))).is_some() || window.try_find(gpui_kit::SharedString::from(format!("agent-mark-{}", b.display()))).is_some(), "an agent's letter on a row");
        })
        .unwrap();

        // A carried on by the other agent: told the task and where to look.
        step(cx, &mut |w, window, cx| w.continue_with(&a, "other", window, cx));
        workspace.update(cx, |w, _| {
            let t = &w.projects[0].work()[0];
            assert_eq!(t.sessions.iter().map(|s| s.profile.as_str()).collect::<Vec<_>>(), ["claude", "other"], "the earlier session stays recorded");
        });
        wait(cx, "the handoff to reach the new agent", &mut |_| std::fs::read_to_string(a.join(".other-told")).is_ok_and(|told| told.contains("Make the file.") && told.contains("git log")));
        assert_eq!(std::fs::read_to_string(a.join(".other-env")).unwrap().trim(), "second", "the new agent in its own environment");

        // What the tasks started ends with them.
        step(cx, &mut |w, _, cx| {
            for view in w.views() {
                view.update(cx, |t, cx| t.shut_down_at_quit(cx));
            }
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
