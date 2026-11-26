//! This machine's store (ideae-core's `store`): kept by the daemon for
//! every window attached to it, by the window itself where there is none
//! (Windows, IDEAE_NO_DAEMON). The window's model stays as it is; at each
//! save what it changed of the machine's (projects, tasks, their work, crew,
//! the machine's settings) since it last took the store in goes to the
//! store, and state.json is written as before, whole, for older builds,
//! with the store's rev. What another window changed is taken in by the
//! window's model ([`changed_elsewhere`], `Workspace::take_in_store`).
//! At the start the store is what the projects and tasks are, the
//! window's own (tabs, spaces, folded rows) from state.json.
//!
//! `IDEAE_NO_STORE=1`: none of it, state.json alone as before.

use std::sync::Mutex;

use ideae_core::store::{self, Machine, WindowId, host::Host};

use crate::state::{self, State};

/// The window here, as a store it keeps itself knows it.
const HERE: WindowId = 1;

enum Keeper {
    Here(Host),
    #[cfg(unix)]
    Daemon(std::sync::Arc<crate::daemon::client::Daemon>),
}

struct Held {
    keeper: Keeper,
    /// The store as this window's model has it: what its changes are
    /// counted from. Moved on by its own changes and by what it took in.
    seen: Machine,
    /// New each time the model takes in another window's changes, and for
    /// each store opened: a snapshot taken before (or by a window of a store
    /// before, as a test's at its end) is not of the model as it is.
    taken_in: u64,
    /// Which store this is: given to the window that opened it.
    id: u64,
}

static MARK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn new_mark() -> u64 {
    MARK.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);
/// What the start found to tell (state.json and the store both changed).
static NOTE: Mutex<Option<String>> = Mutex::new(None);

/// Something to say at the start, once.
pub fn take_note() -> Option<String> {
    NOTE.lock().unwrap().take()
}

fn as_machine(state: &State) -> Result<Machine, String> {
    store::migrate::from_state(&state::for_store(state).map_err(|e| e.to_string())?)
}

#[cfg(unix)]
fn ask(daemon: &crate::daemon::client::Daemon, call: crate::daemon::proto::Call, body: &[u8]) -> Result<Vec<u8>, String> {
    let (reply, out) = daemon.call(&call, body).map_err(|e| e.to_string())?;
    reply.error.map_or(Ok(out), Err)
}

/// Opens the store at its keeper, made of `state` the first time.
fn open_at(state: &State) -> Result<(Keeper, Machine, Option<String>), String> {
    let value = state::for_store(state).map_err(|e| e.to_string())?;
    let (dir, file) = (state::state_dir(), state::state_path());
    #[cfg(unix)]
    if let Some(daemon) = crate::daemon::client::get().filter(|_| std::env::var_os("IDEAE_NO_DAEMON").is_none()) {
        let call = crate::daemon::proto::Call::StoreOpen { dir, state_file: file, machine_rev: state.machine_rev };
        let out = ask(&daemon, call, &serde_json::to_vec(&value).map_err(|e| e.to_string())?)?;
        #[derive(serde::Deserialize)]
        struct Opened {
            machine: Machine,
            note: Option<String>,
        }
        let opened: Opened = serde_json::from_slice(&out).map_err(|e| e.to_string())?;
        return Ok((Keeper::Daemon(daemon), opened.machine, opened.note));
    }
    let (host, note) = Host::open(&dir, &file, &value, state.machine_rev, false)?;
    let machine = host.machine().clone();
    Ok((Keeper::Here(host), machine, note))
}

/// Takes this machine's store and puts its projects and tasks into
/// `state`. Without it (`IDEAE_NO_STORE`, another ideae keeps it, it
/// cannot be read) `state` is as it was read.
/// Which store it is (0: none), for the window's saves (`taken_in`).
pub fn open(state: &mut State) -> u64 {
    // A test of no state of its own leaves alone the store another test beside it opened.
    if cfg!(test) && std::env::var_os("IDEAE_STATE").is_none() {
        return 0;
    }
    *HELD.lock().unwrap() = None;
    // Not closed: that would wake the one who waits on it (a window gone, a test before) from here.
    if let Some(before) = CHANGED.lock().unwrap().take() {
        KEPT.lock().unwrap().push(before);
    }
    if std::env::var_os("IDEAE_NO_STORE").is_some() {
        return 0;
    }
    match open_at(state) {
        Ok((keeper, machine, note)) => {
            if cfg!(test) {
                eprintln!("DIAG open state={:?} note={note:?} rev={} store={:?}", state::state_path(), machine.rev, machine.projects.iter().map(|p| (&p.path, p.tasks.iter().map(|t| &t.path).collect::<Vec<_>>())).collect::<Vec<_>>());
            }
            if note.is_some() {
                *NOTE.lock().unwrap() = note;
            }
            overlay(state, &machine);
            state.machine_rev = machine.rev;
            let id = new_mark();
            *HELD.lock().unwrap() = Some(Held { keeper, seen: machine, taken_in: id, id });
            *CHANGED.lock().unwrap() = Some(async_channel::unbounded());
            id
        }
        Err(e) => {
            eprintln!("ideae: this machine's store not opened, its projects and tasks from state.json alone: {e}");
            0
        }
    }
}

