//! The connections, in a file of their own: hosts and logins do not travel
//! with `state.json`, which gets copied into backups and bug reports, and
//! never go into a project's `.ideae.toml`. Their passwords are not here.
//!
//! Read once in the background at the start and kept in memory; a change is
//! written off the UI thread, and at the latest when the app quits.

use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};

/// Files whose connection is remembered, the latest first.
const FILES_KEPT: usize = 300;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SshHop {
    pub host: String,
    #[serde(default = "ssh_port")]
    pub port: u16,
    pub user: String,
    /// The private key; none leaves the choice to ssh and its agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<PathBuf>,
}

fn ssh_port() -> u16 {
    22
}

/// Which kind of server it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Driver {
    #[default]
    MySql,
    Postgres,
    Redis,
}

impl Driver {
    pub const ALL: [Driver; 3] = [Driver::MySql, Driver::Postgres, Driver::Redis];

    pub fn name(self) -> &'static str {
        match self {
            Driver::MySql => "MySQL",
            Driver::Postgres => "PostgreSQL",
            Driver::Redis => "Redis",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Driver::MySql => 3306,
            Driver::Postgres => 5432,
            Driver::Redis => 6379,
        }
    }

    /// A name as this server reads it, whatever it holds.
    pub fn quoted(self, name: &str) -> String {
        match self {
            Driver::Postgres => name.split('.').map(|part| format!("\"{}\"", part.replace('"', "\"\""))).collect::<Vec<_>>().join("."),
            _ => format!("`{}`", name.replace('`', "``")),
        }
    }

    /// A string as this server reads it: a backslash is a plain character in PostgreSQL's.
    pub fn string(self, text: &str) -> String {
        let doubled = text.replace('\'', "''");
        match self {
            Driver::Postgres => format!("'{doubled}'"),
            _ => format!("'{}'", doubled.replace('\\', "\\\\").replace('\0', "\\0").replace('\n', "\\n").replace('\r', "\\r")),
        }
    }
}

/// A bastion several databases can go through: set up once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bastion {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub hop: SshHop,
}

/// How a connection is encrypted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    Off,
    /// Encrypted when the server can; plain when it cannot.
    #[default]
    Prefer,
    /// Encrypted or not at all, whoever's certificate it is.
    Require,
    /// Encrypted, and the certificate is the server's.
    Verify,
}

impl TlsMode {
    pub const ALL: [TlsMode; 4] = [TlsMode::Off, TlsMode::Prefer, TlsMode::Require, TlsMode::Verify];

    pub fn name(self) -> &'static str {
        match self {
            TlsMode::Off => "Off",
            TlsMode::Prefer => "Prefer",
            TlsMode::Require => "Require",
            TlsMode::Verify => "Verify",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tls {
    #[serde(default)]
    pub mode: TlsMode,
    /// The certificate authority to check against, else the public ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub driver: Driver,
    /// As the bastion knows it, when there is one.
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// The bastion it goes through; none is a direct connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bastion: Option<String>,
    /// That bastion, filled in when read. On disk only in files of an older
    /// version, which held a copy in each connection.
    #[serde(default, skip_serializing)]
    pub ssh: Option<SshHop>,
    #[serde(default)]
    pub tls: Tls,
    /// A mark of [`COLORS`], to tell production from the rest at a glance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u8>,
    /// Production: nothing runs on it until unlocked (`guard`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub production: bool,
}

/// The marks a connection can have: name and hue.
pub const COLORS: [(&str, f32); 5] = [("Red", 0.0), ("Orange", 0.08), ("Green", 0.38), ("Blue", 0.6), ("Purple", 0.77)];

/// What a `.sql` file runs against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChoice {
    pub path: PathBuf,
    pub connection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
}

#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    connections: Vec<Connection>,
    #[serde(default)]
    bastions: Vec<Bastion>,
    #[serde(default)]
    files: Vec<FileChoice>,
    /// How long production stays unlocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    production_minutes: Option<u32>,
}

impl Store {
    /// The bastion with these settings, made if there is none.
    fn bastion_for(&mut self, hop: &SshHop) -> String {
        if let Some(there) = self.bastions.iter().find(|b| b.hop == *hop) {
            return there.id.clone();
        }
        let login = if hop.user.is_empty() { hop.host.clone() } else { format!("{}@{}", hop.user, hop.host) };
        let id = new_id();
        self.bastions.push(Bastion { id: id.clone(), name: login, hop: hop.clone() });
        id
    }

