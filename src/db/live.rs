//! Real servers behind a real bastion, for a look before a release: run by
//! hand with `IDEAE_LIVE_SSH_PORT`, `IDEAE_LIVE_SSH_KEY` and
//! `IDEAE_LIVE_SSH_PROGRAM` set, the databases known to the bastion as
//! `mysql`, `postgres` and `redis` with the password `secret`.

use std::{ops::Range, path::PathBuf, time::{Duration, Instant}};

use async_channel::Receiver;

use super::{
    config::{Connection, Driver, SshHop, Tls, TlsMode},
    export::Format,
    session::{Event, MOST_ROWS, Session},
    tunnel,
    value::Cell,
};

fn connection(driver: Driver, host: &str, user: &str, database: Option<&str>) -> Option<Connection> {
    let port = std::env::var("IDEAE_LIVE_SSH_PORT").ok()?.parse().ok()?;
    let key = PathBuf::from(std::env::var_os("IDEAE_LIVE_SSH_KEY")?);
    Some(Connection {
        id: format!("live-{host}"),
        name: host.into(),
        driver,
        host: host.into(),
        port: driver.default_port(),
        user: user.into(),
        database: database.map(String::from),
        ssh: Some(SshHop { host: "127.0.0.1".into(), port, user: "tunnel".into(), key: Some(key) }),
        bastion: None,
        tls: Default::default(),
        color: None,
        production: false,
    })
}

fn statement(sql: &str) -> Vec<(Range<usize>, String)> {
    vec![(0..sql.len(), sql.to_string())]
}

#[derive(Default, Debug)]
struct Ran {
    rows: Vec<Box<[Cell]>>,
    columns: Vec<String>,
    cut: bool,
    affected: u64,
    failed: Option<String>,
    stopped: bool,
}

fn drain(events: &Receiver<Event>) -> Ran {
    let mut ran = Ran::default();
    loop {
        match events.recv_blocking().expect("the session ended") {
            Event::Result { columns, .. } => ran.columns = columns.into_iter().map(|c| c.name).collect(),
            Event::Rows(batch) => ran.rows.extend(batch),
            Event::ResultDone { cut, affected, .. } => (ran.cut, ran.affected) = (cut, affected),
            Event::Failed { message, .. } => ran.failed = Some(message),
            Event::Idle { stopped } => {
                ran.stopped = stopped;
                return ran;
            }
            Event::Lost(why) => panic!("lost the connection: {why}"),
            _ => {}
        }
    }
}

fn run(session: &Session, events: &Receiver<Event>, sql: &str) -> Ran {
    session.run(statement(sql));
    drain(events)
}

fn ok(session: &Session, events: &Receiver<Event>, sql: &str) -> Ran {
    let ran = run(session, events, sql);
    assert_eq!(ran.failed, None, "{sql}");
    ran
}

fn start(connection: Connection) -> (Session, Receiver<Event>, String) {
    let (session, events) = Session::start(connection, Some("secret".into()), None);
    loop {
        match events.recv_blocking().expect("the session ended") {
            Event::Connected { version, .. } => return (session, events, version),
            Event::Lost(why) => panic!("could not connect: {why}"),
            _ => {}
        }
    }
}