/// Where the model of the window that opened store `store` is: a snapshot
/// taken now is of it ([`sync`]). 0, never brought, for a window of another
/// store (a test's, at its end, when the next opened its own).
pub fn taken_in(store: u64) -> u64 {
    HELD.lock().unwrap().as_ref().filter(|h| h.id == store).map_or(0, |h| h.taken_in)
}

/// Brings what `state` changed of the machine's into the store and marks
/// `state` with the store's rev; before state.json is written. A snapshot
/// taken before the model took in another window's changes (`taken_in`
/// then) is not brought: the save after it is.
/// `aside`: tasks the snapshot leaves out while they are busy, kept in the
/// store as they were.
pub fn sync(state: &mut State, taken_in: u64, aside: &[std::path::PathBuf]) {
    let all = match as_machine(state) {
        Ok(all) => all,
        Err(e) => {
            eprintln!("ideae: the store not brought up to date: {e}");
            return;
        }
    };
    let (local, remote) = split(all);
    sync_local(state, taken_in, aside, local);
    #[cfg(unix)]
    {
        // Over ssh: not on the thread that saves (a save may be on the UI thread), in order.
        if !remote.is_empty() {
            to_machines(Job { taken_in, aside: aside.to_vec(), remote });
        }
        strip_remote(state);
    }
    #[cfg(not(unix))]
    let _ = remote;
}

/// This machine's own: its projects (none of another machine), the crew,
/// its settings; and each other machine's projects, by host.
fn split(mut all: Machine) -> (Machine, std::collections::BTreeMap<String, Machine>) {
    let mut remote: std::collections::BTreeMap<String, Machine> = std::collections::BTreeMap::new();
    let (mine, theirs): (Vec<_>, Vec<_>) = all.projects.drain(..).partition(|p| !crate::hosts::is_remote(&p.path));
    all.projects = mine;
    for project in theirs {
        if let Some((host, _)) = crate::hosts::split(&project.path) {
            remote.entry(host).or_default().projects.push(project);
        }
    }
    (all, remote)
}

fn sync_local(state: &mut State, taken_in: u64, aside: &[std::path::PathBuf], mut wanted: Machine) {
    if cfg!(test) {
        eprintln!(
            "DIAG sync taken_in={taken_in} aside={aside:?} held={:?} wanted={:?}",
            HELD.lock().unwrap().as_ref().map(|h| (h.id, h.taken_in, h.seen.projects.iter().map(|p| (&p.path, p.tasks.iter().map(|t| &t.path).collect::<Vec<_>>())).collect::<Vec<_>>())),
            wanted.projects.iter().map(|p| (&p.path, p.tasks.iter().map(|t| &t.path).collect::<Vec<_>>())).collect::<Vec<_>>()
        );
    }
    // What it changed, worked out under the lock (in memory); the daemon asked out of it.
    let mut held = HELD.lock().unwrap();
    let Some(h) = held.as_mut() else { return };
    if taken_in != h.taken_in {
        if taken_in != 0 {
            state.machine_rev = h.seen.rev;
        }
        return;
    }
    for path in aside {
        let Some((pi, ti)) = h.seen.projects.iter().enumerate().find_map(|(pi, p)| p.tasks.iter().position(|t| &t.path == path).map(|ti| (pi, ti))) else { continue };
        let was = &h.seen.projects[pi];
        if let Some(project) = wanted.projects.iter_mut().find(|p| p.path == was.path).filter(|p| !p.tasks.iter().any(|t| &t.path == path)) {
            let at = ti.min(project.tasks.len());
            project.tasks.insert(at, was.tasks[ti].clone());
        }
    }
    let synced: Result<(u64, Vec<String>), String> = match &mut h.keeper {
        Keeper::Here(host) => {
            let (rev, refused) = host.sync(HERE, &h.seen, &wanted, true);
            Ok((rev, refused.iter().map(ToString::to_string).collect()))
        }
        #[cfg(unix)]
        Keeper::Daemon(daemon) => {
            let daemon = daemon.clone();
            let body = serde_json::to_vec(&serde_json::json!({ "base": &h.seen, "wanted": &wanted, "mirrors": true })).unwrap_or_default();
            let seen_rev = h.seen.rev;
            drop(held);
            let mut asked = ask(&daemon, crate::daemon::proto::Call::StoreSync, &body);
            // A daemon started since (handed over, or after a crash): opened again, then asked again.
            if asked.as_ref().is_err_and(|e| e == ideae_core::daemon::server::STORE_NOT_OPEN) {
                let mut again = state.clone();
                again.machine_rev = seen_rev;
                if let Ok((Keeper::Daemon(daemon), _, _)) = open_at(&again) {
                    asked = ask(&daemon, crate::daemon::proto::Call::StoreSync, &body);
                }
            }
            #[derive(serde::Deserialize)]
            struct Synced {
                rev: u64,
                refused: Vec<String>,
            }
            held = HELD.lock().unwrap();
            asked.and_then(|out| serde_json::from_slice::<Synced>(&out).map_err(|e| e.to_string())).map(|s| (s.rev, s.refused))
        }
    };
    let Some(h) = held.as_mut() else { return };
    let (rev, refused) = match synced {
        Ok(synced) => synced,
        Err(e) => {
            eprintln!("ideae: the store not brought up to date: {e}");
            // Not in the store: state.json alone has it, the next start takes it from there.
            state.machine_rev = 0;
            return;
        }
    };
    for refused in &refused {
        eprintln!("ideae: the store refused: {refused}");
    }
    // What it refused is not the store's: the store is what is, taken in (a project another window
    // removed, a path another task has). Not state.json made the store again, which drops others' work.
    if !refused.is_empty() {
        tell(None);
    }
    if h.taken_in != taken_in {
        // The model took another window's changes in while the daemon was asked: what it
        // counts from is that fetch; its own changes are in the store, taken in with the next.
        changed_elsewhere(rev);
    } else {
        // Its own changes are what it has now, ids and all: the store's, where it has them.
        let mut seen = wanted;
        keep_store_ids(&mut seen, &h.seen);
        // Tasks it made: what the store gave them (ids, a slot or number another had) is fetched and taken in.
        if seen.projects.iter().flat_map(|p| &p.tasks).any(|t| t.id == 0) {
            changed_elsewhere(rev);
        }
        seen.rev = rev;
        h.seen = seen;
    }
    state.machine_rev = rev;
}

