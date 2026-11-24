//! Databases for `.sql` files: connections kept apart from the state, a
//! tunnel through a bastion, a session on a thread of its own for each file.
//! Apart from the keyring's calls, nothing here knows the UI.

pub mod backend;
pub mod browse;
pub mod config;
pub mod csv;
pub mod export;
#[cfg(all(test, unix))]
mod fake;
pub mod format;
pub mod guard;
pub mod history;
pub mod import;
#[cfg(all(test, unix))]
mod live;
pub mod mysql_backend;
pub mod password;
pub mod postgres_backend;
pub mod redis_backend;
pub mod schema;
pub mod schema_cache;
pub mod scratch;
pub mod session;
pub mod statement;
pub mod tls;
pub mod tunnel;
pub mod value;

/// Ends the tunnels still open and writes what changed: the app exits next.
pub fn quit() {
    tunnel::quit();
    config::flush();
    history::flush();
}
