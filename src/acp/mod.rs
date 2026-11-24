//! Agents that speak the Agent Client Protocol (agentclientprotocol.com):
//! the conversation shown in a chat, not a terminal. A host process per
//! agent ([`host`]) speaks ACP with it and keeps the thread; the window
//! talks with the host ([`wire`]) through the daemon, or runs it on a thread
//! when there is none ([`link`]).

pub mod checkpoint;
pub mod host;
pub mod link;
pub mod policy;
pub mod spend;
pub mod terminal;
pub mod thread;
pub mod wire;

/// Where a task's conversation is kept, next to the app's state.
pub fn journal_path(root: &std::path::Path) -> std::path::PathBuf {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    crate::state::state_dir().join("acp").join(format!("{name}-{:016x}.jsonl", h.finish()))
}

/// The conversation of one of a task's agents: a question's beside the
/// task's own.
pub fn journal_of(root: &std::path::Path, tag: Option<&str>) -> std::path::PathBuf {
    let journal = journal_path(root);
    match tag {
        Some(tag) => journal.with_extension(format!("{tag}.jsonl")),
        None => journal,
    }
}
#[cfg(test)]
pub mod fake;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::host;
    use super::thread::{Entry, Status, Thread};
    use super::wire::{HostSpec, ToHost, ToWindow};

    struct Window {
        tx: std::sync::mpsc::Sender<host::Event>,
        rx: async_channel::Receiver<ToWindow>,
        thread: Thread,
        heard: Vec<ToWindow>,
    }

    impl Window {
        fn open(spec: HostSpec) -> Self {
            let (tx, rx) = host::spawn_in_process(spec);
            let window = Self { tx, rx, thread: Thread::default(), heard: Vec::new() };
            window.send(ToHost::Attach { version: super::wire::VERSION });
            window
        }

        fn send(&self, msg: ToHost) {
            self.tx.send(host::Event::Window(msg)).unwrap();
        }

        fn until(&mut self, what: &str, done: impl Fn(&Thread, &[ToWindow]) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                while let Ok(msg) = self.rx.try_recv() {
                    match &msg {
                        ToWindow::Hello { thread, .. } => self.thread = (**thread).clone(),
                        ToWindow::Op { op } => self.thread.apply(op.clone()),
                        _ => {}
                    }
                    self.heard.push(msg);
                }
                if done(&self.thread, &self.heard) {
                    return;
                }
                assert!(Instant::now() < deadline, "{what}: {:#?}", self.thread);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// The host with a stand-in agent: a turn, a choice of model, a
    /// permission asked and given, a file written and read through the
    /// window's buffer, a prompt queued behind a slow turn and cancelled;
    /// then a new host goes on from the journal, the agent's replay of the
    /// same conversation not shown twice.
    ///
    /// `cargo test -- --ignored an_acp_agent` after changing the host.
    #[test]
    #[ignore]
    fn an_acp_agent_through_its_host() {
        let dir = std::env::temp_dir().join(format!("ideae-acp-host-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        // A repository: prompts leave checkpoints in it.
        let git = |args: &[&str]| assert!(std::process::Command::new("git").current_dir(&dir).args(args).stdout(std::process::Stdio::null()).status().unwrap().success());
        git(&["init", "-q"]);
        std::fs::write(dir.join(".gitignore"), "journal.jsonl\n").unwrap();
        let spec = HostSpec {
            command: super::fake::command(),
            cwd: dir.clone(),
            env: vec![(super::fake::ENV.into(), "1".into())],
            journal: Some(dir.join("journal.jsonl")),
            ..Default::default()
        };
        let mut w = Window::open(spec.clone());
        w.until("a session", |t, _| t.status == Status::Idle && t.session.as_deref() == Some("fake-1"));
        assert_eq!(w.thread.option("model").map(|o| o.current.as_str()), Some("small"));

        w.send(ToHost::prompt("hi"));
        w.until("an answer", |t, _| t.status == Status::Done);
        assert_eq!(w.thread.last_said(), Some("Hello there."));
        assert!(w.thread.entries.iter().any(|e| matches!(e, Entry::Thought { text } if text == "Thinking.")));
        assert_eq!(w.thread.usage.as_ref().and_then(|u| u.cost.as_deref()), Some("$0.01"));

        w.send(ToHost::Choose { id: "model".into(), value: "big".into() });
        w.until("the model chosen", |t, _| t.option("model").is_some_and(|o| o.current == "big"));

        w.send(ToHost::prompt("please edit"));
        w.until("a question", |t, _| t.status == Status::Asking && t.asking().is_some());
        let request = w.thread.asking().and_then(|t| t.permission.as_ref()).unwrap().request;
        w.send(ToHost::Answer { request, option: Some("yes".into()) });
        w.until("the edit", |t, heard| t.status == Status::Done && heard.iter().any(|m| matches!(m, ToWindow::Wrote { .. })));
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "edited\n");
        let Some(Entry::Tool(tool)) = w.thread.entries.iter().find(|e| matches!(e, Entry::Tool(_))) else { panic!("no tool call") };
        assert_eq!((tool.status.as_str(), tool.decided.as_deref(), tool.diffs.len()), ("completed", Some("Allow"), 1));
        assert_eq!(w.thread.last_said(), Some("Edited."));

        // The files back as before the prompt, and back again.
        let asked = w.thread.entries.iter().rposition(|e| matches!(e, Entry::User { text } if text == "please edit")).unwrap();
        assert!(w.thread.checkpoints.contains_key(&asked), "a checkpoint before the prompt");
        w.send(ToHost::Preview { ix: asked });
        w.until("what would change", |_, heard| heard.iter().any(|m| matches!(m, ToWindow::Restorable { files: Some(f), .. } if f == &[('M', "f.txt".to_string())])));
        w.send(ToHost::Restore { ix: asked });
        w.until("put back", |t, _| t.entries.iter().any(|e| matches!(e, Entry::Notice { text } if text.starts_with("Files put back"))));
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "one\n");
        let notice = w.thread.entries.len() - 1;
        w.until("its undo", |t, _| t.checkpoints.contains_key(&notice));
        w.send(ToHost::Restore { ix: notice });
        w.until("undone", |t, _| t.entries.len() > notice + 1);
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "edited\n");

        w.send(ToHost::prompt("read it"));
        w.until("a read", |_, heard| heard.iter().any(|m| matches!(m, ToWindow::Read { .. })));
        let Some(ToWindow::Read { request, .. }) = w.heard.iter().rev().find(|m| matches!(m, ToWindow::Read { .. })).cloned() else { unreachable!() };
        w.send(ToHost::Buffer { request, text: Some("unsaved\n".into()) });
        w.until("what it read", |t, _| t.status == Status::Done && t.last_said() == Some("It says unsaved."));

        w.send(ToHost::prompt("run it"));
        w.until("a command run through the host", |t, _| t.status == Status::Done && t.last_said() == Some("It printed hello there."));
        let Some(Entry::Tool(run)) = w.thread.entries.iter().rev().find(|e| matches!(e, Entry::Tool(t) if t.terminal.is_some())) else { panic!("no command") };
        assert_eq!(run.output.trim(), "hello there", "its output on its card");

        w.send(ToHost::prompt("slow"));
        w.until("working", |t, _| t.status == Status::Working);
        w.send(ToHost::prompt("then this"));
        w.until("queued", |t, _| t.queued == ["then this"]);
        w.send(ToHost::prompt("and that"));
        w.until("two queued", |t, _| t.queued == ["then this", "and that"]);
        w.send(ToHost::Take { at: 0 });
        w.until("one taken back", |t, _| t.queued == ["and that"]);
        w.send(ToHost::Cancel);
        w.until("cancelled", |t, _| t.status == Status::Done && t.queued.is_empty());
        let before = w.thread.entries.clone();
        w.send(ToHost::Stop);

        let mut w = Window::open(HostSpec { resume: Some("fake-1".into()), ..spec });
        w.until("the session again", |t, _| t.status == Status::Done && t.option("model").is_some());
        assert_eq!(w.thread.entries, before, "the thread from the journal, the replay not added");
        assert!(w.thread.commands.iter().any(|c| c.name == "review"), "its commands, told while it loaded");
        w.send(ToHost::Stop);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The app itself, built next to the tests: `cargo build` first.
    #[cfg(unix)]
    fn app_binary() -> std::path::PathBuf {
        // Above the tests' own folder, however deep cargo keeps that: `target/debug/deps` once, `target/debug/build/<crate>/<hash>/deps` since.
        let exe = std::env::current_exe().unwrap();
        let bin = exe.ancestors().skip(1).map(|above| above.join("ideae")).find(|bin| bin.is_file());
        bin.unwrap_or_else(|| panic!("no ideae above {}: cargo build first", exe.display()))
    }

    /// A chat's agent through an upgrade of the daemon: the real app's
    /// daemon runs the real host, which runs a stand-in agent; a daemon of
    /// another build takes the pipes over, and the same agent, its turn
    /// and its thread go on there.
    ///
    /// `cargo build && cargo test -- --ignored an_acp_agent` after changing the daemon's pipes.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn an_acp_agent_through_a_daemon_upgrade() {
        use crate::daemon::proto::{self, Frame, KIND_BYTES, Msg, Size, Spec};
        use std::os::unix::net::UnixStream;
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        let app = app_binary();
        let dir = std::env::temp_dir().join(format!("ideae-acp-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("state.json");
        // SAFETY: set before anything of the daemon reads it.
        unsafe { std::env::set_var("IDEAE_STATE", &state) };
        let path = proto::socket_path();
        let _ = std::fs::remove_file(&path);
        let mut daemon = std::process::Command::new(&app).arg("--daemon").env("IDEAE_STATE", &state).spawn().unwrap();
        let spec = HostSpec {
            command: super::fake::command(),
            cwd: dir.clone(),
            env: vec![(super::fake::ENV.into(), "1".into())],
            journal: Some(dir.join("journal.jsonl")),
            ..Default::default()
        };
        let program = vec![app.display().to_string(), "--acp-host".to_string()];
        // As the window starts it: the spec on its first line, not in its environment.
        let first = String::from_utf8(super::wire::line(&spec)).unwrap();
        let pipe = Spec { cwd: dir.clone(), env: vec![("IDEAE_TEST_ACP_HOST".into(), "1".into())], input: Some(first), program };
        let size = Size { cols: 80, rows: 24, cell_w: 8, cell_h: 16 };
        let id = "task#acp".to_string();
        let connect = || {
            let mut stream = (0..300)
                .find_map(|_| {
                    std::thread::sleep(Duration::from_millis(20));
                    UnixStream::connect(&path).ok()
                })
                .expect("the daemon listens");
            proto::write_msg(&mut stream, &Msg::Hello { protocol: proto::PROTOCOL, version: "test".into() }).unwrap();
            let Ok(Frame::Msg(Msg::Welcome { sessions, .. })) = proto::read_frame(&mut stream) else { panic!("no welcome") };
            stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            (stream, sessions)
        };
        let attach = |stream: &mut UnixStream| {
            proto::write_msg(stream, &Msg::Open { id: id.clone(), spec: pipe.clone(), size }).unwrap();
            proto::write_bytes(stream, KIND_BYTES, &id, &super::wire::line(&ToHost::Attach { version: super::wire::VERSION })).unwrap();
        };
        let prompt = |stream: &mut UnixStream, text: &str| {
            proto::write_bytes(stream, KIND_BYTES, &id, &super::wire::line(&ToHost::prompt(text))).unwrap();
        };
        let mut lines = super::wire::Lines::default();
        let mut until = |stream: &mut UnixStream, thread: &mut Thread, done: &dyn Fn(&Thread) -> bool| loop {
            if done(thread) {
                return;
            }
            match proto::read_frame(stream) {
                Ok(Frame::Bytes { bytes, .. }) => {
                    for line in lines.feed(&bytes) {
                        match super::wire::parse::<ToWindow>(&line) {
                            Some(ToWindow::Hello { thread: t, .. }) => *thread = *t,
                            Some(ToWindow::Op { op }) => thread.apply(op),
                            _ => {}
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => panic!("{e}: {thread:#?}"),
            }
        };
        let answers = |t: &Thread| t.entries.iter().filter(|e| matches!(e, Entry::Agent { .. })).count();

        let (mut stream, _) = connect();
        attach(&mut stream);
        let mut thread = Thread::default();
        until(&mut stream, &mut thread, &|t| t.status == Status::Idle);
        prompt(&mut stream, "hi");
        until(&mut stream, &mut thread, &|t| t.status == Status::Done);

        // A handover that cannot happen is told, and the daemon serves on.
        proto::write_msg(&mut stream, &Msg::Upgrade { exe: dir.join("no-such-build") }).unwrap();
        loop {
            match proto::read_frame(&mut stream) {
                Ok(Frame::Msg(Msg::Stays)) => break,
                Ok(_) => {}
                Err(e) => panic!("the daemon left: {e}"),
            }
        }
        proto::write_msg(&mut stream, &Msg::Upgrade { exe: app.clone() }).unwrap();
        // The old daemon leaves once the new one has the sessions.
        while proto::read_frame(&mut stream).is_ok() {}
        let _ = daemon.wait();
        let (mut stream, sessions) = connect();
        assert_eq!(sessions, [id.clone()], "the new daemon has the agent's host");
        attach(&mut stream);
        let mut again = Thread::default();
        until(&mut stream, &mut again, &|t| t.status == Status::Done && answers(t) == 1);
        prompt(&mut stream, "hi again");
        until(&mut stream, &mut again, &|t| t.status == Status::Done && answers(t) == 2);
        assert_eq!(again.session.as_deref(), Some("fake-1"), "the same agent, not a new session");
        let _ = proto::write_msg(&mut stream, &Msg::Shutdown);
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The app's rules answer what is safe: an edit inside the worktree is
    /// allowed and the card says by whom; asked again once "Ask me" is
    /// chosen.
    ///
    /// `cargo test -- --ignored an_acp_agent` after changing the rules.
    #[test]
    #[ignore]
    fn an_acp_agent_answered_by_the_rules() {
        let dir = std::env::temp_dir().join(format!("ideae-acp-rules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        let spec = HostSpec { command: super::fake::command(), cwd: dir.clone(), env: vec![(super::fake::ENV.into(), "1".into())], policy: "rules".into(), ..Default::default() };
        let mut w = Window::open(spec.clone());
        w.until("a session", |t, _| t.status == Status::Idle);
        w.send(ToHost::prompt("please edit"));
        w.until("the edit, unasked", |t, _| t.status == Status::Done && t.last_said() == Some("Edited."));
        let Some(Entry::Tool(tool)) = w.thread.entries.iter().find(|e| matches!(e, Entry::Tool(_))) else { panic!("no tool call") };
        assert_eq!(tool.decided.as_deref(), Some("Allowed by ideae: inside the worktree"));
        w.send(ToHost::Policy { policy: "ask".into() });
        w.send(ToHost::prompt("edit again"));
        w.until("asked", |t, _| t.status == Status::Asking);
        w.send(ToHost::Stop);

        // Nobody answers, the window gone: declined in the time set.
        let mut w = Window::open(HostSpec { policy: "ask".into(), unanswered_secs: 1, ..spec.clone() });
        w.until("a session", |t, _| matches!(t.status, Status::Idle | Status::Done));
        w.send(ToHost::Detached);
        w.send(ToHost::prompt("please edit"));
        w.until("declined", |t, _| t.status == Status::Done && t.last_said() == Some("Not edited."));
        assert!(w.thread.entries.iter().any(|e| matches!(e, Entry::Tool(t) if t.decided.as_deref() == Some("Declined: no answer in 1 s"))));
        w.send(ToHost::Stop);

        // A daily limit: the turn that spends it stops, the next is not sent.
        let mut w = Window::open(HostSpec { budget: Some(0.005), ..spec.clone() });
        w.until("a session", |t, _| matches!(t.status, Status::Idle | Status::Done));
        w.send(ToHost::prompt("hi"));
        w.until("stopped", |t, _| t.entries.iter().any(|e| matches!(e, Entry::Notice { text } if text.contains("daily limit is reached"))));
        w.send(ToHost::prompt("hi again"));
        w.until("not sent", |t, _| t.entries.iter().any(|e| matches!(e, Entry::Notice { text } if text.contains("limit is spent"))));
        w.send(ToHost::Stop);
        let mut w = Window::open(HostSpec { budget: Some(0.0), journal: None, ..spec });
        w.until("a session", |t, _| matches!(t.status, Status::Idle | Status::Done));
        w.send(ToHost::prompt("hi"));
        w.until("spent before it began", |t, _| t.entries.iter().any(|e| matches!(e, Entry::Notice { text } if text.contains("limit is spent"))));
        w.send(ToHost::Stop);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A long answer in thousands of pieces comes through as few changes.
    #[test]
    #[ignore]
    fn an_acp_agent_streams_a_long_answer() {
        let dir = std::env::temp_dir().join(format!("ideae-acp-long-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = HostSpec { command: super::fake::command(), cwd: dir.clone(), env: vec![(super::fake::ENV.into(), "1".into())], journal: Some(dir.join("j.jsonl")), ..Default::default() };
        let mut w = Window::open(spec);
        w.until("a session", |t, _| t.status == Status::Idle);
        let started = Instant::now();
        w.send(ToHost::prompt("long"));
        w.until("the answer", |t, _| t.status == Status::Done);
        let ops = w.heard.iter().filter(|m| matches!(m, ToWindow::Op { op: super::thread::Op::Append { .. } })).count();
        eprintln!("3000 pieces: {:?}, {ops} appends", started.elapsed());
        assert!(ops < 300, "{ops} changes for one answer");
        assert!(started.elapsed() < Duration::from_secs(5));
        w.send(ToHost::Stop);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An agent that ends in the middle of a turn is started again once and
    /// goes on with its session; one that is not there says what it needs.
    #[test]
    #[ignore]
    fn an_acp_agent_that_ends_mid_turn() {
        let dir = std::env::temp_dir().join(format!("ideae-acp-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = HostSpec { command: super::fake::command(), cwd: dir.clone(), env: vec![(super::fake::ENV.into(), "1".into())], journal: Some(dir.join("j.jsonl")), ..Default::default() };
        let mut w = Window::open(spec.clone());
        w.until("a session", |t, _| t.status == Status::Idle);
        w.send(ToHost::prompt("hi"));
        w.until("an answer", |t, _| t.status == Status::Done);
        w.send(ToHost::prompt("crash"));
        w.until("started again", |t, _| t.status == Status::Done && t.entries.iter().any(|e| matches!(e, Entry::Notice { text } if text.contains("started again"))));
        let notice = w.thread.entries.iter().rev().find_map(|e| match e { Entry::Notice { text } => Some(text.clone()), _ => None }).unwrap();
        assert!(notice.contains("out of memory"), "{notice}");
        // Its error stream, whole, in its log: the user's alone.
        let log = host::log_path(&dir.join("j.jsonl"));
        assert!(std::fs::read_to_string(&log).unwrap().contains("fake: out of memory"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
        }
        w.send(ToHost::prompt("hi"));
        w.until("an answer after", |t, _| t.status == Status::Done && t.last_said() == Some("Hello there."));
        w.send(ToHost::prompt("crash"));
        w.send(ToHost::prompt("crash"));
        // Twice in a minute: it stays ended, and says why.
        w.until("ended", |t, _| matches!(&t.status, Status::Exited(Some(why)) if why.contains("out of memory")));
        // A prompt now goes nowhere, and says so, not "working" for ever.
        w.send(ToHost::prompt("are you there"));
        w.until("told", |t, _| t.entries.iter().any(|e| matches!(e, Entry::Notice { text } if text.contains("not sent"))));
        assert!(matches!(w.thread.status, Status::Exited(_)));
        w.send(ToHost::Stop);

        let mut w = Window::open(HostSpec { command: "no-such-agent-here --acp".into(), journal: None, ..spec });
        w.until("not found", |t, _| matches!(&t.status, Status::Exited(Some(why)) if why.contains("`no-such-agent-here` was not found")));
        w.send(ToHost::Stop);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A chat asked to go on with a session it never had in its journal (a
    /// terminal agent's, carried on): loaded, its conversation replayed;
    /// one it cannot load starts anew, told where the work stands.
    #[test]
    #[ignore]
    fn an_acp_agent_goes_on_with_another_session() {
        let dir = std::env::temp_dir().join(format!("ideae-acp-carry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = HostSpec { command: super::fake::command(), cwd: dir.clone(), env: vec![(super::fake::ENV.into(), "1".into())], journal: Some(dir.join("j.jsonl")), resume: Some("from-terminal".into()), ..Default::default() };
        let mut w = Window::open(spec.clone());
        w.send(ToHost::Otherwise { prompt: "You carry on a task".into() });
        w.until("the session loaded", |t, _| t.status == Status::Done && t.last_said() == Some("replayed"));
        assert!(!w.thread.entries.iter().any(|e| matches!(e, Entry::User { text } if text.contains("carry on"))), "no handover when it remembers");
        w.send(ToHost::Stop);

        let _ = std::fs::remove_file(dir.join("j.jsonl"));
        let mut w = Window::open(HostSpec { resume: Some("gone".into()), ..spec });
        w.send(ToHost::Otherwise { prompt: "You carry on a task".into() });
        w.until("told instead", |t, _| t.status == Status::Done && t.entries.iter().any(|e| matches!(e, Entry::User { text } if text == "You carry on a task")));
        assert_eq!(w.thread.last_said(), Some("Hello there."));
        w.send(ToHost::Stop);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An agent run by npx whose download an earlier start cut short: npm
    /// fails on it at every start; the host clears that copy and starts it
    /// again, once.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn an_acp_agent_whose_npx_copy_was_cut_short() {
        let dir = std::env::temp_dir().join(format!("ideae-acp-npx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let broken = dir.join("cache/_npx/0123abcd");
        std::fs::create_dir_all(broken.join("node_modules")).unwrap();
        // npx as npm is then: the error while the copy is there, the agent once it is gone.
        let script = dir.join("npx.sh");
        std::fs::write(
            &script,
            format!(
                "if [ -d '{b}' ]; then echo \"npm error enoent Could not read package.json: Error: ENOENT: no such file or directory, open '{b}/package.json'\" >&2; exit 254; fi\nexec {agent}\n",
                b = broken.display(),
                agent = super::fake::command()
            ),
        )
        .unwrap();
        let spec = HostSpec { command: format!("sh {}", script.display()), cwd: dir.clone(), env: vec![(super::fake::ENV.into(), "1".into())], ..Default::default() };
        let mut w = Window::open(spec);
        w.until("started after the copy was cleared", |t, _| t.status == Status::Idle);
        assert!(!broken.exists(), "the copy cut short is gone");
        assert!(w.thread.entries.iter().any(|e| matches!(e, Entry::Notice { text } if text.contains("cut short"))));
        w.send(ToHost::prompt("hi"));
        w.until("an answer", |t, _| t.last_said() == Some("Hello there."));
        w.send(ToHost::Stop);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An agent that ends itself with a command of its own: told so, not by
    /// its last debug lines, and started again with its session.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn an_acp_agent_that_ends_itself() {
        let dir = std::env::temp_dir().join(format!("ideae-acp-self-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = HostSpec { command: super::fake::command(), cwd: dir.clone(), env: vec![(super::fake::ENV.into(), "1".into())], journal: Some(dir.join("j.jsonl")), ..Default::default() };
        let mut w = Window::open(spec);
        w.until("a session", |t, _| t.status == Status::Idle);
        w.send(ToHost::prompt("hi"));
        w.until("an answer", |t, _| t.status == Status::Done);
        w.send(ToHost::prompt("clean up"));
        w.until("started again", |t, _| t.status == Status::Done && t.entries.iter().any(|e| matches!(e, Entry::Notice { text } if text.contains("started again"))));
        let notice = w.thread.entries.iter().rev().find_map(|e| match e { Entry::Notice { text } => Some(text.clone()), _ => None }).unwrap();
        assert!(notice.contains("by its own command `kill"), "{notice}");
        w.send(ToHost::prompt("hi"));
        w.until("it answers again", |t, _| t.last_said() == Some("Hello there."));
        w.send(ToHost::Stop);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Many tool calls and a command that prints a lot: few changes, a
    /// journal that does not grow with every look at the output.
    #[test]
    #[ignore]
    fn an_acp_agent_under_load() {
        let dir = std::env::temp_dir().join(format!("ideae-acp-load-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let journal = dir.join("j.jsonl");
        let spec = HostSpec { command: super::fake::command(), cwd: dir.clone(), env: vec![(super::fake::ENV.into(), "1".into())], journal: Some(journal.clone()), ..Default::default() };
        let mut w = Window::open(spec);
        w.until("a session", |t, _| t.status == Status::Idle);
        let started = Instant::now();
        w.send(ToHost::prompt("tools"));
        w.until("the tools", |t, _| t.status == Status::Done);
        let tools = w.thread.entries.iter().filter(|e| matches!(e, Entry::Tool(_))).count();
        eprintln!("300 tool calls: {:?}", started.elapsed());
        assert_eq!(tools, 300);
        assert!(started.elapsed() < Duration::from_secs(5));
        let started = Instant::now();
        w.send(ToHost::prompt("noisy"));
        w.until("the noisy command", |t, _| t.status == Status::Done && t.last_said() == Some("Noisy done."));
        let Some(Entry::Tool(noisy)) = w.thread.entries.iter().rev().find(|e| matches!(e, Entry::Tool(t) if t.terminal.is_some())) else { panic!("no command") };
        assert!(noisy.output.trim_end().ends_with("20000"), "its last output shown");
        let shown = w.heard.iter().filter(|m| matches!(m, ToWindow::Op { op: super::thread::Op::Set { .. } })).count();
        let size = std::fs::metadata(&journal).unwrap().len();
        eprintln!("noisy command: {:?}, {shown} changes, journal {size} bytes", started.elapsed());
        assert!(size < 2 * super::host::JOURNAL_MOST, "{size} bytes of journal");
        w.send(ToHost::Stop);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The host as a process of its own, when the daemon runs it.
    #[test]
    #[ignore]
    fn host_process() {
        if std::env::var(host::SPEC_ENV).is_ok() || std::env::var("IDEAE_TEST_ACP_HOST").is_ok() {
            host::run_process();
        }
    }

    /// The host in a real daemon, a process of its own: a window attaches,
    /// prompts, goes away; another attaches and is told the whole thread,
    /// the agent having run on meanwhile.
    ///
    /// `cargo test -- --ignored an_acp_agent` after changing the daemon's pipes.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn an_acp_agent_in_the_daemon_outlives_its_window() {
        use crate::daemon::proto::{self, Frame, KIND_BYTES, Msg, Size, Spec};
        use std::os::unix::net::UnixStream;
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ideae-acp-daemon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        // SAFETY: set before anything of the daemon reads it.
        unsafe { std::env::set_var("IDEAE_STATE", dir.join("state.json")) };
        std::thread::spawn(|| crate::daemon::server::run(false));
        let path = proto::socket_path();
        let spec = HostSpec {
            command: super::fake::command(),
            cwd: dir.clone(),
            env: vec![(super::fake::ENV.into(), "1".into())],
            journal: Some(dir.join("journal.jsonl")),
            ..Default::default()
        };
        let exe = std::env::current_exe().unwrap().display().to_string();
        let program = [exe.as_str(), "--exact", "acp::tests::host_process", "--ignored", "--nocapture", "--test-threads=1"].map(String::from).to_vec();
        // As the window starts it: the spec on its first line, not in its environment.
        let first = String::from_utf8(super::wire::line(&spec)).unwrap();
        let pipe = Spec { cwd: dir.clone(), env: vec![("IDEAE_TEST_ACP_HOST".into(), "1".into())], input: Some(first), program };
        let size = Size { cols: 80, rows: 24, cell_w: 8, cell_h: 16 };
        let id = "task#acp".to_string();

        let attach = |pipe: &Spec| {
            let mut stream = (0..200)
                .find_map(|_| {
                    std::thread::sleep(Duration::from_millis(20));
                    UnixStream::connect(&path).ok()
                })
                .expect("the daemon listens");
            proto::write_msg(&mut stream, &Msg::Hello { protocol: proto::PROTOCOL, version: crate::build_watch::build_id() }).unwrap();
            assert!(matches!(proto::read_frame(&mut stream), Ok(Frame::Msg(Msg::Welcome { .. }))));
            proto::write_msg(&mut stream, &Msg::Open { id: id.clone(), spec: pipe.clone(), size }).unwrap();
            proto::write_bytes(&mut stream, KIND_BYTES, &id, &super::wire::line(&ToHost::Attach { version: super::wire::VERSION })).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            stream
        };
        let mut lines = super::wire::Lines::default();
        let mut until = |stream: &mut UnixStream, thread: &mut Thread, done: &dyn Fn(&Thread) -> bool| loop {
            if done(thread) {
                return;
            }
            match proto::read_frame(stream) {
                Ok(Frame::Bytes { bytes, .. }) => {
                    for line in lines.feed(&bytes) {
                        match super::wire::parse::<ToWindow>(&line) {
                            Some(ToWindow::Hello { thread: t, .. }) => *thread = *t,
                            Some(ToWindow::Op { op }) => thread.apply(op),
                            _ => {}
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => panic!("{e}: {thread:#?}"),
            }
        };

        let mut stream = attach(&pipe);
        let mut thread = Thread::default();
        until(&mut stream, &mut thread, &|t| t.status == Status::Idle);
        proto::write_bytes(&mut stream, KIND_BYTES, &id, &super::wire::line(&ToHost::prompt("hi"))).unwrap();
        until(&mut stream, &mut thread, &|t| t.status == Status::Working);
        drop(stream);

        // Another window: the turn ended while none was there.
        std::thread::sleep(Duration::from_millis(300));
        let mut stream = attach(&pipe);
        let mut again = Thread::default();
        until(&mut stream, &mut again, &|t| t.status == Status::Done && t.last_said() == Some("Hello there."));
        // A command left running in the background, then the session ended:
        // the agent and all it started end with the host.
        proto::write_bytes(&mut stream, KIND_BYTES, &id, &super::wire::line(&ToHost::prompt("serve"))).unwrap();
        until(&mut stream, &mut again, &|t| t.last_said() == Some("Serving."));
        let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
        let pid_in = |name: &str| std::fs::read_to_string(dir.join(name)).ok().and_then(|p| p.trim().parse::<i32>().ok());
        let (agent, server) = (pid_in("agent.pid").expect("the agent's pid"), pid_in("bg.pid").expect("the server's pid"));
        assert!(alive(agent) && alive(server));
        proto::write_msg(&mut stream, &Msg::Kill { id: id.clone() }).unwrap();
        let gone = (0..60).any(|_| {
            std::thread::sleep(Duration::from_millis(100));
            !alive(agent) && !alive(server)
        });
        assert!(gone, "the agent ({}) and its server ({}) ended with the host", alive(agent), alive(server));
        let mut check = UnixStream::connect(&path).unwrap();
        proto::write_msg(&mut check, &Msg::Hello { protocol: proto::PROTOCOL, version: crate::build_watch::build_id() }).unwrap();
        let Ok(Frame::Msg(Msg::Welcome { sessions, .. })) = proto::read_frame(&mut check) else { panic!("no welcome") };
        assert!(sessions.is_empty(), "the host ended with its session: {sessions:?}");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