/// `seen`'s tasks with the ids `before` knew them by; new ones as yet
/// without one (the store gave them theirs, taken in with the next fetch).
fn keep_store_ids(seen: &mut Machine, before: &Machine) {
    for project in &mut seen.projects {
        let was = before.projects.iter().find(|p| p.path == project.path);
        for task in &mut project.tasks {
            task.id = was.and_then(|p| p.tasks.iter().find(|t| t.path == task.path)).map_or(0, |t| t.id);
        }
    }
}

/// An id given by the store now, never again: a crew record's, or (with
/// `question_of`) a question's of that task. None without a store: the
/// window gives its own. Off the UI thread.
pub fn reserve(question_of: Option<&std::path::Path>) -> Option<u32> {
    // A question of another machine's task: its store's.
    #[cfg(unix)]
    if let Some((host, there)) = question_of.and_then(crate::hosts::split) {
        let daemon = HOSTS.lock().unwrap().get(&host)?.daemon.clone();
        let call = crate::daemon::proto::Call::StoreReserve { question_of: Some(there) };
        let out = ask_there(&daemon, call, b"").inspect_err(|e| eprintln!("ideae: no id from the store of {host}: {e}")).ok()?;
        return String::from_utf8_lossy(&out).trim().parse().ok();
    }
    let op = match question_of {
        Some(task) => store::Op::ReserveQuestion { task: task.to_path_buf() },
        None => store::Op::ReserveCrew,
    };
    let mut held = HELD.lock().unwrap();
    match &mut held.as_mut()?.keeper {
        Keeper::Here(host) => host.reserve(HERE, op).inspect_err(|e| eprintln!("ideae: no id from the store: {e}")).ok(),
        #[cfg(unix)]
        Keeper::Daemon(daemon) => {
            let daemon = daemon.clone();
            drop(held);
            let call = crate::daemon::proto::Call::StoreReserve { question_of: question_of.map(std::path::Path::to_path_buf) };
            let out = ask(&daemon, call, b"").inspect_err(|e| eprintln!("ideae: no id from the store: {e}")).ok()?;
            String::from_utf8_lossy(&out).trim().parse().ok()
        }
    }
}

/// Another window changed the store (its daemon said so): the windows here
/// fetch it and take it in.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn changed_elsewhere(rev: u64) {
    let _ = rev;
    tell(None);
}

/// The store of `host` changed, or this machine's (None): its window fetches it.
fn tell(host: Option<String>) {
    if let Some((tx, _)) = CHANGED.lock().unwrap().as_ref() {
        let _ = tx.try_send(host);
    }
}

