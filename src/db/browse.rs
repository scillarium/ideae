//! What the Database panel shows of a connection: its databases and their
//! tables, each read on a connection of its own that is closed after.

use super::{
    backend::{self, Backend, DbResult},
    config::Connection,
    schema::TableMeta,
    schema_cache, tunnel,
};

fn with<T>(connection: &Connection, password: Option<String>, database: Option<String>, read: impl FnOnce(&mut dyn Backend) -> DbResult<T>) -> Result<T, String> {
    super::guard::may_run(connection)?;
    let tunnel = connection.ssh.as_ref().map(|hop| tunnel::open(hop, &connection.host, connection.port)).transpose().map_err(|e| format!("{e:#}"))?;
    let mut db = backend::connect(connection, tunnel.as_deref(), password, database).map_err(|e| format!("{e:#}"))?;
    read(db.as_mut()).map_err(|e| e.to_string())
}

pub fn databases(connection: &Connection, password: Option<String>) -> Result<Vec<String>, String> {
    let mut names = with(connection, password, None, |db| db.databases())?;
    // Redis numbers its databases: 2 before 10.
    names.sort_by_key(|n| (n.parse::<u64>().unwrap_or(u64::MAX), n.to_lowercase()));
    Ok(names)
}

/// The tables a file's completions read, when they were kept.
pub fn kept_tables(connection: &str, database: &str) -> Option<Vec<TableMeta>> {
    schema_cache::read(connection, database).map(|(_, schema)| sorted(schema.tables))
}

/// The tables, from the server; they are kept for the files' completions.
pub fn tables(connection: &Connection, password: Option<String>, database: &str, fresh: bool) -> Result<Vec<TableMeta>, String> {
    if !fresh && let Some(tables) = kept_tables(&connection.id, database) {
        return Ok(tables);
    }
    let schema = with(connection, password, Some(database.to_string()), |db| {
        let stamp = db.schema_stamp(database)?;
        let schema = db.schema(database)?;
        if let Some(stamp) = stamp {
            schema_cache::write(&connection.id, database, &stamp, &schema);
        }
        Ok(schema)
    })?;
    Ok(sorted(schema.tables))
}

/// A database's schema for a file not connected yet: as kept, else read
/// from the server and kept.
pub fn schema(connection: &Connection, password: Option<String>, database: &str) -> Result<super::schema::Schema, String> {
    if let Some((_, schema)) = schema_cache::read(&connection.id, database) {
        return Ok(schema);
    }
    with(connection, password, Some(database.to_string()), |db| {
        let stamp = db.schema_stamp(database)?;
        let schema = db.schema(database)?;
        if let Some(stamp) = stamp {
            schema_cache::write(&connection.id, database, &stamp, &schema);
        }
        Ok(schema)
    })
}

fn sorted(mut tables: Vec<TableMeta>) -> Vec<TableMeta> {
    tables.sort_by(|a, b| (a.view, a.name.to_lowercase()).cmp(&(b.view, b.name.to_lowercase())));
    tables
}