    /// A connection that holds its own copy of a bastion goes to share one.
    fn share_bastions(&mut self) -> bool {
        let mut changed = false;
        for ix in 0..self.connections.len() {
            if self.connections[ix].bastion.is_none()
                && let Some(hop) = self.connections[ix].ssh.take()
            {
                let id = self.bastion_for(&hop);
                self.connections[ix].bastion = Some(id);
                changed = true;
            }
        }
        changed
    }

    fn resolved(&self, connection: &Connection) -> Connection {
        let mut out = connection.clone();
        out.ssh = out.bastion.as_ref().and_then(|id| self.bastions.iter().find(|b| b.id == *id)).map(|b| b.hop.clone());
        out
    }
}

static STORE: RwLock<Option<Store>> = RwLock::new(None);
static UNSAVED: AtomicBool = AtomicBool::new(false);
static WRITING: Mutex<()> = Mutex::new(());

fn path() -> PathBuf {
    crate::state::state_dir().join("connections.json")
}

/// Read off the UI thread at the start.
pub fn preload() {
    std::thread::spawn(|| {
        let read = load_from(&path());
        let mut store = STORE.write().unwrap_or_else(|e| e.into_inner());
        if store.is_none() {
            *store = Some(read);
        }
    });
}

fn with<T>(read: impl FnOnce(&Store) -> T) -> T {
    {
        let store = STORE.read().unwrap_or_else(|e| e.into_inner());
        if let Some(store) = store.as_ref() {
            return read(store);
        }
    }
    // A file restored before `preload` was done must not find no connection,
    // and so forget its own: this small read, once, is the lesser harm.
    let mut store = STORE.write().unwrap_or_else(|e| e.into_inner());
    read(store.get_or_insert_with(|| load_from(&path())))
}

fn change(apply: impl FnOnce(&mut Store)) {
    {
        let mut store = STORE.write().unwrap_or_else(|e| e.into_inner());
        // Changed before the first read was done: that read is needed now.
        apply(store.get_or_insert_with(|| load_from(&path())));
    }
    UNSAVED.store(true, Ordering::Release);
    std::thread::spawn(flush);
}

/// Writes what changed. Waits for a write under way.
pub fn flush() {
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    if !UNSAVED.swap(false, Ordering::AcqRel) {
        return;
    }
    let Some(store) = STORE.read().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    if let Err(e) = save_to(&path(), &store) {
        eprintln!("connections: {e}");
        UNSAVED.store(true, Ordering::Release);
    }
}

pub fn list() -> Vec<Connection> {
    with(|s| s.connections.iter().map(|c| s.resolved(c)).collect())
}

pub fn find(id: &str) -> Option<Connection> {
    with(|s| s.connections.iter().find(|c| c.id == id).map(|c| s.resolved(c)))
}

/// Adds it, or replaces the one with its id. One that comes with SSH
/// settings and no bastion (an import) gets the bastion that has them.
pub fn put(mut connection: Connection) {
    change(|s| {
        if connection.bastion.is_none()
            && let Some(hop) = connection.ssh.take()
        {
            connection.bastion = Some(s.bastion_for(&hop));
        }
        connection.ssh = None;
        match s.connections.iter_mut().find(|c| c.id == connection.id) {
            Some(there) => *there = connection,
            None => s.connections.push(connection),
        }
    });
}

/// One of the choices offered; anything else written into the file is the default.
pub fn production_minutes() -> u32 {
    with(|s| s.production_minutes).filter(|m| super::guard::MINUTES.contains(m)).unwrap_or(10)
}

pub fn set_production_minutes(minutes: u32) {
    change(|s| s.production_minutes = Some(minutes));
}

pub fn bastions() -> Vec<Bastion> {
    with(|s| s.bastions.clone())
}

pub fn find_bastion(id: &str) -> Option<Bastion> {
    with(|s| s.bastions.iter().find(|b| b.id == id).cloned())
}

/// What goes through this bastion.
pub fn bastion_uses(id: &str) -> usize {
    with(|s| s.connections.iter().filter(|c| c.bastion.as_deref() == Some(id)).count())
}

pub fn put_bastion(bastion: Bastion) {
    change(|s| match s.bastions.iter_mut().find(|b| b.id == bastion.id) {
        Some(there) => *there = bastion,
        None => s.bastions.push(bastion),
    });
}

/// Not while a connection goes through it: that one would quietly start
/// trying to reach an inner host directly.
pub fn remove_bastion(id: &str) -> bool {
    if bastion_uses(id) > 0 {
        return false;
    }
    change(|s| s.bastions.retain(|b| b.id != id));
    true
}