type Channel = (async_channel::Sender<Option<String>>, async_channel::Receiver<Option<String>>);
/// Of the store opened last: the window that opened it waits on it.
static CHANGED: Mutex<Option<Channel>> = Mutex::new(None);
/// Those of stores opened before (once a run in the app; once a test in tests).
static KEPT: Mutex<Vec<Channel>> = Mutex::new(Vec::new());

/// What the window waits on to take in others' changes; none without a store.
pub fn changes_elsewhere() -> Option<async_channel::Receiver<Option<String>>> {
    CHANGED.lock().unwrap().as_ref().map(|(_, rx)| rx.clone())
}

/// The store as it is now, from its keeper: off the UI thread.
pub fn fetch() -> Option<Machine> {
    let held = HELD.lock().unwrap();
    match &held.as_ref()?.keeper {
        Keeper::Here(host) => Some(host.machine().clone()),
        #[cfg(unix)]
        Keeper::Daemon(daemon) => {
            let daemon = daemon.clone();
            drop(held);
            let out = ask(&daemon, crate::daemon::proto::Call::StoreGet, b"").inspect_err(|e| eprintln!("ideae: the store not fetched: {e}")).ok()?;
            serde_json::from_slice(&out).ok()
        }
    }
}

/// What a fetched store is to the model.
pub enum Fetched {
    /// What it has the model has not: taken in now.
    Changes(store::sync::Changes),
    /// Older than what this window brought since: fetched again.
    Stale,
    Nothing,
}

/// What of `now` (fetched) the model has not yet: others' changes and what
/// the store gave its own (ids, a ports' slot or variant number another
/// task had); `now` is what the model is counted from after. For the UI
/// thread, which takes the changes in at once: a snapshot taken before is
/// not brought to the store (`taken_in`).
pub fn take_in(now: Machine) -> Fetched {
    let mut held = HELD.lock().unwrap();
    let Some(held) = held.as_mut() else { return Fetched::Nothing };
    // Fetched before this window's own sync went in: it would take that back.
    if now.rev < held.seen.rev {
        return Fetched::Stale;
    }
    let changes = store::sync::changes(&held.seen, &now);
    held.seen = now;
    if changes.is_empty() {
        return Fetched::Nothing;
    }
    // Only a model that takes something in is not what a snapshot before was.
    held.taken_in = new_mark();
    Fetched::Changes(changes)
}

// ------------------------------------------------------------ other machines

/// A machine of another host's store, as this window has it: its projects
/// keyed as the window keys them (`ssh:/<host>/<path there>`). Only the
/// projects this window shows go to it; one it no longer shows is left to
/// the machine (removed here, it is hidden, not taken from the others').
#[cfg(unix)]
struct There {
    daemon: std::sync::Arc<crate::daemon::client::Daemon>,
    seen: Machine,
    /// Taken in once since it connected: before, the window's model has no
    /// more of its projects than state.json's tabs, nothing to bring.
    taken: bool,
}

#[cfg(unix)]
static HOSTS: Mutex<std::collections::BTreeMap<String, There>> = Mutex::new(std::collections::BTreeMap::new());

/// `machine`'s paths, each as `to` gives it.
#[cfg(unix)]
fn rekey(machine: &mut Machine, to: &dyn Fn(&std::path::Path) -> std::path::PathBuf) {
    for p in &mut machine.projects {
        p.path = to(&p.path);
        p.made_by_agents = std::mem::take(&mut p.made_by_agents).into_iter().map(|(made, by)| (to(&made), to(&by))).collect();
        for t in &mut p.tasks {
            t.path = to(&t.path);
            t.spawned_by = t.spawned_by.as_deref().map(to);
            for q in &mut t.questions {
                q.asked_by = q.asked_by.as_deref().map(to);
            }
        }
    }
    for c in &mut machine.crew {
        c.parent = to(&c.parent);
        c.path = c.path.as_deref().map(to);
    }
}

/// Keyed as the window keys `host`'s: from its paths there.
#[cfg(unix)]
fn keyed(mut machine: Machine, host: &str) -> Machine {
    rekey(&mut machine, &|p| if p.is_absolute() { crate::hosts::join(host, p) } else { p.to_path_buf() });
    machine
}

/// As `host` has them: its paths there.
#[cfg(unix)]
fn there(mut machine: Machine, host: &str) -> Machine {
    rekey(&mut machine, &|p| match crate::hosts::split(p) {
        Some((h, there)) if h == host => there,
        _ => p.to_path_buf(),
    });
    machine
}

/// What `state` has of `host`'s projects, keyed: the window's own view of them.
#[cfg(unix)]
pub fn of_host(state: &State, host: &str) -> Machine {
    as_machine(state).map(|all| split(all).1.remove(host).unwrap_or_default()).unwrap_or_default()
}

