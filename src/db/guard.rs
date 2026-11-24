//! Production connections: a safety catch. Nothing runs on one until
//! production is unlocked, which asks first; unlocked, all of them run for
//! a while and lock again by themselves. It keeps a statement from going to
//! production by mistake; it is no secret, and keeps nothing from a program
//! that has the connection's login.

use std::{
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

use super::config::{self, Connection};

/// What a build that sealed production passwords left in the keyring.
const SEALED: &str = "ideae-sealed:v1:";
pub const LOCKED: &str = "Production is locked: unlock it to run this";
pub const MINUTES: [u32; 4] = [5, 10, 15, 30];

struct Open {
    until: Instant,
    /// The same moment on the clock, to show.
    clock: SystemTime,
}

static OPEN: Mutex<Option<Open>> = Mutex::new(None);

/// Locked again once its time is up. Both clocks are asked: `Instant`
/// stands still while the machine sleeps, the wall clock does not.
pub fn is_open() -> bool {
    let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    if open.as_ref().is_some_and(|o| Instant::now() >= o.until || SystemTime::now() >= o.clock) {
        *open = None;
    }
    open.is_some()
}

/// Until when it is unlocked, on the clock.
pub fn open_until() -> Option<SystemTime> {
    is_open().then(|| OPEN.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|o| o.clock)).flatten()
}

/// How long until it locks again.
pub fn left() -> Option<Duration> {
    is_open()
        .then(|| {
            OPEN.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|o| {
                let by_clock = o.clock.duration_since(SystemTime::now()).unwrap_or_default();
                o.until.saturating_duration_since(Instant::now()).min(by_clock)
            })
        })
        .flatten()
}

pub(super) fn unlock_for(lasts: Duration) {
    *OPEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Open { until: Instant::now() + lasts, clock: SystemTime::now() + lasts });
}

pub fn unlock() {
    unlock_for(Duration::from_secs(u64::from(config::production_minutes()) * 60));
}

pub fn lock() {
    *OPEN.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Whether statements may go to this connection now. Its profile as it is
/// now, not as it was when a session took its copy.
pub fn may_run(connection: &Connection) -> Result<(), String> {
    let production = config::find(&connection.id).map_or(connection.production, |now| now.production);
    if production && !is_open() { Err(LOCKED.into()) } else { Ok(()) }
}

/// A password as the keyring has it; one sealed by an earlier build cannot be opened any more.
pub fn kept(stored: String) -> Result<String, String> {
    if stored.starts_with(SEALED) { Err("This password was sealed by an earlier build: type it again in the connection's form".into()) } else { Ok(stored) }
}

/// Tests that lock or unlock hold this: the state is the process's.
#[cfg(test)]
pub static TESTS: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_runs_only_while_unlocked_and_locks_again_on_time() {
        let _one = TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let connection = |production| Connection {
            id: "c".into(),
            name: "c".into(),
            driver: Default::default(),
            host: "h".into(),
            port: 1,
            user: String::new(),
            database: None,
            bastion: None,
            ssh: None,
            tls: Default::default(),
            color: None,
            production,
        };
        lock();
        assert_eq!(may_run(&connection(true)), Err(LOCKED.into()));
        assert_eq!(may_run(&connection(false)), Ok(()));
        unlock_for(Duration::from_secs(60));
        assert!(may_run(&connection(true)).is_ok() && open_until().is_some() && left().is_some());
        unlock_for(Duration::ZERO);
        assert_eq!(may_run(&connection(true)), Err(LOCKED.into()));
        assert!(kept("plain".into()).is_ok() && kept(format!("{SEALED}abc")).is_err());
    }
}
