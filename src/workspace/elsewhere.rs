//! What another window changed in this machine's store, taken into this
//! one's model (crate::machine): a task renamed or starred there, one made
//! or archived there, a project added there. Only what changed is taken,
//! field by field: what this window changed meanwhile and has not yet
//! brought to the store stays its own.
//!
//! An open task's work (review notes, viewed files, bookmarks) is given to
//! its view.

use super::*;
use ideae_core::store::Machine;

impl Workspace {
    /// Takes in what other windows change in the store, as they do.
    pub(super) fn follow_store(window: &mut Window, cx: &mut Context<Self>) {
        let Some(changed) = crate::machine::changes_elsewhere() else { return };
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(first) = changed.recv().await {
                // A burst of changes is fetched once a store.
                let mut stores = std::collections::BTreeSet::from([first]);
                while let Ok(more) = changed.try_recv() {
                    stores.insert(more);
                }
                for store in stores {
                    // One fetched before this window's own sync went in is fetched again.
                    for _ in 0..5 {
                        let again = match &store {
                            None => {
                                let Some(machine) = cx.background_spawn(async { crate::machine::fetch() }).await else { break };
                                this.update_in(cx, |this, window, cx| this.take_in_store(machine, window, cx))
                            }
                            #[cfg(unix)]
                            Some(host) => {
                                let asked = host.clone();
                                let Some(machine) = cx.background_spawn(async move { crate::machine::fetch_remote(&asked) }).await else { break };
                                let host = host.clone();
                                this.update_in(cx, |this, window, cx| this.take_in_host(&host, machine, false, window, cx))
                            }
                            #[cfg(not(unix))]
                            Some(_) => break,
                        };
                        match again {
                            Ok(true) => continue,
                            Ok(false) => break,
                            Err(_) => return,
                        }
                    }
                }
            }
        })
        .detach();
    }

    /// Whether what was fetched is older than what this window brought since: fetch again.
    pub(super) fn take_in_store(&mut self, machine: Machine, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match crate::machine::take_in(machine.clone()) {
            crate::machine::Fetched::Changes(changes) => self.take_in_changes(changes, machine, window, cx),
            crate::machine::Fetched::Stale => true,
            crate::machine::Fetched::Nothing => false,
        }
    }

    /// Another machine's store, fetched: what of it the projects of `host`
    /// this window shows have not yet. `first`: just connected, from what
    /// the window has of them (no more than their tabs).
    #[cfg(unix)]
    pub(super) fn take_in_host(&mut self, host: &str, machine: Machine, first: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let shown: Vec<PathBuf> = self.projects.iter().filter(|p| crate::hosts::split(&p.path).is_some_and(|(h, _)| h == host)).map(|p| p.path.clone()).collect();
        let first = first.then(|| crate::machine::of_host(&self.snapshot(cx), host));
        let again = match crate::machine::take_in_host(host, machine.clone(), &shown, first) {
            crate::machine::Fetched::Changes(changes) => self.take_in_changes(changes, machine, window, cx),
            crate::machine::Fetched::Stale => true,
            crate::machine::Fetched::Nothing => false,
        };
        self.find_more_elsewhere(host);
        cx.notify();
        again
    }

    /// What `host`'s store has that this window does not show, for its row.
    #[cfg(unix)]
    pub(super) fn find_more_elsewhere(&mut self, host: &str) {
        let more: Vec<(PathBuf, String)> = crate::machine::projects_there(host)
            .into_iter()
            .filter(|p| !self.projects.iter().any(|known| known.path == p.path))
            .map(|p| {
                let name = p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.path.display().to_string());
                (p.path, name)
            })
            .collect();
        if more.is_empty() {
            self.more_elsewhere.remove(host);
        } else {
            self.more_elsewhere.insert(host.to_string(), more);
        }
    }

    /// Projects of `host`'s this window did not show, shown now: as its store has them.
    #[cfg(unix)]
    pub fn show_elsewhere(&mut self, host: &str, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let there = Machine { projects: crate::machine::projects_there(host).into_iter().filter(|p| paths.contains(&p.path)).collect(), ..Default::default() };
        let mut saved = State::default();
        crate::machine::overlay(&mut saved, &there);
        for p in &saved.projects {
            if !self.projects.iter().any(|known| known.path == p.path) {
                let mut project = self.project_of_saved(p);
                // Shown in the space in the window, as a project added there is.
                project.space = self.space.clone();
                self.projects.push(project);
            }
        }
        self.find_more_elsewhere(host);
        self.resolve_projects(cx);
        // Their worktrees, as made there by anyone.
        self.sync_worktrees(window, cx);
        self.save_later(cx);
        cx.notify();
    }

    fn take_in_changes(&mut self, changes: ideae_core::store::sync::Changes, machine: Machine, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Everything as the store has it, in the saved form the model is made of.
        let mut from = State::default();
        crate::machine::overlay(&mut from, &machine);
        let saved_project = |path: &Path| from.projects.iter().find(|p| p.path == path);
        let saved_task = |path: &Path| from.projects.iter().flat_map(|p| &p.tasks).find(|t| t.path == path);

        // Projects removed elsewhere: out at once, their views ended; the task in view, if it was one of theirs, the next one of another.
        let was_active = self.active.clone();
        for path in &changes.projects_removed {
            if let Some(pi) = self.projects.iter().position(|p| &p.path == path) {
                let gone = self.projects.remove(pi);
                for view in gone.tasks.into_iter().filter_map(|t| t.view) {
                    view.update(cx, |t, cx| t.shut_down(cx));
                }
            }
        }
        if was_active.as_ref().is_some_and(|a| self.find(a).is_none()) {
            self.active = None;
            let next = self.projects.iter().flat_map(|p| &p.tasks).filter(|t| t.view.is_some()).max_by_key(|t| t.last_active).map(|t| t.path.clone());
            if let Some(next) = next {
                self.switch(&next, window, cx);
            }
        }
        for p in &changes.projects_added {
            if self.projects.iter().any(|known| known.path == p.path) {
                continue;
            }
            if let Some(saved) = saved_project(&p.path) {
                let project = self.project_of_saved(saved);
                self.projects.push(project);
            }
        }
        for (path, fields) in &changes.projects {
            let (Some(saved), Some(pi)) = (saved_project(path), self.projects.iter().position(|p| &p.path == path)) else { continue };
            let project = &mut self.projects[pi];
            for field in fields.keys() {
                match field.as_str() {
                    "check" => project.check = saved.check.clone(),
                    "check_auto" => project.check_auto = saved.check_auto,
                    "settings" => project.settings = saved.settings.clone(),
                    "trusted" => project.trusted = saved.trusted.clone(),
                    "hints_off" => project.hints_off = saved.hints_off.iter().cloned().collect(),
                    "made_by_agents" => project.made_by_agents = saved.made_by_agents.clone(),
                    "own" => crate::project_config::set_own(path, saved.own.clone()),
                    "verdicts" => {
                        for (group, verdict) in &saved.verdicts {
                            self.judges.insert(group.clone(), verdict.clone());
                            self.judged.insert(group.clone());
                        }
                    }
                    "stale_verdicts" => self.stale_verdicts.extend(saved.stale_verdicts.iter().cloned()),
                    _ => {}
                }
            }
        }
        for path in &changes.tasks_removed {
            // One being made here is this window's yet.
            if self.find(path).is_some_and(|(pi, ti)| self.projects[pi].tasks[ti].busy.is_none()) {
                self.gone_elsewhere(path, window, cx);
            }
        }
        for (project, task) in &changes.tasks_added {
            let Some(pi) = self.projects.iter().position(|p| &p.path == project) else { continue };
            if self.find(&task.path).is_some() {
                continue;
            }
            if let Some(saved) = saved_task(&task.path) {
                self.projects[pi].tasks.push(TaskEntry::of_saved(saved));
            }
        }
        for (path, fields) in &changes.tasks {
            let Some((pi, ti)) = self.find(path) else { continue };
            // Its saved form as this window has it, the fields changed elsewhere set over it.
            let mine = self.task_state(&self.projects[pi].tasks[ti], cx);
            let Ok(serde_json::Value::Object(mut json)) = serde_json::to_value(&mine) else { continue };
            for (k, v) in fields {
                match (k.as_str(), v) {
                    // Its work, a part of its session.
                    ("work", work) => {
                        let session = json.entry("session").or_insert_with(|| serde_json::json!({}));
                        for part in ["review", "viewed", "bookmarks"] {
                            session[part] = work.get(part).cloned().unwrap_or(serde_json::json!(null));
                        }
                    }
                    ("check", serde_json::Value::Null) => {
                        self.checks.remove(path);
                    }
                    ("check", run) => {
                        if let Ok(run) = serde_json::from_value(run.clone()) {
                            self.checks.insert(path.clone(), run);
                        }
                    }
                    (_, serde_json::Value::Null) => {
                        json.remove(k);
                    }
                    (_, v) => {
                        json.insert(k.clone(), v.clone());
                    }
                }
            }
            // Nulls of the session's parts: their defaults.
            if let Some(serde_json::Value::Object(session)) = json.get_mut("session") {
                session.retain(|_, v| !v.is_null());
            }
            match serde_json::from_value::<state::TaskState>(serde_json::Value::Object(json)) {
                Ok(theirs) => {
                    // An open task's work is its view's.
                    if fields.contains_key("work") {
                        if let Some(view) = self.projects[pi].tasks[ti].view.clone() {
                            let s = &theirs.session;
                            let (review, viewed, bookmarks) = (s.review.clone(), s.viewed.clone(), s.bookmarks.clone());
                            view.update(cx, |t, cx| t.take_work(review, viewed, bookmarks, cx));
                        }
                    }
                    self.projects[pi].tasks[ti].take_saved(&theirs);
                }
                Err(e) => eprintln!("ideae: a task changed elsewhere not taken in: {e}"),
            }
        }
        for (project, order) in &changes.orders {
            let Some(p) = self.projects.iter_mut().find(|p| &p.path == project) else { continue };
            // Those it does not list (being made here) after.
            p.tasks.sort_by_key(|t| order.iter().position(|o| *o == t.path).unwrap_or(usize::MAX));
        }
        if changes.crew {
            self.crew.take_in(&machine.crew, &changes.crew_before);
        }
        if changes.settings || changes.profiles {
            let mut settings = state::settings();
            let mut fields = serde_json::Map::new();
            if let Some(machine) = machine.settings.as_object().filter(|_| changes.settings) {
                fields.extend(machine.clone());
            }
            if changes.profiles {
                fields.insert("agents".into(), serde_json::Value::Array(machine.profiles.clone()));
            }
            match settings.take_machine(&fields) {
                Ok(()) => state::set_settings(settings),
                Err(e) => eprintln!("ideae: the machine's settings changed elsewhere not taken in: {e}"),
            }
        }
        self.resolve_projects(cx);
        self.save_later(cx);
        cx.notify();
        false
    }

    /// A task archived or deleted in another window: out of this one, its
    /// view ended (its teardown ran there).
    fn gone_elsewhere(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pi, ti)) = self.find(path) else { return };
        if let Some(view) = self.projects[pi].tasks[ti].view.take() {
            view.update(cx, |t, cx| t.shut_down(cx));
        }
        self.projects[pi].tasks.remove(ti);
        if self.active.as_deref() == Some(path) {
            self.active = None;
            let next = self.projects.iter().flat_map(|p| &p.tasks).filter(|t| t.view.is_some()).max_by_key(|t| t.last_active).map(|t| t.path.clone());
            if let Some(next) = next {
                self.switch(&next, window, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Workspace;
    use crate::state::{self, ProjectState, State};
    use ideae_core::store::{Op, Task};
    use std::path::Path;

    /// A start that finds an older build's state.json and a store another
    /// window changed since: the window opens (the store's tasks) and says so.
    #[gpui_kit::test]
    fn a_start_that_finds_both_changed_opens_and_tells(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::{AppContext as _, Bounds, Point, WindowBounds, WindowOptions, px, size};
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ideae-both-changed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: set before anything of the app reads them, under the lock the tests that set them share.
        unsafe {
            std::env::set_var("IDEAE_STATE", dir.join("state.json"));
            std::env::set_var("IDEAE_NO_DAEMON", "1");
        }
        let p = dir.join("p");
        let main = state::TaskState { path: p.clone(), branch: "main".into(), main: true, title: "Older's".into(), ..Default::default() };
        let saved = State { projects: vec![ProjectState { path: p.clone(), tasks: vec![main], ..Default::default() }], ..Default::default() };
        std::fs::write(dir.join("state.json"), serde_json::to_string(&saved).unwrap()).unwrap();
        let mut machine = ideae_core::store::migrate::from_state(&serde_json::to_value(&saved).unwrap()).unwrap();
        machine.projects[0].tasks[0].title = "Another window's".into();
        (machine.rev, machine.mirrored) = (5, 4);
        ideae_core::store::file::save(&dir, &machine).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_cn::init(cx);
            crate::theme::init(cx);
        });
        let (_handle, workspace) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1300.), px(800.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(Vec::new(), Some(None), None, window, cx))).expect("a test window")
        });
        cx.run_until_parked();
        workspace.update(cx, |w, _| assert_eq!(w.projects[0].tasks[0].title, "Another window's", "the store's"));
        assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("state.json.not-merged-")));
        // SAFETY: as above.
        unsafe {
            std::env::remove_var("IDEAE_STATE");
            std::env::remove_var("IDEAE_NO_DAEMON");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Another window renames, stars, adds and archives tasks: this one
    /// shows it, and its own change after takes none of it back.
    #[gpui_kit::test]
    fn what_another_window_changed_shows_here(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::{AppContext as _, Bounds, Point, WindowBounds, WindowOptions, px, size};
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ideae-elsewhere-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: set before anything of the app reads them, under the lock the tests that set them share.
        unsafe {
            std::env::set_var("IDEAE_STATE", dir.join("state.json"));
            std::env::set_var("IDEAE_NO_DAEMON", "1");
        }
        let p = dir.join("p");
        let task = |name: &str, slot: u32| state::TaskState { path: dir.join(name), branch: name.into(), title: name.to_uppercase(), port_slot: slot, ..Default::default() };
        let main = state::TaskState { path: p.clone(), branch: "main".into(), main: true, ..Default::default() };
        let saved = State { projects: vec![ProjectState { path: p.clone(), tasks: vec![main, task("a", 1), task("b", 2)], ..Default::default() }], ..Default::default() };
        std::fs::write(dir.join("state.json"), serde_json::to_string(&saved).unwrap()).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_cn::init(cx);
            crate::theme::init(cx);
        });
        let (handle, workspace) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1300.), px(800.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(Vec::new(), Some(None), None, window, cx))).expect("a test window")
        });
        cx.run_until_parked();
        let at = |name: &str| dir.join(name);
        let (a, b) = (at("a"), at("b"));
        // DIAG (Windows CI): everything the window and the store hold before the other window's change.
        eprintln!("DIAG temp_dir={:?} dir={:?} a={:?} exists(dir)={} TEMP={:?}", std::env::temp_dir(), dir, a, dir.exists(), std::env::var("TEMP"));
        eprintln!("DIAG canonical dir={:?}", std::fs::canonicalize(&dir));
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            eprintln!("DIAG file {:?} {:?}", entry.path(), entry.metadata().map(|m| m.len()));
        }
        eprintln!("DIAG state.json {}", std::fs::read_to_string(dir.join("state.json")).unwrap_or_default());
        eprintln!("DIAG machine.json {}", std::fs::read_to_string(dir.join("machine.json")).unwrap_or_default());
        workspace.read_with(cx, |w, _| {
            for p in &w.projects {
                eprintln!("DIAG model project {:?}: {:?}", p.path, p.tasks.iter().map(|t| (&t.path, &t.branch, &t.title, t.main, t.busy.as_deref())).collect::<Vec<_>>());
            }
        });
        crate::machine::as_another_window(|store| {
            eprintln!("DIAG store {}", serde_json::to_string(&store.machine).unwrap_or_default());
        });
        // The other window: a renamed and given a review note, b archived, c made.
        crate::machine::as_another_window(|store| {
            let id = |store: &ideae_core::store::Store, path: &Path| store.task_at(path).unwrap_or_else(|| panic!("DIAG no task at {path:?}")).id;
            let fields = |v: serde_json::Value| v.as_object().unwrap().clone();
            let (ia, ib) = (id(store, &a), id(store, &b));
            store.apply(2, Op::SetTask { id: ia, fields: fields(serde_json::json!({ "title": "A by two", "work": { "review": [{ "text": "look", "line": 1 }] } })) }).unwrap();
            store.apply(2, Op::DeleteTask { id: ib }).unwrap();
            let c = Task { path: at("c"), branch: "c".into(), title: "C".into(), ..Default::default() };
            store.apply(2, Op::CreateTask { project: p.clone(), task: c, after: Some(ia) }).unwrap();
        });
        cx.update_window(handle.into(), |_, window, cx| {
            workspace.update(cx, |w, cx| {
                let machine = crate::machine::fetch().unwrap();
                assert!(!w.take_in_store(machine, window, cx));
            })
        })
        .unwrap();
        cx.run_until_parked();
        workspace.update(cx, |w, _| {
            let titles: Vec<_> = w.projects[0].tasks.iter().filter(|t| !t.main).map(|t| t.title.clone()).collect();
            assert_eq!(titles, ["A by two", "C"], "renamed, archived and made elsewhere, in the store's order");
            let a = w.projects[0].tasks.iter().find(|t| t.path == at("a")).unwrap();
            assert_eq!(a.session.review.first().map(|n| n.text.as_str()), Some("look"));
        });
        // This window's own change: the other's stay.
        workspace.update(cx, |w, cx| {
            w.entry_mut(&at("a")).unwrap().note = "mine".into();
            w.save(cx);
        });
        let machine = crate::machine::fetch().unwrap();
        let a = machine.projects[0].tasks.iter().find(|t| t.path == at("a")).unwrap();
        assert_eq!((a.title.as_str(), a.note.as_str()), ("A by two", "mine"));
        assert!(machine.projects[0].tasks.iter().all(|t| t.path != at("b")), "not brought back");
        assert!(machine.projects[0].tasks.iter().any(|t| t.path == at("c")));
        // Another change there, and this window saves before it has fetched it: it takes it back no more.
        crate::machine::as_another_window(|store| {
            let id = store.task_at(&at("a")).unwrap().id;
            store.apply(2, Op::SetTask { id, fields: serde_json::json!({ "title": "A again by two" }).as_object().unwrap().clone() }).unwrap();
        });
        workspace.update(cx, |w, cx| {
            w.entry_mut(&at("a")).unwrap().starred = true;
            w.save(cx);
        });
        let machine = crate::machine::fetch().unwrap();
        let a = machine.projects[0].tasks.iter().find(|t| t.path == at("a")).unwrap();
        assert_eq!((a.title.as_str(), a.starred), ("A again by two", true), "its own change in, the other's kept");
        // A store fetched before this window's own sync is not taken in: it would take that back.
        let old = crate::machine::fetch().unwrap();
        workspace.update(cx, |w, cx| {
            w.entry_mut(&at("a")).unwrap().note = "newer".into();
            w.save(cx);
        });
        cx.update_window(handle.into(), |_, window, cx| workspace.update(cx, |w, cx| assert!(w.take_in_store(old, window, cx), "stale: fetched again"))).unwrap();
        workspace.update(cx, |w, _| assert_eq!(w.entry_mut(&at("a")).unwrap().note, "newer"));
        // A task busy without a view (a remote one connecting) is left out of the snapshot, not deleted.
        workspace.update(cx, |w, cx| {
            w.entry_mut(&at("c")).unwrap().busy = Some("connecting…".into());
            w.save(cx);
        });
        assert!(crate::machine::fetch().unwrap().projects[0].tasks.iter().any(|t| t.path == at("c")), "kept while busy");
        workspace.update(cx, |w, _| w.entry_mut(&at("c")).unwrap().busy = None);
        // A task this window made with a slot another has: the store gives another, and the window takes it in.
        crate::machine::as_another_window(|store| {
            let taken = Task { path: at("d"), branch: "d".into(), port_slot: 7, ..Default::default() };
            store.apply(2, Op::CreateTask { project: p.clone(), task: taken, after: None }).unwrap();
        });
        workspace.update(cx, |w, cx| {
            let mut e = super::super::TaskEntry::of_saved(&state::TaskState { path: at("e"), branch: "e".into(), port_slot: 7, ..Default::default() });
            e.title = "E".into();
            w.projects[0].tasks.push(e);
            w.save(cx);
        });
        for _ in 0..50 {
            cx.run_until_parked();
            if workspace.update(cx, |w, _| w.entry_mut(&at("e")).is_some_and(|e| e.port_slot != 7)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        workspace.update(cx, |w, _| {
            assert_ne!(w.entry_mut(&at("e")).unwrap().port_slot, 7, "the store's slot, not one another task has");
            assert!(w.entry_mut(&at("d")).is_some(), "and the other's task with it");
        });
        // SAFETY: as above.
        unsafe {
            std::env::remove_var("IDEAE_STATE");
            std::env::remove_var("IDEAE_NO_DAEMON");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