/// `machine` with only the projects `shown`.
#[cfg(unix)]
fn shown(machine: &Machine, shown: &[std::path::PathBuf]) -> Machine {
    Machine { projects: machine.projects.iter().filter(|p| shown.contains(&p.path)).cloned().collect(), ..Default::default() }
}

/// The store of `host`, its daemon there connected: the machine's own,
/// made of its state.json the first time (its own ideae's). Off the UI
/// thread; what of it the window shows is taken in with `take_in_host`.
#[cfg(unix)]
pub fn open_remote(host: &str, daemon: std::sync::Arc<crate::daemon::client::Daemon>) -> Result<Machine, String> {
    let out = ask(&daemon, crate::daemon::proto::Call::StoreOpenHere, b"")?;
    #[derive(serde::Deserialize)]
    struct Opened {
        machine: Machine,
    }
    let opened: Opened = serde_json::from_slice(&out).map_err(|e| e.to_string())?;
    let now = keyed(opened.machine, host);
    HOSTS.lock().unwrap().insert(host.to_string(), There { daemon, seen: Machine::default(), taken: false });
    Ok(now)
}

/// The projects `host`'s store has, keyed, as this window last took it in.
#[cfg(unix)]
pub fn projects_there(host: &str) -> Vec<store::Project> {
    HOSTS.lock().unwrap().get(host).map(|t| t.seen.projects.clone()).unwrap_or_default()
}

/// `host`'s Machine layer and its agents' profiles, as its store has them.
#[cfg(unix)]
pub fn machine_settings(host: &str) -> Option<(serde_json::Value, Vec<serde_json::Value>)> {
    HOSTS.lock().unwrap().get(host).map(|t| (t.seen.settings.clone(), t.seen.profiles.clone()))
}

/// `call` asked of `host`'s daemon; one started since (that machine
/// restarted, upgraded) has its store opened again first.
#[cfg(unix)]
fn ask_there(daemon: &crate::daemon::client::Daemon, call: crate::daemon::proto::Call, body: &[u8]) -> Result<Vec<u8>, String> {
    match ask(daemon, call.clone(), body) {
        Err(e) if e == ideae_core::daemon::server::STORE_NOT_OPEN => {
            ask(daemon, crate::daemon::proto::Call::StoreOpenHere, b"")?;
            ask(daemon, call, body)
        }
        done => done,
    }
}

/// The daemon of a host said its store changed: the window fetches it.
#[cfg(unix)]
pub fn changed_remote(daemon: &crate::daemon::client::Daemon) {
    let host = HOSTS.lock().unwrap().iter().find(|(_, t)| std::ptr::eq(std::sync::Arc::as_ptr(&t.daemon), daemon)).map(|(h, _)| h.clone());
    if host.is_some() {
        tell(host);
    }
}

/// The store of `host` as it is now, keyed: off the UI thread.
#[cfg(unix)]
pub fn fetch_remote(host: &str) -> Option<Machine> {
    let daemon = HOSTS.lock().unwrap().get(host)?.daemon.clone();
    let out = ask_there(&daemon, crate::daemon::proto::Call::StoreGet, b"").inspect_err(|e| eprintln!("ideae: the store of {host} not fetched: {e}")).ok()?;
    serde_json::from_slice::<Machine>(&out).ok().map(|m| keyed(m, host))
}

/// What of `now` (fetched from `host`) the model has not yet, of the
/// projects it shows (`shown`; `first`: as the model has them now, the
/// first time after it connected). Moves on what the window counts from.
#[cfg(unix)]
pub fn take_in_host(host: &str, now: Machine, shown_now: &[std::path::PathBuf], first: Option<Machine>) -> Fetched {
    let mut hosts = HOSTS.lock().unwrap();
    let Some(there) = hosts.get_mut(host) else { return Fetched::Nothing };
    if first.is_none() && now.rev < there.seen.rev {
        return Fetched::Stale;
    }
    let changes = match first {
        // The first time: of the projects both have. One the store has not is this window's,
        // not yet brought (just added): it goes there with the next sync, it is not gone.
        Some(first) => {
            let both: Vec<std::path::PathBuf> = shown_now.iter().filter(|p| now.projects.iter().any(|n| &n.path == *p)).cloned().collect();
            store::sync::changes(&shown(&first, &both), &shown(&now, &both))
        }
        None => store::sync::changes(&shown(&there.seen, shown_now), &shown(&now, shown_now)),
    };
    there.seen = now;
    there.taken = true;
    drop(hosts);
    if changes.is_empty() {
        return Fetched::Nothing;
    }
    // A snapshot taken before is not of the model as it is now.
    if let Some(h) = HELD.lock().unwrap().as_mut() {
        h.taken_in = new_mark();
    }
    Fetched::Changes(changes)
}