pub fn remove(id: &str) {
    change(|s| {
        s.connections.retain(|c| c.id != id);
        s.files.retain(|f| f.connection != id);
    });
}

/// What this file was told to run against. Never what another file last
/// used: a migration opened in some project must not be one key from production.
pub fn recall(file: &Path) -> Option<FileChoice> {
    with(|s| s.files.iter().find(|f| f.path == file && s.connections.iter().any(|c| c.id == f.connection)).cloned())
}

pub fn remember(file: &Path, connection: &str, database: Option<&str>) {
    let choice = FileChoice { path: file.to_path_buf(), connection: connection.to_string(), database: database.map(String::from) };
    if with(|s| s.files.first() == Some(&choice)) {
        return;
    }
    change(|s| {
        s.files.retain(|f| f.path != file);
        s.files.insert(0, choice);
        s.files.truncate(FILES_KEPT);
    });
}

/// A file went by another name: what it runs against goes with it.
pub fn file_moved(from: &Path, to: &Path) {
    let to = to.to_path_buf();
    let from = from.to_path_buf();
    change(move |s| {
        for choice in s.files.iter_mut().filter(|f| f.path == from) {
            choice.path = to.clone();
        }
    });
}

fn load_from(path: &Path) -> Store {
    let Ok(bytes) = std::fs::read(path) else { return Store::default() };
    match serde_json::from_slice::<Store>(&bytes) {
        Ok(mut store) => {
            if store.share_bastions() {
                UNSAVED.store(true, Ordering::Release);
            }
            store
        }
        Err(e) => {
            // Kept for a look: the next save would write over it.
            eprintln!("connections {}: {e}", path.display());
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            Store::default()
        }
    }
}

fn save_to(path: &Path, store: &Store) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(store).map_err(std::io::Error::other)?;
    write_private(path, &json)
}

/// Whole or not at all, and for its owner only.
pub fn write_private(path: &Path, json: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&tmp)?, json)?;
    std::fs::rename(&tmp, path)
}

/// An id for a new connection: what its password is kept under.
pub fn new_id() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 8];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        bytes = (u64::from(std::process::id()) ^ crate::state::now() << 20).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Store {
        Store {
            connections: vec![Connection {
                id: "a1".into(),
                name: "Stage".into(),
                driver: Driver::MySql,
                host: "db-internal".into(),
                port: 3306,
                user: "me".into(),
                database: Some("app".into()),
                bastion: Some("b1".into()),
                ssh: None,
                tls: Tls { mode: TlsMode::Verify, ca: Some("/keys/ca.pem".into()) },
                color: Some(0),
                production: false,
            }],
            bastions: vec![Bastion { id: "b1".into(), name: "Main".into(), hop: SshHop { host: "bastion.example".into(), port: 22, user: "me".into(), key: Some("/keys/id".into()) } }],
            files: vec![FileChoice { path: "/work/a.sql".into(), connection: "a1".into(), database: Some("app".into()) }],
            production_minutes: None,
        }
    }

    #[test]
    fn connections_come_back_as_saved_and_only_their_owner_reads_them() {
        let dir = std::env::temp_dir().join(format!("ideae-db-config-{}", new_id()));
        let path = dir.join("connections.json");
        save_to(&path, &sample()).unwrap();
        assert_eq!(load_from(&path), sample());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn connections_that_held_one_bastion_each_come_to_share_it() {
        let dir = std::env::temp_dir().join(format!("ideae-db-config-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connections.json");
        let one = |id: &str, host: &str| format!(r#"{{"id":"{id}","name":"{id}","host":"db","port":3306,"user":"me","ssh":{{"host":"{host}","port":22,"user":"me"}}}}"#);
        let direct = r#"{"id":"c","name":"c","host":"db","port":3306,"user":"me"}"#;
        std::fs::write(&path, format!(r#"{{"connections":[{},{},{direct}]}}"#, one("a", "b1.example"), one("b", "b1.example"))).unwrap();
        let store = load_from(&path);
        assert_eq!(store.bastions.len(), 1);
        assert_eq!(store.connections[0].bastion, store.connections[1].bastion);
        assert_eq!(store.connections[2].bastion, None, "a direct one stays direct");
        assert_eq!(store.connections[2].tls.mode, TlsMode::Prefer, "and is encrypted where the server can");
        assert_eq!(store.resolved(&store.connections[0]).ssh.unwrap().host, "b1.example");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unreadable_file_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("ideae-db-config-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connections.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load_from(&path), Store::default());
        assert!(path.with_extension("json.bad").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
