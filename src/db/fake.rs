//! Stand-ins for the end-to-end test: a MySQL server speaking as much of the
//! protocol as a session uses, and an `ssh` (this test binary, run again)
//! that forwards a port to it.
//!
//! What the server does goes by the statement: `SELECT rows N` sends N rows,
//! `SELECT sleep` waits to be killed, `UPDATE…` changes three rows, and what
//! it does not know is a syntax error.

use std::{
    collections::HashSet,
    io::{BufRead as _, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

const VERSION: &str = "8.0.31-fake";
const TYPE_DECIMAL: u8 = 0xf6;
const TYPE_LONGLONG: u8 = 0x08;
const TYPE_STRING: u8 = 0xfd;
const UTF8: u16 = 45;

const ARGS_ENV: &str = "IDEAE_FAKE_SSH_ARGS";
const PID_ENV: &str = "IDEAE_FAKE_SSH_PID";

type Killed = Arc<Mutex<HashSet<u32>>>;

/// Starts the server; gives its port.
fn serve() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let killed: Killed = Arc::default();
    let ids = Arc::new(AtomicU32::new(1));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (killed, id) = (killed.clone(), ids.fetch_add(1, Ordering::Relaxed));
            std::thread::spawn(move || {
                let _ = Client { stream, seq: 0, id, killed }.run();
            });
        }
    });
    port
}

struct Client {
    stream: TcpStream,
    seq: u8,
    id: u32,
    killed: Killed,
}

fn lenenc(out: &mut Vec<u8>, text: &[u8]) {
    assert!(text.len() < 251);
    out.push(text.len() as u8);
    out.extend_from_slice(text);
}

impl Client {
    fn read(&mut self) -> std::io::Result<Vec<u8>> {
        let mut head = [0u8; 4];
        self.stream.read_exact(&mut head)?;
        self.seq = head[3].wrapping_add(1);
        let mut body = vec![0; usize::from(head[0]) | usize::from(head[1]) << 8 | usize::from(head[2]) << 16];
        self.stream.read_exact(&mut body)?;
        Ok(body)
    }

    fn send(&mut self, body: &[u8]) -> std::io::Result<()> {
        let n = body.len();
        self.stream.write_all(&[n as u8, (n >> 8) as u8, (n >> 16) as u8, self.seq])?;
        self.seq = self.seq.wrapping_add(1);
        self.stream.write_all(body)
    }

    fn ok(&mut self, affected: u8) -> std::io::Result<()> {
        self.send(&[0x00, affected, 0, 2, 0, 0, 0])
    }

    fn eof(&mut self) -> std::io::Result<()> {
        self.send(&[0xfe, 0, 0, 2, 0])
    }

    fn err(&mut self, code: u16, message: &str) -> std::io::Result<()> {
        let mut body = vec![0xff, code as u8, (code >> 8) as u8, b'#'];
        body.extend_from_slice(b"42000");
        body.extend_from_slice(message.as_bytes());
        self.send(&body)
    }

    fn handshake(&mut self) -> std::io::Result<()> {
        let caps: u32 = 0x1 | 0x4 | 0x8 | 0x200 | 0x2000 | 0x8000 | 0x1_0000 | 0x2_0000 | 0x8_0000;
        let mut body = vec![10];
        body.extend_from_slice(VERSION.as_bytes());
        body.push(0);
        body.extend_from_slice(&self.id.to_le_bytes());
        body.extend_from_slice(b"12345678\0");
        body.extend_from_slice(&(caps as u16).to_le_bytes());
        body.push(UTF8 as u8);
        body.extend_from_slice(&2u16.to_le_bytes());
        body.extend_from_slice(&((caps >> 16) as u16).to_le_bytes());
        body.push(21);
        body.extend_from_slice(&[0; 10]);
        body.extend_from_slice(b"abcdefghijkl\0");
        body.extend_from_slice(b"mysql_native_password\0");
        self.send(&body)?;
        self.read()?;
        self.ok(0)
    }