/// A save's changes to other machines' projects, for their worker.
#[cfg(unix)]
struct Job {
    taken_in: u64,
    aside: Vec<std::path::PathBuf>,
    remote: std::collections::BTreeMap<String, Machine>,
}

/// Saves' jobs not yet done, and what tells the one waiting for them.
#[cfg(unix)]
static PENDING: (Mutex<usize>, std::sync::Condvar) = (Mutex::new(0), std::sync::Condvar::new());

/// Hands `job` to the one thread that brings saves to other machines, in their order.
#[cfg(unix)]
fn to_machines(job: Job) {
    static WORKER: std::sync::OnceLock<std::sync::mpsc::Sender<Job>> = std::sync::OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::spawn(move || {
            for job in rx {
                sync_remote(job.taken_in, &job.aside, job.remote);
                let (count, done) = &PENDING;
                *count.lock().unwrap() -= 1;
                done.notify_all();
            }
        });
        tx
    });
    *PENDING.0.lock().unwrap() += 1;
    if worker.send(job).is_err() {
        *PENDING.0.lock().unwrap() -= 1;
    }
}

/// Waits, `at_most`, for saves to reach other machines: the app quits.
pub fn flush_remote(at_most: std::time::Duration) {
    #[cfg(unix)]
    {
        let (count, done) = &PENDING;
        let pending = count.lock().unwrap();
        let _ = done.wait_timeout_while(pending, at_most, |n| *n > 0);
    }
    #[cfg(not(unix))]
    let _ = at_most;
}

/// What the window changed of each machine's projects it shows, brought
/// to its store (its daemon there connected, and taken in once).
#[cfg(unix)]
fn sync_remote(taken_in: u64, aside: &[std::path::PathBuf], remote: std::collections::BTreeMap<String, Machine>) {
    // A snapshot from before the model took something in is not brought (the next save is);
    // without a store of this machine's there is no such mark, and nothing to wait for.
    if HELD.lock().unwrap().as_ref().is_some_and(|h| h.taken_in != taken_in) {
        return;
    }
    for (host, mut wanted) in remote {
        let Some((daemon, base)) = HOSTS.lock().unwrap().get(&host).filter(|t| t.taken).map(|t| (t.daemon.clone(), shown(&t.seen, &wanted.projects.iter().map(|p| p.path.clone()).collect::<Vec<_>>()))) else { continue };
        // Its tasks busy here (connecting, being made) as the store has them.
        for path in aside.iter().filter(|p| crate::hosts::split(p).is_some_and(|(h, _)| h == host)) {
            let found = base.projects.iter().find_map(|p| p.tasks.iter().position(|t| &t.path == path).map(|ti| (p.path.clone(), ti, p.tasks[ti].clone())));
            if let Some((project, ti, task)) = found {
                if let Some(p) = wanted.projects.iter_mut().find(|p| p.path == project).filter(|p| !p.tasks.iter().any(|t| &t.path == path)) {
                    let at = ti.min(p.tasks.len());
                    p.tasks.insert(at, task);
                }
            }
        }
        // The machine's own settings and profiles are its own, not this window's.
        if store::sync::changes(&base, &wanted).is_empty() {
            continue;
        }
        let body = serde_json::to_vec(&serde_json::json!({ "base": there(base.clone(), &host), "wanted": there(wanted.clone(), &host) })).unwrap_or_default();
        match ask_there(&daemon, crate::daemon::proto::Call::StoreSync, &body) {
            Ok(out) => {
                let synced = serde_json::from_slice::<serde_json::Value>(&out).unwrap_or_default();
                for refused in synced["refused"].as_array().into_iter().flatten() {
                    eprintln!("ideae: the store of {host} refused: {refused}");
                }
                let mut hosts = HOSTS.lock().unwrap();
                if let Some(t) = hosts.get_mut(&host) {
                    // A fetch from before this is older than what it counts from now.
                    if let Some(rev) = synced["rev"].as_u64() {
                        t.seen.rev = t.seen.rev.max(rev);
                    }
                    // What it counts from: its own changes made over what it had.
                    for project in wanted.projects {
                        let mut project = project;
                        if let Some(was) = t.seen.projects.iter().find(|p| p.path == project.path) {
                            for task in &mut project.tasks {
                                task.id = was.tasks.iter().find(|w| w.path == task.path).map_or(0, |w| w.id);
                            }
                        }
                        match t.seen.projects.iter_mut().find(|p| p.path == project.path) {
                            Some(at) => *at = project,
                            None => t.seen.projects.push(project),
                        }
                    }
                }
                drop(hosts);
                // What the store gave (new tasks' ids, slots) and others' changes meanwhile.
                tell(Some(host));
            }
            Err(e) => eprintln!("ideae: the store of {host} not brought up to date: {e}"),
        }
    }
}

