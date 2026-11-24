//! A file's connection to its database, on a thread of its own: drivers
//! block. What it reads comes back in batches through a bounded channel, so
//! a window that is busy holds the server's rows back instead of piling them
//! up. A statement is never sent twice: a connection lost is reported, and
//! the next run connects anew.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use async_channel::{Receiver, Sender};

pub use super::backend::{ColumnInfo, ColumnSource};
use super::{
    backend::{self, Backend, DbError, DbResult, Flow, Sink, Stop},
    config::{Connection, Driver},
    export::{Format, Writer},
    schema::Schema,
    tunnel,
    value::Row,
};

/// Rows kept of one result; past them the statement is stopped.
pub const MOST_ROWS: usize = 10_000;
/// And bytes, for rows of long values.
const MOST_BYTES: usize = 128 * 1024 * 1024;
const BATCH: usize = 200;

#[derive(Debug)]
pub enum Event {
    Connected { version: String, encrypted: bool },
    /// Could not connect, or the connection is gone: the session is over.
    Lost(String),
    Databases(Vec<String>),
    /// A result begins. `statement` is the place in the file.
    Result { statement: Range<usize>, columns: Vec<ColumnInfo> },
    Rows(Vec<Row>),
    /// A result ended; a statement that returns no rows has only this.
    ResultDone { statement: Range<usize>, affected: u64, took: Duration, cut: bool },
    /// `at`: the byte of the statement the server says it is about.
    Failed { statement: Range<usize>, message: String, at: Option<usize> },
    /// Every statement of the run is done, or the run was stopped.
    Idle { stopped: bool },
    /// A line for the Output tab: a commit, a mode changed, an export done.
    Note { text: String, failed: bool },
    /// Whether statements now wait for a commit.
    Manual(bool),
    /// The open transaction was committed or rolled back.
    TransactionEnded,
    Schema(Arc<Schema>),
    /// What is known of the schema: being read, or as read and still so.
    SchemaState(SchemaState),
    /// What creates a table, for the Output tab.
    Definition { table: String, text: String },
    /// A batch of cell changes ended: all of them kept, or, with why, none.
    BatchDone { error: Option<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaState {
    /// Being compared with the server, or read from it.
    Checking,
    Current,
}

enum Command {
    Run(Vec<(Range<usize>, String)>),
    UseDatabase(String),
    ListDatabases,
    SetManual(bool),
    Commit,
    Rollback,
    Export { sql: String, format: Format, path: PathBuf, table: String },
    ShowCreate(String),
    Batch(Vec<String>),
    Import { table: String, columns: Vec<String>, path: PathBuf, separator: u8, empty_is_null: bool },
}

/// What it takes to stop a statement from outside.
#[derive(Default)]
struct Stopping {
    stopper: Mutex<Option<Arc<dyn Stop>>>,
    running: AtomicBool,
    stopped: AtomicBool,
}

impl Stopping {
    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

/// Dropped with the file: what runs is stopped, and the connection closed.
pub struct Session {
    commands: Sender<Command>,
    stopping: Arc<Stopping>,
    /// For the schema's own connection, which leaves this one free for statements.
    schema_job: Arc<SchemaJob>,
}

struct SchemaJob {
    connection: Connection,
    password: Option<String>,
    events: Sender<Event>,
    /// A later read makes an earlier one of no use.
    latest: AtomicU64,
}

impl SchemaJob {
    /// What was kept shows at once; the server is then asked whether it still holds.
    fn run(&self, database: String, turn: u64) {
        let current = || self.latest.load(Ordering::Acquire) == turn;
        let say = |event| current() && self.events.send_blocking(event).is_ok();
        let kept = super::schema_cache::read(&self.connection.id, &database);
        let kept_stamp = kept.as_ref().map(|(stamp, _)| stamp.clone());
        if let Some((_, schema)) = kept
            && !say(Event::Schema(Arc::new(schema)))
        {
            return;
        }
        // Locked production: what was kept, and no login to the server.
        if super::guard::may_run(&self.connection).is_err() {
            say(Event::SchemaState(SchemaState::Current));
            return;
        }
        if !say(Event::SchemaState(SchemaState::Checking)) {
            return;
        }
        let outcome = (|| -> Result<Option<(Option<String>, Schema)>> {
            let tunnel = self.connection.ssh.as_ref().map(|hop| tunnel::open(hop, &self.connection.host, self.connection.port)).transpose()?;
            let mut db = backend::connect(&self.connection, tunnel.as_deref(), self.password.clone(), Some(database.clone()))?;
            let stamp = db.schema_stamp(&database)?;
            if stamp.is_some() && stamp == kept_stamp {
                return Ok(None);
            }
            Ok(Some((stamp, db.schema(&database)?)))
        })();
        match outcome {
            Ok(None) => {}
            Ok(Some((stamp, schema))) => {
                if let Some(stamp) = &stamp {
                    super::schema_cache::write(&self.connection.id, &database, stamp, &schema);
                }
                if !say(Event::Schema(Arc::new(schema))) {
                    return;
                }
            }
            Err(e) => {
                say(Event::Note { text: format!("The schema could not be read: {e}"), failed: true });
            }
        }
        say(Event::SchemaState(SchemaState::Current));
    }
}

impl Session {
    /// Connects in the background. `events` ends when the session does.
    pub fn start(connection: Connection, password: Option<String>, database: Option<String>) -> (Session, Receiver<Event>) {
        let (commands, inbox) = async_channel::unbounded();
        let (events, receiver) = async_channel::bounded(16);
        let stopping = Arc::new(Stopping::default());
        let held = stopping.clone();
        let schema_job = Arc::new(SchemaJob { connection: connection.clone(), password: password.clone(), events: events.clone(), latest: AtomicU64::new(0) });
        std::thread::Builder::new()
            .name("sql-session".into())
            .spawn(move || run(connection, password, database, inbox, events, held))
            .expect("start a thread for the session");
        (Session { commands, stopping, schema_job }, receiver)
    }

    pub fn run(&self, statements: Vec<(Range<usize>, String)>) {
        self.stopping.stopped.store(false, Ordering::Release);
        let _ = self.commands.try_send(Command::Run(statements));
    }

    pub fn use_database(&self, name: String) {
        let _ = self.commands.try_send(Command::UseDatabase(name));
    }

    pub fn list_databases(&self) {
        let _ = self.commands.try_send(Command::ListDatabases);
    }

    /// Manual: nothing is kept until [`Session::commit`].
    pub fn set_manual(&self, manual: bool) {
        let _ = self.commands.try_send(Command::SetManual(manual));
    }

    pub fn commit(&self) {
        let _ = self.commands.try_send(Command::Commit);
    }

    pub fn rollback(&self) {
        let _ = self.commands.try_send(Command::Rollback);
    }

    /// Runs `sql` again and streams all of its rows to a file, past the table's limit.
    pub fn export(&self, sql: String, format: Format, path: PathBuf, table: String) {
        self.stopping.stopped.store(false, Ordering::Release);
        let _ = self.commands.try_send(Command::Export { sql, format, path, table });
    }

    /// Reads what `database` holds; [`Event::Schema`] brings it.
    pub fn load_schema(&self, database: String) {
        let turn = self.schema_job.latest.fetch_add(1, Ordering::AcqRel) + 1;
        let job = self.schema_job.clone();
        let _ = std::thread::Builder::new().name("sql-schema".into()).spawn(move || job.run(database, turn));
    }

    pub fn show_create(&self, table: String) {
        let _ = self.commands.try_send(Command::ShowCreate(table));
    }

    /// Statements that each change one row, kept all or none.
    pub fn apply_batch(&self, statements: Vec<String>) {
        self.stopping.stopped.store(false, Ordering::Release);
        let _ = self.commands.try_send(Command::Batch(statements));
    }

    /// A CSV file into a table, all of it or none.
    pub fn import_csv(&self, table: String, columns: Vec<String>, path: PathBuf, separator: u8, empty_is_null: bool) {
        self.stopping.stopped.store(false, Ordering::Release);
        let _ = self.commands.try_send(Command::Import { table, columns, path, separator, empty_is_null });
    }

    /// Stops what runs, and what was to run after it.
    pub fn stop(&self) {
        self.stopping.stopped.store(true, Ordering::Release);
        if self.stopping.running.load(Ordering::Acquire)
            && let Some(stopper) = self.stopping.stopper.lock().unwrap_or_else(|e| e.into_inner()).clone()
        {
            std::thread::spawn(move || {
                let _ = stopper.stop();
            });
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
        self.schema_job.latest.fetch_add(1, Ordering::AcqRel);
        self.commands.close();
    }
}

/// Connects once and says what the server is: for Test Connection.
pub fn test(connection: &Connection, password: Option<String>) -> Result<String> {
    super::guard::may_run(connection).map_err(anyhow::Error::msg)?;
    let tunnel = connection.ssh.as_ref().map(|hop| tunnel::open(hop, &connection.host, connection.port)).transpose()?;
    let mut backend = backend::connect(connection, tunnel.as_deref(), password, None)?;
    Ok(backend.version())
}

fn run(connection: Connection, password: Option<String>, database: Option<String>, inbox: Receiver<Command>, events: Sender<Event>, stopping: Arc<Stopping>) {
    let say = |event| events.send_blocking(event).is_ok();
    if let Err(locked) = super::guard::may_run(&connection) {
        say(Event::Lost(locked));
        return;
    }
    let open_tunnel = || connection.ssh.as_ref().map(|hop| tunnel::open(hop, &connection.host, connection.port)).transpose();
    // Held for as long as the connection, and dropped after it.
    let mut tunnel = match open_tunnel() {
        Ok(tunnel) => tunnel,
        Err(e) => {
            say(Event::Lost(e.to_string()));
            return;
        }
    };
    let mut current = database.clone();
    let mut manual = false;
    let mut db = match backend::connect(&connection, tunnel.as_deref(), password.clone(), database) {
        Ok(db) => db,
        Err(e) => {
            say(Event::Lost(e.to_string()));
            return;
        }
    };
    *stopping.stopper.lock().unwrap_or_else(|e| e.into_inner()) = Some(db.stopper());
    if !say(Event::Connected { version: db.version(), encrypted: db.encrypted() }) {
        return;
    }
    // The answer to a command that has no result of its own.
    let answer = |outcome: Result<Event, DbError>| match outcome {
        Ok(event) => say(event),
        Err(DbError::Gone(text)) => {
            say(Event::Lost(text));
            false
        }
        Err(e) => say(Event::Note { text: e.to_string(), failed: true }),
    };
    while let Ok(command) = inbox.recv_blocking() {
        // Locked again since it connected: nothing more goes to production,
        // and the session stays, with its transaction, for when it is unlocked.
        if let Err(locked) = super::guard::may_run(&connection) {
            let refused = format!("{locked}. Nothing was sent");
            let alive = match &command {
                Command::Run(statements) => {
                    let statement = statements.first().map_or(0..0, |(r, _)| r.clone());
                    say(Event::Failed { statement, message: refused, at: None }) && say(Event::Idle { stopped: false })
                }
                Command::Batch(_) => say(Event::BatchDone { error: Some(refused) }) && say(Event::Idle { stopped: false }),
                Command::Export { .. } | Command::Import { .. } => say(Event::Note { text: refused, failed: true }) && say(Event::Idle { stopped: false }),
                Command::Commit => say(Event::Note { text: refused, failed: true }),
                _ => true,
            };
            // Auto-commit turned on commits what is open: only turning it off passes.
            let passes = matches!(command, Command::ListDatabases | Command::UseDatabase(_) | Command::Rollback | Command::ShowCreate(_) | Command::SetManual(true));
            if matches!(command, Command::SetManual(false)) && !say(Event::Note { text: format!("{locked}. Auto-commit was not turned on"), failed: true }) {
                break;
            }
            if !alive {
                break;
            }
            if !passes {
                continue;
            }
        }
        // Found dead before anything is sent: nothing is lost, and nothing runs twice.
        if matches!(command, Command::Run(_) | Command::Export { .. } | Command::Batch(_) | Command::Import { .. }) && !db.ping() {
            if manual {
                say(Event::Lost("The connection was lost, with its open transaction. The statement was not sent".into()));
                break;
            }
            let again = open_tunnel().and_then(|t| backend::connect(&connection, t.as_deref(), password.clone(), current.clone()).map(|d| (t, d)));
            match again {
                Ok((new_tunnel, new_db)) => {
                    db = new_db;
                    tunnel = new_tunnel;
                    *stopping.stopper.lock().unwrap_or_else(|e| e.into_inner()) = Some(db.stopper());
                    if !say(Event::Note { text: format!("The connection had dropped: connected again{}", if db.encrypted() { "" } else { ", not encrypted" }), failed: false }) {
                        break;
                    }
                }
                Err(e) => {
                    say(Event::Lost(format!("The connection was lost, and the statement was not sent: {e}")));
                    break;
                }
            }
        }
        let alive = match command {
            Command::ListDatabases => answer(db.databases().map(Event::Databases)),
            Command::UseDatabase(name) => match db.use_database(&name) {
                Ok(()) => {
                    current = Some(name);
                    true
                }
                Err(e) => answer(Err(e)),
            },
            Command::Run(statements) => run_all(db.as_mut(), statements, &events, &stopping),
            Command::Batch(statements) => {
                stopping.running.store(true, Ordering::Release);
                let outcome = apply_batch(db.as_mut(), &statements, manual);
                stopping.running.store(false, Ordering::Release);
                match outcome {
                    Err(DbError::Gone(text)) => {
                        say(Event::Lost(text));
                        false
                    }
                    Err(e) => say(Event::BatchDone { error: Some(e.to_string()) }) && say(Event::Idle { stopped: false }),
                    Ok(()) => say(Event::BatchDone { error: None }) && say(Event::Idle { stopped: false }),
                }
            }
            Command::Import { table, columns, path, separator, empty_is_null } => {
                stopping.running.store(true, Ordering::Release);
                let outcome = import_csv(db.as_mut(), connection.driver, &table, &columns, &path, separator, empty_is_null, manual, &stopping);
                stopping.running.store(false, Ordering::Release);
                let stopped = stopping.is_stopped();
                match outcome {
                    Err(DbError::Gone(text)) => {
                        say(Event::Lost(text));
                        false
                    }
                    Ok(rows) => {
                        let tail = if manual { ": waiting for Commit" } else { "" };
                        say(Event::Note { text: format!("Imported {rows} rows into {table}{tail}"), failed: false }) && say(Event::Idle { stopped })
                    }
                    Err(e) => say(Event::Note { text: format!("Nothing was imported: {e}"), failed: true }) && say(Event::Idle { stopped }),
                }
            }
            Command::SetManual(on) => match db.set_manual(on) {
                Ok(()) => {
                    manual = on;
                    let text = if on { "Manual transactions: nothing is kept until Commit" } else { "Auto-commit: each statement is kept at once" };
                    say(Event::Manual(on)) && say(Event::Note { text: text.into(), failed: false })
                }
                Err(e) => answer(Err(e)),
            },
            Command::Commit => match db.commit() {
                Ok(()) => say(Event::Note { text: "Committed".into(), failed: false }) && say(Event::TransactionEnded),
                Err(e) => answer(Err(e)),
            },
            Command::Rollback => match db.rollback() {
                Ok(()) => say(Event::Note { text: "Rolled back".into(), failed: false }) && say(Event::TransactionEnded),
                Err(e) => answer(Err(e)),
            },
            Command::ShowCreate(table) => answer(db.definition(&table).map(|text| Event::Definition { table, text })),
            Command::Export { sql, format, path, table } => {
                stopping.running.store(true, Ordering::Release);
                let outcome = export(db.as_mut(), &sql, format, &path, &table, connection.driver, &stopping);
                stopping.running.store(false, Ordering::Release);
                let stopped = stopping.is_stopped();
                let note = match &outcome {
                    Ok(rows) => Event::Note { text: format!("Exported {rows} rows to {}", path.display()), failed: false },
                    Err(_) if stopped => Event::Note { text: "Export stopped: no file was written".into(), failed: true },
                    Err(e) => Event::Note { text: format!("Export failed: {e}"), failed: true },
                };
                say(note) && say(Event::Idle { stopped })
            }
        };
        if !alive {
            break;
        }
    }
    *stopping.stopper.lock().unwrap_or_else(|e| e.into_inner()) = None;
    drop(db);
    drop(tunnel);
}

/// How many rows a statement of a batch changed.
#[derive(Default)]
struct Counted(u64);

impl Sink for Counted {
    fn columns(&mut self, _: Vec<ColumnInfo>) -> Flow {
        Flow::Stop
    }

    fn row(&mut self, _: Row, _: usize) -> Flow {
        Flow::Stop
    }

    fn done(&mut self, affected: u64) {
        self.0 += affected;
    }
}

/// All or none. In a transaction of its own, unless one is open for the user's Commit.
fn apply_batch(db: &mut dyn Backend, statements: &[String], manual: bool) -> Result<(), DbError> {
    if !manual {
        db.begin()?;
    }
    for sql in statements {
        let mut count = Counted::default();
        let outcome = db.run(sql, &mut count).and_then(|()| {
            // Told by a primary key: more than one would mean the key was not one.
            if count.0 > 1 { Err(DbError::Refused(format!("{sql} would change {} rows", count.0))) } else { Ok(()) }
        });
        if let Err(e) = outcome {
            if matches!(e, DbError::Gone(_)) {
                return Err(e);
            }
            if !manual {
                db.rollback()?;
            }
            return Err(e);
        }
    }
    if !manual {
        db.commit()?;
    }
    Ok(())
}

/// Rows of a file into a table, in statements of a few hundred, in one
/// transaction: all of them or none, whatever fails or stops it.
#[allow(clippy::too_many_arguments)]
fn import_csv(db: &mut dyn Backend, driver: Driver, table: &str, columns: &[String], path: &Path, separator: u8, empty_is_null: bool, manual: bool, stopping: &Stopping) -> Result<u64, DbError> {
    const ROWS_A_STATEMENT: usize = 200;
    let bad = |e: std::io::Error| DbError::Refused(format!("{}: {e}", path.display()));
    let mut records = super::csv::opened(path, separator).map_err(bad)?;
    records.next_record().map_err(bad)?;
    let head = format!("INSERT INTO {} ({}) VALUES ", driver.quoted(table), columns.iter().map(|c| driver.quoted(c)).collect::<Vec<_>>().join(", "));
    if !manual {
        db.begin()?;
    }
    let send = |db: &mut dyn Backend, values: &mut Vec<String>| -> DbResult<()> {
        if values.is_empty() {
            return Ok(());
        }
        let sql = format!("{head}{}", values.join(", "));
        values.clear();
        struct Nothing;
        impl Sink for Nothing {
            fn columns(&mut self, _: Vec<ColumnInfo>) -> Flow {
                Flow::Go
            }
            fn row(&mut self, _: Row, _: usize) -> Flow {
                Flow::Go
            }
            fn done(&mut self, _: u64) {}
        }
        db.run(&sql, &mut Nothing)
    };
    let mut body = || -> DbResult<u64> {
        let (mut values, mut count) = (Vec::with_capacity(ROWS_A_STATEMENT), 0u64);
        while let Some(record) = records.next_record().map_err(bad)? {
            if record.len() == 1 && record[0].text.is_empty() && columns.len() != 1 {
                continue;
            }
            if record.len() != columns.len() {
                return Err(DbError::Refused(format!("Record {} has {} fields, the header {}", count + 1, record.len(), columns.len())));
            }
            let row = record.iter().map(|f| if f.text.is_empty() && !f.quoted && empty_is_null { "NULL".to_string() } else { driver.string(&f.text) }).collect::<Vec<_>>().join(", ");
            values.push(format!("({row})"));
            count += 1;
            if values.len() == ROWS_A_STATEMENT {
                send(db, &mut values)?;
                if stopping.is_stopped() {
                    return Err(DbError::Interrupted);
                }
            }
        }
        send(db, &mut values)?;
        Ok(count)
    };
    match body() {
        Ok(count) => {
            if !manual {
                db.commit()?;
            }
            Ok(count)
        }
        Err(DbError::Gone(text)) => Err(DbError::Gone(text)),
        Err(e) => {
            if !manual {
                db.rollback()?;
            }
            Err(e)
        }
    }
}

/// A statement's rows, on their way to the window.
struct ToWindow<'a> {
    events: &'a Sender<Event>,
    stopping: &'a Stopping,
    place: Range<usize>,
    began: Instant,
    batch: Vec<Row>,
    rows: usize,
    bytes: usize,
    cut: bool,
    /// Nobody reads the events any more.
    gone: bool,
}

impl ToWindow<'_> {
    fn send(&mut self, event: Event) {
        self.gone = self.gone || self.events.send_blocking(event).is_err();
    }

    fn flush(&mut self) {
        if !self.batch.is_empty() {
            let batch = std::mem::replace(&mut self.batch, Vec::with_capacity(BATCH));
            self.send(Event::Rows(batch));
        }
    }
}

impl Sink for ToWindow<'_> {
    fn columns(&mut self, columns: Vec<ColumnInfo>) -> Flow {
        (self.rows, self.bytes, self.cut) = (0, 0, false);
        self.send(Event::Result { statement: self.place.clone(), columns });
        if self.gone { Flow::Stop } else { Flow::Go }
    }

    fn row(&mut self, row: Row, bytes: usize) -> Flow {
        self.batch.push(row);
        self.rows += 1;
        self.bytes += bytes;
        if self.batch.len() == BATCH {
            self.flush();
        }
        self.cut = self.rows >= MOST_ROWS || self.bytes >= MOST_BYTES;
        if self.cut || self.gone || self.stopping.is_stopped() { Flow::Stop } else { Flow::Go }
    }

    fn done(&mut self, affected: u64) {
        self.flush();
        if self.cut || !self.stopping.is_stopped() {
            let done = Event::ResultDone { statement: self.place.clone(), affected, took: self.began.elapsed(), cut: self.cut };
            self.send(done);
        }
    }
}

/// False when the session is over.
fn run_all(db: &mut dyn Backend, statements: Vec<(Range<usize>, String)>, events: &Sender<Event>, stopping: &Stopping) -> bool {
    for (place, sql) in statements {
        if stopping.is_stopped() {
            break;
        }
        stopping.running.store(true, Ordering::Release);
        let mut sink = ToWindow { events, stopping, place: place.clone(), began: Instant::now(), batch: Vec::with_capacity(BATCH), rows: 0, bytes: 0, cut: false, gone: false };
        let outcome = db.run(&sql, &mut sink);
        sink.flush();
        let gone = sink.gone;
        stopping.running.store(false, Ordering::Release);
        if gone {
            return false;
        }
        match outcome {
            Ok(()) => {}
            // What was asked for: the rows read so far stay, `Idle` says it stopped.
            Err(DbError::Interrupted) if stopping.is_stopped() => break,
            Err(DbError::Gone(text)) => {
                let _ = events.send_blocking(Event::Failed { statement: place, message: text, at: None });
                let _ = events.send_blocking(Event::Lost("The connection was lost".into()));
                return false;
            }
            Err(e) => {
                if events.send_blocking(Event::Failed { statement: place, at: e.position(), message: e.to_string() }).is_err() {
                    return false;
                }
                break;
            }
        }
    }
    events.send_blocking(Event::Idle { stopped: stopping.is_stopped() }).is_ok()
}

/// A statement's rows, on their way to a file.
struct ToFile<'a> {
    file: Option<std::io::BufWriter<std::fs::File>>,
    writer: Option<Writer<std::io::BufWriter<std::fs::File>>>,
    format: Format,
    table: &'a str,
    driver: Driver,
    stopping: &'a Stopping,
    failed: Option<std::io::Error>,
}