    fn columns(&mut self, columns: &[(&str, u8)]) -> std::io::Result<()> {
        self.send(&[columns.len() as u8])?;
        for (name, kind) in columns {
            let mut body = Vec::new();
            for part in ["def", "", "", "", name, name] {
                lenenc(&mut body, part.as_bytes());
            }
            body.push(0x0c);
            body.extend_from_slice(&UTF8.to_le_bytes());
            body.extend_from_slice(&255u32.to_le_bytes());
            body.push(*kind);
            body.extend_from_slice(&[0, 0, 0, 0, 0]);
            self.send(&body)?;
        }
        self.eof()
    }

    fn row(&mut self, cells: &[Option<&str>]) -> std::io::Result<()> {
        let mut body = Vec::new();
        for cell in cells {
            match cell {
                Some(text) => lenenc(&mut body, text.as_bytes()),
                None => body.push(0xfb),
            }
        }
        self.send(&body)
    }

    fn one_column(&mut self, name: &str, values: &[&str]) -> std::io::Result<()> {
        self.columns(&[(name, TYPE_STRING)])?;
        for value in values {
            self.row(&[Some(value)])?;
        }
        self.eof()
    }

    fn was_killed(&self) -> bool {
        self.killed.lock().unwrap().remove(&self.id)
    }

    fn query(&mut self, sql: &str) -> std::io::Result<()> {
        let upper = sql.to_ascii_uppercase();
        if upper.starts_with("SELECT @@") {
            return self.one_column("value", &["67108864"]);
        }
        if upper == "SELECT VERSION()" {
            return self.one_column("version", &[VERSION]);
        }
        if upper == "SHOW DATABASES" {
            return self.one_column("Database", &["app", "other"]);
        }
        if let Some(id) = upper.strip_prefix("KILL QUERY ") {
            self.killed.lock().unwrap().insert(id.trim().parse().unwrap_or(0));
            return self.ok(0);
        }
        if upper.starts_with("UPDATE") {
            return self.ok(3);
        }
        if upper == "SELECT SLEEP" {
            let began = Instant::now();
            while began.elapsed() < Duration::from_secs(15) {
                if self.was_killed() {
                    return self.err(1317, "Query execution was interrupted");
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            return self.err(1064, "was never killed");
        }
        if let Some(n) = upper.strip_prefix("SELECT ROWS ") {
            let n: u64 = n.trim().parse().unwrap_or(0);
            self.columns(&[("id", TYPE_LONGLONG), ("amount", TYPE_DECIMAL), ("note", TYPE_STRING)])?;
            for i in 0..n {
                if self.was_killed() {
                    return self.err(1317, "Query execution was interrupted");
                }
                let id = (9_007_199_254_740_992 + i).to_string();
                let note = (i % 2 == 0).then_some("");
                self.row(&[Some(&id), Some("10.50"), note])?;
            }
            return self.eof();
        }
        self.err(1064, "You have an error in your SQL syntax")
    }

    fn run(mut self) -> std::io::Result<()> {
        self.handshake()?;
        loop {
            let packet = self.read()?;
            match packet.first() {
                Some(0x01) | None => return Ok(()),
                Some(0x03) => {
                    self.killed.lock().unwrap().remove(&self.id);
                    let sql = String::from_utf8_lossy(&packet[1..]).into_owned();
                    self.query(sql.trim())?;
                }
                _ => self.ok(0)?,
            }
        }
    }
}

/// The stand-in `ssh`: `-L 127.0.0.1:LOCAL:HOST:PORT`, and it says so as ssh does.
fn be_ssh(args: &str) {
    let words: Vec<&str> = args.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "-L").unwrap();
    let parts: Vec<&str> = words[at + 1].split(':').collect();
    let (local, host, port) = (parts[1].parse::<u16>().unwrap(), parts[2].to_string(), parts[3].parse::<u16>().unwrap());
    if let Ok(file) = std::env::var(PID_ENV) {
        std::fs::write(file, std::process::id().to_string()).unwrap();
    }
    let listener = TcpListener::bind(("127.0.0.1", local)).unwrap();
    eprintln!("debug1: Local forwarding listening on 127.0.0.1 port {local}.");
    for inbound in listener.incoming().flatten() {
        let outbound = TcpStream::connect((host.as_str(), port)).unwrap();
        for (mut from, mut to) in [(inbound.try_clone().unwrap(), outbound.try_clone().unwrap()), (outbound, inbound)] {
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut from, &mut to);
                let _ = to.shutdown(std::net::Shutdown::Write);
            });
        }
    }
}

mod tests {
    use std::ops::Range;