/// What state.json keeps of another machine's projects whose store this
/// window has: what is the window's alone (where it sorts each, its tabs,
/// which tasks it has open). Their work is the machine's, not this one's.
#[cfg(unix)]
fn strip_remote(state: &mut State) {
    // Of those taken in: their work is in their store (before, the model has no more than this).
    let hosts: Vec<String> = HOSTS.lock().unwrap().iter().filter(|(_, t)| t.taken).map(|(h, _)| h.clone()).collect();
    for project in &mut state.projects {
        if !crate::hosts::split(&project.path).is_some_and(|(h, _)| hosts.contains(&h)) {
            continue;
        }
        let tasks = std::mem::take(&mut project.tasks)
            .into_iter()
            .map(|t| {
                let mut session = t.session;
                session.review.clear();
                session.viewed.clear();
                session.bookmarks.clear();
                state::TaskState { path: t.path, branch: t.branch, main: t.main, open: t.open, session, ..Default::default() }
            })
            .collect();
        *project = state::ProjectState { path: project.path.clone(), space: project.space.clone(), space_id: project.space_id.clone(), collapsed: project.collapsed, tasks, ..Default::default() };
    }
}

/// Another window's change, made to the store this window keeps itself (tests).
#[cfg(test)]
fn store_id() -> u64 {
    HELD.lock().unwrap().as_ref().map_or(0, |h| h.id)
}

#[cfg(test)]
pub fn as_another_window(change: impl FnOnce(&mut store::Store)) {
    if let Some(Held { keeper: Keeper::Here(host), .. }) = HELD.lock().unwrap().as_mut() {
        change(&mut host.store);
    }
}