fn wanted<T>(events: &Receiver<Event>, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    loop {
        let event = events.recv_blocking().expect("the session ended");
        if let Event::Note { text, failed: true } = &event {
            panic!("{text}");
        }
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

/// What MySQL and PostgreSQL both have to do.
fn sql_server(connection: Connection, sleep: &str, series: &str, big: &str) {
    let dir = std::env::temp_dir().join(format!("ideae-live-{}", super::config::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = connection.database.clone().unwrap();
    let (session, events, version) = start(connection);
    assert!(!version.is_empty());
    eprintln!("connected through the bastion: {version}");

    session.list_databases();
    let names = wanted(&events, |e| if let Event::Databases(n) = e { Some(n) } else { None });
    assert!(names.contains(&database), "{names:?}");

    ok(&session, &events, "DROP TABLE IF EXISTS ideae_live");
    ok(&session, &events, big);
    let inserted = ok(&session, &events, "INSERT INTO ideae_live (id, amount, note) VALUES (9007199254740993, 12345678901234.56, NULL), (2, 0.10, ''), (3, -1.00, 'it''s')");
    assert_eq!(inserted.affected, 3);

    let read = ok(&session, &events, "SELECT id, amount, note FROM ideae_live ORDER BY id DESC");
    assert_eq!(read.columns, ["id", "amount", "note"]);
    assert_eq!(read.rows[0][0], Cell::Int(9_007_199_254_740_993), "a BIGINT keeps its last digit");
    assert_eq!(read.rows[0][1], Cell::Decimal("12345678901234.56".into()));
    assert_eq!(read.rows[0][2], Cell::Null);
    assert_eq!(read.rows[2][2], Cell::Text("".into()), "empty is not NULL");

    let refused = run(&session, &events, "SELEC nonsense");
    assert!(refused.failed.is_some());
    ok(&session, &events, "SELECT 1");

    session.load_schema(database.clone());
    let schema = wanted(&events, |e| if let Event::Schema(s) = e { Some(s) } else { None });
    let table = schema.table("ideae_live").expect("the table in the schema");
    assert_eq!(table.columns.iter().find(|c| c.key == "PRI").map(|c| c.name.as_str()), Some("id"));

    session.show_create("ideae_live".into());
    let text = wanted(&events, |e| if let Event::Definition { text, .. } = e { Some(text) } else { None });
    assert!(text.contains("CREATE TABLE") && text.contains("amount"), "{text}");

    session.set_manual(true);
    assert!(wanted(&events, |e| if let Event::Manual(m) = e { Some(m) } else { None }));
    ok(&session, &events, "UPDATE ideae_live SET note = 'changed' WHERE id = 2");
    session.rollback();
    wanted(&events, |e| matches!(&e, Event::Note { text, .. } if text == "Rolled back").then_some(()));
    assert_eq!(ok(&session, &events, "SELECT note FROM ideae_live WHERE id = 2").rows[0][0], Cell::Text("".into()), "rolled back");
    ok(&session, &events, "UPDATE ideae_live SET note = 'kept' WHERE id = 2");
    session.commit();
    wanted(&events, |e| matches!(&e, Event::Note { text, .. } if text == "Committed").then_some(()));
    session.set_manual(false);
    assert!(!wanted(&events, |e| if let Event::Manual(m) = e { Some(m) } else { None }));
    assert_eq!(ok(&session, &events, "SELECT note FROM ideae_live WHERE id = 2").rows[0][0], Cell::Text("kept".into()));

    let many = ok(&session, &events, series);
    assert_eq!((many.rows.len(), many.cut), (MOST_ROWS, true), "stopped at the limit");
    assert_eq!(ok(&session, &events, "SELECT 1").rows.len(), 1, "usable after the limit");

    let file = dir.join("all.csv");
    session.export(series.to_string(), Format::Csv, file.clone(), "t".into());
    wanted(&events, |e| matches!(&e, Event::Note { text, .. } if text.starts_with("Exported")).then_some(()));
    drain(&events);
    let lines = std::fs::read_to_string(&file).unwrap().lines().count();
    assert!(lines > MOST_ROWS + 1, "the whole result, past the limit: {lines}");

    session.run(statement(sleep));
    std::thread::sleep(Duration::from_millis(300));
    let began = Instant::now();
    session.stop();
    let stopped = drain(&events);
    assert!(stopped.stopped && began.elapsed() < Duration::from_secs(5), "{stopped:?}");
    assert_eq!(ok(&session, &events, "SELECT 1").rows.len(), 1, "usable after a stop");

    ok(&session, &events, "DROP TABLE ideae_live");
    drop(session);
    while events.recv_blocking().is_ok() {}
    std::fs::remove_dir_all(dir).unwrap();
}

fn use_live_ssh() -> bool {
    let Some(program) = std::env::var_os("IDEAE_LIVE_SSH_PROGRAM") else {
        eprintln!("no live servers named: skipped");
        return false;
    };
    *tunnel::STAND_IN.lock().unwrap() = Some(PathBuf::from(program));
    true
}

#[test]
#[ignore]
fn a_real_mysql_through_a_bastion() {
    if !use_live_ssh() {
        return;
    }
    let Some(connection) = connection(Driver::MySql, "mysql", "app", Some("app")) else { return };
    sql_server(
        connection,
        "SELECT SLEEP(30)",
        "WITH RECURSIVE n (i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 25000) SELECT /*+ SET_VAR(cte_max_recursion_depth = 30000) */ i FROM n",
        "CREATE TABLE ideae_live (id BIGINT PRIMARY KEY, amount DECIMAL(20,2), note VARCHAR(50))",
    );
}

#[test]
#[ignore]
fn a_real_postgres_through_a_bastion() {
    if !use_live_ssh() {
        return;
    }
    let Some(connection) = connection(Driver::Postgres, "postgres", "app", Some("app")) else { return };
    sql_server(connection, "SELECT pg_sleep(30)", "SELECT i FROM generate_series(1, 25000) AS i", "CREATE TABLE ideae_live (id BIGINT PRIMARY KEY, amount NUMERIC(20,2), note VARCHAR(50))");
}

#[test]
#[ignore]
fn a_real_redis_through_a_bastion() {
    if !use_live_ssh() {
        return;
    }
    let Some(connection) = connection(Driver::Redis, "redis", "", Some("1")) else { return };
    let (session, events, version) = start(connection);
    eprintln!("connected through the bastion: redis {version}");
    ok(&session, &events, "FLUSHDB");
    assert_eq!(ok(&session, &events, "SET ideae:key \"a b\"").rows[0][0], Cell::Text("OK".into()));
    assert_eq!(ok(&session, &events, "GET ideae:key").rows[0][0], Cell::Text("a b".into()));
    assert_eq!(ok(&session, &events, "GET ideae:none").rows[0][0], Cell::Null);
    ok(&session, &events, "HSET ideae:hash f1 v1 f2 v2");
    let hash = ok(&session, &events, "HGETALL ideae:hash");
    assert_eq!((hash.columns.as_slice(), hash.rows.len()), (["field".to_string(), "value".to_string()].as_slice(), 2));
    assert!(run(&session, &events, "NOSUCHCOMMAND").failed.is_some());
    assert!(run(&session, &events, "MONITOR").failed.is_some());
    // A command that waits for ever is stopped, and the connection goes on.
    session.run(statement("BLPOP ideae:nothing 0"));
    std::thread::sleep(Duration::from_millis(200));
    session.stop();
    assert!(drain(&events).stopped);
    assert_eq!(ok(&session, &events, "PING").rows[0][0], Cell::Text("PONG".into()));
    session.load_schema("1".into());
    let schema = wanted(&events, |e| if let Event::Schema(s) = e { Some(s) } else { None });
    assert_eq!(schema.table("ideae:hash").map(|t| t.columns[0].kind.as_str()), Some("hash"));
    session.show_create("ideae:key".into());
    let text = wanted(&events, |e| if let Event::Definition { text, .. } = e { Some(text) } else { None });
    assert!(text.contains("string"), "{text}");
    ok(&session, &events, "FLUSHDB");
    drop(session);
    while events.recv_blocking().is_ok() {}
}

/// The same server reached directly, on the port published for it.
fn direct(driver: Driver, port_env: &str, user: &str, database: Option<&str>, mode: TlsMode, ca: bool) -> Option<Connection> {
    let port = std::env::var(port_env).ok()?.parse().ok()?;
    let ca = ca.then(|| std::env::var_os("IDEAE_LIVE_CA").map(PathBuf::from)).flatten();
    Some(Connection {
        id: format!("direct-{port}"),
        name: "direct".into(),
        driver,
        host: "localhost".into(),
        port,
        user: user.into(),
        database: database.map(String::from),
        bastion: None,
        ssh: None,
        tls: Tls { mode, ca },
        color: None,
        production: false,
    })
}

fn refused(connection: Connection) -> String {
    let (session, events) = Session::start(connection, Some("secret".into()), None);
    let why = loop {
        match events.recv_blocking() {
            Ok(Event::Lost(why)) => break why,
            Ok(Event::Connected { version, .. }) => panic!("connected to {version}, and should not have"),
            Ok(_) => {}
            Err(_) => panic!("ended without saying why"),
        }
    };
    drop(session);
    why
}

fn encrypted_mysql(session: &Session, events: &Receiver<Event>) -> bool {
    let row = ok(session, events, "SHOW SESSION STATUS LIKE 'Ssl_cipher'");
    !matches!(row.rows.first().map(|r| r[1].shown().into_owned()), Some(cipher) if cipher.is_empty())
}

fn encrypted_postgres(session: &Session, events: &Receiver<Event>) -> bool {
    ok(session, events, "SELECT ssl::text FROM pg_stat_ssl WHERE pid = pg_backend_pid()").rows[0][0] == Cell::Text("true".into())
}

#[test]
#[ignore]
fn a_real_server_reached_directly_with_each_kind_of_encryption() {
    if std::env::var_os("IDEAE_LIVE_MYSQL_PORT").is_none() {
        return eprintln!("no live servers named: skipped");
    }
    for (mode, want) in [(TlsMode::Off, false), (TlsMode::Prefer, true), (TlsMode::Require, true)] {
        let (session, events, _) = start(direct(Driver::MySql, "IDEAE_LIVE_MYSQL_PORT", "app", Some("app"), mode, false).unwrap());
        assert_eq!(encrypted_mysql(&session, &events), want, "MySQL {mode:?}");
        let (session, events, _) = start(direct(Driver::Postgres, "IDEAE_LIVE_POSTGRES_PORT", "app", Some("app"), mode, false).unwrap());
        assert_eq!(encrypted_postgres(&session, &events), want, "PostgreSQL {mode:?}");
    }
    // The certificate checked against the test's own authority, and refused without it.
    let (session, events, _) = start(direct(Driver::MySql, "IDEAE_LIVE_MYSQL_PORT", "app", Some("app"), TlsMode::Verify, true).unwrap());
    assert!(encrypted_mysql(&session, &events));
    let (session, events, _) = start(direct(Driver::Postgres, "IDEAE_LIVE_POSTGRES_PORT", "app", Some("app"), TlsMode::Verify, true).unwrap());
    assert!(encrypted_postgres(&session, &events));
    eprintln!("no CA, MySQL: {}", refused(direct(Driver::MySql, "IDEAE_LIVE_MYSQL_PORT", "app", Some("app"), TlsMode::Verify, false).unwrap()));
    eprintln!("no CA, PostgreSQL: {}", refused(direct(Driver::Postgres, "IDEAE_LIVE_POSTGRES_PORT", "app", Some("app"), TlsMode::Verify, false).unwrap()));

    // Redis: plain on one port, only TLS on the other.
    for (env, mode, ca) in [("IDEAE_LIVE_REDIS_PORT", TlsMode::Off, false), ("IDEAE_LIVE_REDIS_PORT", TlsMode::Prefer, false), ("IDEAE_LIVE_REDIS_TLS_PORT", TlsMode::Require, false), ("IDEAE_LIVE_REDIS_TLS_PORT", TlsMode::Prefer, false), ("IDEAE_LIVE_REDIS_TLS_PORT", TlsMode::Verify, true)] {
        let (session, events, _) = start(direct(Driver::Redis, env, "", Some("2"), mode, ca).unwrap());
        assert_eq!(ok(&session, &events, "PING").rows[0][0], Cell::Text("PONG".into()), "{env} {mode:?}");
    }
    eprintln!("no CA, Redis: {}", refused(direct(Driver::Redis, "IDEAE_LIVE_REDIS_TLS_PORT", "", Some("2"), TlsMode::Verify, false).unwrap()));
}

#[test]
#[ignore]
fn a_certificate_is_checked_for_the_database_s_name_through_a_tunnel() {
    if !use_live_ssh() {
        return;
    }
    let Some(ca) = std::env::var_os("IDEAE_LIVE_CA").map(PathBuf::from) else { return };
    let Some(mut good) = connection(Driver::Postgres, "postgres", "app", Some("app")) else { return };
    good.tls = Tls { mode: TlsMode::Verify, ca: Some(ca) };
    let (session, events, _) = start(good);
    assert!(encrypted_postgres(&session, &events));
}

/// What a result's columns really are, a batch kept whole, and a connection killed.
fn safety(connection: Connection, this_connection: &str, kill: &str) {
    let (session, events, _) = start(connection.clone());
    ok(&session, &events, "DROP TABLE IF EXISTS ideae_safe");
    ok(&session, &events, "CREATE TABLE ideae_safe (id BIGINT PRIMARY KEY, user_id BIGINT, note VARCHAR(20))");
    ok(&session, &events, "INSERT INTO ideae_safe VALUES (1, 100, 'a'), (2, 200, 'b')");

    session.run(statement("SELECT user_id AS id, id + 1 AS x, note AS other FROM ideae_safe"));
    let columns = wanted(&events, |e| if let Event::Result { columns, .. } = e { Some(columns) } else { None });
    drain(&events);
    assert_eq!(columns[0].source.as_ref().map(|s| s.column.as_str()), Some("user_id"), "an alias is not the column");
    assert_eq!(columns[1].source, None, "an expression has no column");
    assert_eq!(columns[2].source.as_ref().map(|s| (s.table.as_str(), s.column.as_str())), Some(("ideae_safe", "note")));

    session.apply_batch(vec!["UPDATE ideae_safe SET note = 'x' WHERE id = 1".into(), "UPDATE ideae_safe SET nosuch = 1 WHERE id = 2".into()]);
    let error = wanted(&events, |e| if let Event::BatchDone { error } = e { Some(error) } else { None });
    assert!(error.is_some());
    drain(&events);
    assert_eq!(ok(&session, &events, "SELECT note FROM ideae_safe WHERE id = 1").rows[0][0], Cell::Text("a".into()), "all or none");
    session.apply_batch(vec!["UPDATE ideae_safe SET note = 'x' WHERE id = 1".into(), "UPDATE ideae_safe SET note = 'y' WHERE id = 2".into()]);
    assert_eq!(wanted(&events, |e| if let Event::BatchDone { error } = e { Some(error) } else { None }), None);
    drain(&events);
    assert_eq!(ok(&session, &events, "SELECT note FROM ideae_safe WHERE id = 2").rows[0][0], Cell::Text("y".into()));

    // An error says where: `WHER` reads as an alias, so it is the `id` after it.
    session.run(statement("SELECT id\nFROM ideae_safe\nWHER id = 1"));
    let at = wanted(&events, |e| if let Event::Failed { at, .. } = e { Some(at) } else { None });
    drain(&events);
    assert_eq!(at, Some("SELECT id\nFROM ideae_safe\nWHER ".len()), "the place of the error");

    // A file in: all of it or none, an empty field NULL and two quotes an empty string.
    let dir = std::env::temp_dir().join(format!("ideae-live-csv-{}", super::config::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good.csv");
    std::fs::write(&good, "id,user_id,note\n10,1000,\"a, b\"\n11,,\"\"\n").unwrap();
    session.import_csv("ideae_safe".into(), vec!["id".into(), "user_id".into(), "note".into()], good, b',', true);
    wanted(&events, |e| matches!(&e, Event::Note { text, .. } if text.starts_with("Imported 2 rows")).then_some(()));
    drain(&events);
    let read = ok(&session, &events, "SELECT user_id, note FROM ideae_safe WHERE id = 11");
    assert_eq!((&read.rows[0][0], &read.rows[0][1]), (&Cell::Null, &Cell::Text("".into())));
    let bad = dir.join("bad.csv");
    std::fs::write(&bad, "id,user_id,note\n20,1,x\n10,1,duplicate key\n").unwrap();
    session.import_csv("ideae_safe".into(), vec!["id".into(), "user_id".into(), "note".into()], bad, b',', true);
    let note = loop {
        match events.recv_blocking().expect("the session ended") {
            Event::Note { text, failed: true } => break text,
            Event::Note { text, failed: false } => panic!("{text}"),
            _ => {}
        }
    };
    assert!(note.starts_with("Nothing was imported"), "{note}");
    drain(&events);
    assert!(ok(&session, &events, "SELECT id FROM ideae_safe WHERE id = 20").rows.is_empty(), "all or none");
    std::fs::remove_dir_all(dir).unwrap();

    // The schema is read beside the statements, with its keys and indexes.
    ok(&session, &events, "CREATE INDEX ideae_safe_user ON ideae_safe (user_id)");
    session.load_schema(connection.database.clone().unwrap());
    let schema = wanted(&events, |e| if let Event::Schema(s) = e { Some(s) } else { None });
    assert!(schema.indexes.iter().any(|i| i.table == "ideae_safe" && i.name == "ideae_safe_user" && i.columns == ["user_id"]), "{:?}", schema.indexes);

    // A long RETURNING: the table keeps its limit, and every row is written.
    if this_connection.contains("pg_backend") {
        ok(&session, &events, "CREATE TEMP TABLE ideae_many (n INT)");
        let inserted = ok(&session, &events, &format!("INSERT INTO ideae_many SELECT generate_series(1, {}) RETURNING n", MOST_ROWS * 2));
        assert_eq!((inserted.rows.len(), inserted.cut), (MOST_ROWS, true));
        assert_eq!(ok(&session, &events, "SELECT count(*) FROM ideae_many").rows[0][0], Cell::Int(MOST_ROWS as i64 * 2), "not stopped half way");
    }

    // Killed while idle, as a link that died in the machine's sleep: the next statement still runs, once.
    let id = ok(&session, &events, this_connection).rows[0][0].shown().into_owned();
    let (other, other_events, _) = start(connection);
    let _ = run(&other, &other_events, &kill.replace("{id}", &id));
    std::thread::sleep(Duration::from_millis(200));
    let after = ok(&session, &events, "UPDATE ideae_safe SET user_id = user_id + 1 WHERE id = 1");
    assert_eq!(after.affected, 1);
    assert_eq!(ok(&other, &other_events, "SELECT user_id FROM ideae_safe WHERE id = 1").rows[0][0], Cell::Int(101), "sent exactly once");
    assert_ne!(ok(&session, &events, this_connection).rows[0][0].shown(), id, "on a new connection");
    ok(&session, &events, "DROP TABLE ideae_safe");
}

#[test]
#[ignore]
fn a_real_server_keeps_data_safe() {
    if std::env::var_os("IDEAE_LIVE_MYSQL_PORT").is_none() {
        return eprintln!("no live servers named: skipped");
    }
    safety(direct(Driver::MySql, "IDEAE_LIVE_MYSQL_PORT", "app", Some("app"), TlsMode::Prefer, false).unwrap(), "SELECT CONNECTION_ID()", "KILL {id}");
    safety(direct(Driver::Postgres, "IDEAE_LIVE_POSTGRES_PORT", "app", Some("app"), TlsMode::Prefer, false).unwrap(), "SELECT pg_backend_pid()", "SELECT pg_terminate_backend({id})");
}