    use super::super::{
        config::{Connection, SshHop},
        session::{Event, MOST_ROWS, Session},
        tunnel,
        value::Cell,
    };
    use super::*;

    #[test]
    #[ignore]
    fn ssh_stand_in() {
        if let Ok(args) = std::env::var(ARGS_ENV) {
            be_ssh(&args);
        }
    }

    /// A server to look at the app against: `IDEAE_FAKE_MYSQL=<file>` gets its port.
    #[test]
    #[ignore]
    fn mysql_stand_in() {
        if let Ok(file) = std::env::var("IDEAE_FAKE_MYSQL") {
            std::fs::write(file, serve().to_string()).unwrap();
            std::thread::park();
        }
    }

    /// A shell script, as the arguments of a real ssh would not do for a test binary.
    fn write_ssh(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let script = dir.join("ssh");
        let exe = std::env::current_exe().unwrap();
        let pid = dir.join("ssh.pid");
        let body = format!(
            "#!/bin/sh\n{ARGS_ENV}=\"$*\" {PID_ENV}='{}' exec '{}' --exact db::fake::tests::ssh_stand_in --ignored --nocapture --test-threads=1\n",
            pid.display(),
            exe.display()
        );
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn statement(sql: &str) -> Vec<(Range<usize>, String)> {
        vec![(0..sql.len(), sql.to_string())]
    }

    fn until(events: &async_channel::Receiver<Event>, mut wanted: impl FnMut(&Event) -> bool) -> Event {
        loop {
            let event = events.recv_blocking().expect("the session ended");
            if wanted(&event) {
                return event;
            }
        }
    }

    /// Rows read up to the end of the run, whether the last result was cut, what failed.
    fn run(session: &Session, events: &async_channel::Receiver<Event>, sql: &str) -> (Vec<Box<[Cell]>>, bool, u64, Option<String>) {
        session.run(statement(sql));
        let (mut rows, mut cut, mut affected, mut failed) = (Vec::new(), false, 0, None);
        loop {
            match events.recv_blocking().expect("the session ended") {
                Event::Rows(batch) => rows.extend(batch),
                Event::ResultDone { cut: c, affected: a, .. } => (cut, affected) = (c, a),
                Event::Failed { message, .. } => failed = Some(message),
                Event::Idle { .. } => return (rows, cut, affected, failed),
                _ => {}
            }
        }
    }

    #[test]
    fn the_database_panel_lists_a_connection_s_databases() {
        let connection = Connection {
            id: "panel".into(),
            name: "Direct".into(),
            driver: super::super::config::Driver::MySql,
            host: "127.0.0.1".into(),
            port: serve(),
            user: "me".into(),
            database: None,
            ssh: None,
            bastion: None,
            tls: Default::default(),
            color: None,
            production: false,
        };
        assert_eq!(super::super::browse::databases(&connection, Some("secret".into())), Ok(vec!["app".to_string(), "other".to_string()]));

        // Production, locked: nothing connects to it, whatever the password.
        let _one = super::super::guard::TESTS.lock().unwrap_or_else(|e| e.into_inner());
        super::super::guard::lock();
        let production = Connection { production: true, ..connection };
        assert_eq!(super::super::browse::databases(&production, Some("secret".into())), Err(super::super::guard::LOCKED.to_string()));
        let (_session, events) = Session::start(production.clone(), Some("secret".into()), None);
        assert!(matches!(until(&events, |_| true), Event::Lost(said) if said == super::super::guard::LOCKED));

        // Locked again while connected: the statement is refused, the session stays.
        super::super::guard::unlock_for(Duration::from_secs(60));
        let (session, events) = Session::start(production, Some("secret".into()), None);
        assert!(matches!(until(&events, |e| matches!(e, Event::Connected { .. } | Event::Lost(_))), Event::Connected { .. }));
        super::super::guard::lock();
        let (rows, _, _, failed) = run(&session, &events, "SELECT rows 2");
        assert!(rows.is_empty() && failed.is_some_and(|f| f.starts_with(super::super::guard::LOCKED)));
        super::super::guard::unlock_for(Duration::from_secs(60));
        let (rows, _, _, failed) = run(&session, &events, "SELECT rows 2");
        assert_eq!((rows.len(), failed), (2, None));
        super::super::guard::lock();
    }

    #[test]
    #[ignore]
    fn a_sql_file_runs() {
        // A hung protocol would otherwise hang the whole check.
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watching = done.clone();
        std::thread::spawn(move || {
            let began = Instant::now();
            while !watching.load(Ordering::Relaxed) {
                assert!(began.elapsed() < Duration::from_secs(30) || { eprintln!("a_sql_file_runs hung"); std::process::exit(101) });
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        let dir = std::env::temp_dir().join(format!("ideae-sql-e2e-{}", super::super::config::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        *tunnel::STAND_IN.lock().unwrap() = Some(write_ssh(&dir));
        let port = serve();
        let connection = Connection {
            id: "e2e".into(),
            name: "Through a bastion".into(),
            driver: super::super::config::Driver::MySql,
            host: "127.0.0.1".into(),
            port,
            user: "me".into(),
            database: Some("app".into()),
            ssh: Some(SshHop { host: "bastion.invalid".into(), port: 22, user: "me".into(), key: None }),
            bastion: None,
            tls: Default::default(),
            color: None,
            production: false,
        };

        let (session, events) = Session::start(connection.clone(), Some("secret".into()), None);
        let Event::Connected { version, .. } = until(&events, |e| matches!(e, Event::Connected { .. } | Event::Lost(_))) else { panic!("did not connect") };
        assert_eq!(version, VERSION);

        session.list_databases();
        let Event::Databases(names) = until(&events, |e| matches!(e, Event::Databases(_))) else { unreachable!() };
        assert_eq!(names, ["app", "other"]);

        let (rows, cut, _, failed) = run(&session, &events, "SELECT rows 3");
        assert_eq!((rows.len(), cut, failed), (3, false, None));
        assert_eq!(rows[1][0], Cell::Int(9_007_199_254_740_993), "a BIGINT keeps its last digit");
        assert_eq!(rows[0][1], Cell::Decimal("10.50".into()));
        assert_eq!((&rows[0][2], &rows[1][2]), (&Cell::Text("".into()), &Cell::Null), "empty and NULL are told apart");

        let (_, _, _, failed) = run(&session, &events, "nonsense");
        assert!(failed.is_some_and(|m| m.contains("syntax")));
        let (_, _, affected, failed) = run(&session, &events, "UPDATE t SET a = 1");
        assert_eq!((affected, failed), (3, None), "a refused statement leaves the connection usable");

        let (rows, cut, _, failed) = run(&session, &events, &format!("SELECT rows {}", MOST_ROWS * 3));
        assert_eq!((rows.len(), cut, failed), (MOST_ROWS, true, None), "stopped at the limit, not read to the end");

        session.run(statement("SELECT sleep"));
        std::thread::sleep(Duration::from_millis(30));
        session.stop();
        let Event::Idle { stopped } = until(&events, |e| matches!(e, Event::Idle { .. })) else { unreachable!() };
        assert!(stopped);
        let (rows, ..) = run(&session, &events, "SELECT rows 1");
        assert_eq!(rows.len(), 1, "it runs again after a stop");

        // A second file shares the tunnel; the last one to go ends it.
        let pid: u32 = std::fs::read_to_string(dir.join("ssh.pid")).unwrap().trim().parse().unwrap();
        let (second, second_events) = Session::start(connection, Some("secret".into()), None);
        until(&second_events, |e| matches!(e, Event::Connected { .. }));
        assert_eq!(std::fs::read_to_string(dir.join("ssh.pid")).unwrap().trim(), pid.to_string(), "one ssh for both");
        drop(session);
        while events.recv_blocking().is_ok() {}
        assert!(crate::procs::alive(pid), "the other file still uses it");
        drop(second);
        while second_events.recv_blocking().is_ok() {}
        let began = Instant::now();
        while crate::procs::alive(pid) {
            assert!(began.elapsed() < Duration::from_secs(5), "ssh outlived its last session");
            std::thread::sleep(Duration::from_millis(5));
        }

        *tunnel::STAND_IN.lock().unwrap() = None;
        done.store(true, Ordering::Relaxed);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_stand_in_is_not_run_by_accident() {
        assert!(std::env::var(ARGS_ENV).is_err());
        let _ = std::io::BufReader::new(std::io::empty()).lines().count();
    }
}