/// `state`'s projects and tasks as the store has them; what is the
/// window's of each (its space, folded or not, a task's tabs and whether it
/// was open) as `state` had it. Projects in the window's order, those it
/// did not have after. Another machine's (not in this store) as `state`
/// has them: their tasks come from their machine's store once it is reached.
pub fn overlay(state: &mut State, machine: &Machine) {
    use serde_json::{Value, json};
    let before = std::mem::take(&mut state.projects);
    enum Next<'a> {
        Store(&'a store::Project),
        Kept(state::ProjectState),
    }
    let mut order: Vec<Next> = Vec::new();
    for b in &before {
        match machine.projects.iter().find(|p| p.path == b.path) {
            Some(p) => order.push(Next::Store(p)),
            None if crate::hosts::is_remote(&b.path) && !machine.projects.iter().any(|p| crate::hosts::is_remote(&p.path)) => order.push(Next::Kept(b.clone())),
            None => {}
        }
    }
    order.extend(machine.projects.iter().filter(|p| !before.iter().any(|b| b.path == p.path)).map(Next::Store));
    for next in order {
        let project = match next {
            Next::Kept(kept) => {
                state.projects.push(kept);
                continue;
            }
            Next::Store(project) => project,
        };
        let was = before.iter().find(|b| b.path == project.path);
        let Ok(Value::Object(mut fields)) = serde_json::to_value(project) else { continue };
        fields.remove("tasks");
        if let Some(was) = was {
            fields.insert("space".into(), json!(was.space));
            fields.insert("space_id".into(), json!(was.space_id));
            fields.insert("collapsed".into(), json!(was.collapsed));
        }
        let mut tasks = Vec::new();
        for task in &project.tasks {
            let was = was.and_then(|w| w.tasks.iter().find(|t| t.path == task.path));
            let Ok(Value::Object(mut fields)) = serde_json::to_value(task) else { continue };
            for k in ["id", "pending", "work"] {
                fields.remove(k);
            }
            let mut session = was.map(|w| w.session.clone()).unwrap_or_default();
            session.review = task.work.review.clone();
            session.viewed = task.work.viewed.clone();
            session.bookmarks = task.work.bookmarks.clone();
            fields.insert("session".into(), serde_json::to_value(&session).unwrap_or_default());
            fields.insert("open".into(), json!(was.is_some_and(|w| w.open)));
            match serde_json::from_value::<state::TaskState>(Value::Object(fields)) {
                Ok(task) => tasks.push(task),
                Err(e) => eprintln!("ideae: a task of the store not read: {e}"),
            }
        }
        match serde_json::from_value::<state::ProjectState>(Value::Object(fields)) {
            Ok(mut made) => {
                made.tasks = tasks;
                state.projects.push(made);
            }
            Err(e) => eprintln!("ideae: a project of the store not read: {e}"),
        }
    }
    state.crew = machine.crew.clone();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state through the store and back: what a restart shows is what was.
    #[test]
    fn a_state_comes_back_through_the_store_as_it_was() {
        // A process a test beside this one starts may hold the lock a moment, until it runs its program.
        let open = |state: &mut State| {
            let before = state.clone();
            for _ in 0..40 {
                *state = before.clone();
                if super::open(state) != 0 {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            panic!("the store not opened");
        };
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ideae-machine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: under the lock the tests that set it share.
        unsafe { std::env::set_var("IDEAE_STATE", dir.join("state.json")) };
        let json = serde_json::json!({
            "settings": { "worktrees_dir": "~/wt" },
            "crew": [{ "id": 3, "parent": "/p", "kind": "question" }],
            "projects": [
                { "path": "/q", "space_id": "s2", "tasks": [{ "path": "/q", "branch": "main", "main": true }] },
                { "path": "/p", "space_id": "s1", "collapsed": true, "check": "make", "settings": { "base": "develop" }, "tasks": [
                    { "path": "/p", "branch": "main", "main": true, "open": true },
                    { "path": "/w/a", "branch": "a", "title": "A", "port_slot": 1, "starred": true, "open": true, "note": "n",
                      "questions": [{ "id": 1, "title": "why" }], "asked": 1,
                      "session": { "tabs": [{ "kind": "agent" }], "review": [{ "text": "fix", "line": 3 }], "bookmarks": { "b.rs": [1] } } }
                ]}
            ]
        });
        let mut first: State = serde_json::from_value(json).unwrap();
        std::fs::write(dir.join("state.json"), serde_json::to_vec(&first).unwrap()).unwrap();
        let read = first.clone();
        open(&mut first);
        assert!(dir.join("machine.json").exists() && dir.join("state.json.before-machine").exists());
        assert!(first.projects == read.projects, "the first start shows what state.json had");
        // A change, saved; then a start again from what was written.
        first.projects[1].tasks[1].title = "A2".into();
        first.projects[1].tasks[1].session.review.clear();
        sync(&mut first, taken_in(store_id()), &[]);
        assert!(first.machine_rev > 0);
        let written = first.clone();
        drop(HELD.lock().unwrap().take());
        let mut again = written.clone();
        open(&mut again);
        assert!(again.projects == written.projects, "a restart shows what was saved");
        assert_eq!(again.crew, written.crew);
        let id_of = |path: &str| {
            let held = HELD.lock().unwrap();
            match &held.as_ref()?.keeper {
                Keeper::Here(host) => host.store.task_at(std::path::Path::new(path)).map(|t| t.id),
                #[cfg(unix)]
                Keeper::Daemon(_) => None,
            }
        };
        let id_a = id_of("/w/a").unwrap();
        // An older build wrote state.json since (no rev): it is what is, the store made of it again.
        drop(HELD.lock().unwrap().take());
        let mut older = written.clone();
        older.machine_rev = 0;
        older.projects[1].tasks[1].title = "By an older build".into();
        open(&mut older);
        assert_eq!(older.projects[1].tasks[1].title, "By an older build");
        assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("machine.json.before-reimport")));
        assert_eq!(id_of("/w/a"), Some(id_a), "its id kept: never one given twice");
        // A task the store refuses (its path another project's task's): the store is what is, not
        // state.json made the store again at the next start (which would drop other windows' work).
        let mut odd = older.clone();
        let mut twin = odd.projects[1].tasks[1].clone();
        twin.title = "Twin".into();
        odd.projects[0].tasks.push(twin);
        sync(&mut odd, taken_in(store_id()), &[]);
        assert!(odd.machine_rev > 0, "state.json still the store's");
        drop(HELD.lock().unwrap().take());
        let mut back = odd.clone();
        open(&mut back);
        assert!(!back.projects[0].tasks.iter().any(|t| t.title == "Twin"), "the store's");
        assert!(back.projects[1].tasks.iter().any(|t| t.path == std::path::Path::new("/w/a")), "and the task whose path it was");
        drop(HELD.lock().unwrap().take());
        // What it costs a save, on a state of a real size (IDEAE_MIGRATE_CHECK, copied by hand).
        if let Some(real) = std::env::var_os("IDEAE_MIGRATE_CHECK") {
            let mut real: State = serde_json::from_slice(&std::fs::read(real).unwrap()).unwrap();
            let _ = std::fs::remove_file(dir.join("machine.json"));
            open(&mut real);
            let started = std::time::Instant::now();
            for n in 0..100 {
                if let Some(t) = real.projects.iter_mut().flat_map(|p| &mut p.tasks).next() {
                    t.last_active = n;
                }
                sync(&mut real, taken_in(store_id()), &[]);
            }
            eprintln!("a save's store sync: {:?}", started.elapsed() / 100);
            drop(HELD.lock().unwrap().take());
        }
        // SAFETY: as above.
        unsafe { std::env::remove_var("IDEAE_STATE") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