impl Sink for ToFile<'_> {
    fn columns(&mut self, columns: Vec<ColumnInfo>) -> Flow {
        // The first result: a call that returns several exports one.
        let Some(file) = self.file.take() else { return Flow::Stop };
        match Writer::start(file, self.format, columns.into_iter().map(|c| c.name).collect(), self.table, self.driver) {
            Ok(writer) => {
                self.writer = Some(writer);
                Flow::Go
            }
            Err(e) => {
                self.failed = Some(e);
                Flow::Stop
            }
        }
    }

    fn row(&mut self, row: Row, _: usize) -> Flow {
        let Some(writer) = &mut self.writer else { return Flow::Stop };
        if let Err(e) = writer.row(&row) {
            self.failed = Some(e);
            return Flow::Stop;
        }
        if self.stopping.is_stopped() { Flow::Stop } else { Flow::Go }
    }

    fn done(&mut self, _: u64) {}
}

/// All the rows of `sql` into a file, written beside it and moved into place
/// when whole: a failed export leaves no half of a file under the name asked for.
fn export(db: &mut dyn Backend, sql: &str, format: Format, path: &Path, table: &str, driver: Driver, stopping: &Stopping) -> Result<u64> {
    let partial = path.with_extension(format!("{}.part", format.extension()));
    let written = (|| -> Result<u64> {
        let file = std::io::BufWriter::new(std::fs::File::create(&partial)?);
        let mut sink = ToFile { file: Some(file), writer: None, format, table, driver, stopping, failed: None };
        db.run(sql, &mut sink)?;
        if let Some(e) = sink.failed.take() {
            return Err(e.into());
        }
        anyhow::ensure!(!stopping.is_stopped(), "stopped");
        let writer = sink.writer.take().ok_or_else(|| anyhow::anyhow!("the statement returns no rows"))?;
        Ok(writer.finish()?)
    })();
    match written {
        Ok(rows) => {
            std::fs::rename(&partial, path)?;
            Ok(rows)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}
