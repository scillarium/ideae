//! Connections read from DataGrip (and JetBrains IDEs with its plugin): its
//! data sources and SSH configurations. Never its passwords, which are in the
//! system's keyring under its own names.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use super::config::{Connection, Driver, SshHop};

/// A connection found, and whether it is there already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub connection: Connection,
    pub known: bool,
}

/// Host, port and database of a JDBC URL this app has a driver for.
pub fn parse_jdbc(url: &str) -> Option<(Driver, String, u16, Option<String>)> {
    let rest = url.trim().strip_prefix("jdbc:")?;
    let (scheme, rest) = rest.split_once("://")?;
    let driver = match scheme {
        "mysql" | "mariadb" => Driver::MySql,
        "postgresql" => Driver::Postgres,
        "redis" => Driver::Redis,
        _ => return None,
    };
    let rest = rest.split(['?', ';']).next()?;
    let (authority, database) = rest.split_once('/').unwrap_or((rest, ""));
    // Several hosts are a cluster's failover: not one address to connect to.
    if authority.contains(',') {
        return None;
    }
    let authority = authority.rsplit('@').next()?;
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, after) = bracketed.split_once(']')?;
        (host.to_string(), after.strip_prefix(':').and_then(|p| p.parse().ok()))
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host.to_string(), port.parse().ok()),
            None => (authority.to_string(), None),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some((driver, host, port.unwrap_or(driver.default_port()), (!database.is_empty()).then(|| database.to_string())))
}

fn home_in(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("$USER_HOME$") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

/// `sshConfigs.xml`: its configurations by id.
fn ssh_configs(xml: &str, home: &Path) -> HashMap<String, SshHop> {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return HashMap::new() };
    doc.descendants()
        .filter(|n| n.has_tag_name("sshConfig"))
        .filter_map(|n| {
            let id = n.attribute("id")?;
            let host = n.attribute("host")?;
            let hop = SshHop {
                host: host.to_string(),
                port: n.attribute("port").and_then(|p| p.parse().ok()).unwrap_or(22),
                user: n.attribute("username").unwrap_or_default().to_string(),
                key: n.attribute("keyPath").filter(|k| !k.is_empty()).map(|k| home_in(k, home)),
            };
            Some((id.to_string(), hop))
        })
        .collect()
}

#[derive(Default)]
struct Source {
    name: Option<String>,
    url: Option<String>,
    user: Option<String>,
    ssh: Option<String>,
}

/// Data sources in a file, into what is known by their uuid: one file has
/// the URL, another the user and the tunnel.
fn read_sources(xml: &str, into: &mut HashMap<String, Source>) {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return };
    for node in doc.descendants().filter(|n| n.has_tag_name("data-source")) {
        let Some(uuid) = node.attribute("uuid") else { continue };
        let source = into.entry(uuid.to_string()).or_default();
        if let Some(name) = node.attribute("name") {
            source.name.get_or_insert_with(|| name.to_string());
        }
        let text = |tag: &str| node.children().find(|c| c.has_tag_name(tag)).and_then(|c| c.text()).map(str::trim).filter(|t| !t.is_empty()).map(String::from);
        if let Some(url) = text("jdbc-url") {
            source.url.get_or_insert(url);
        }
        if let Some(user) = text("user-name") {
            source.user.get_or_insert(user);
        }
        if let Some(ssh) = node.children().find(|c| c.has_tag_name("ssh-properties")) {
            let field = |tag: &str| ssh.children().find(|c| c.has_tag_name(tag)).and_then(|c| c.text()).map(str::trim);
            if field("enabled") == Some("true")
                && let Some(id) = field("ssh-config-id")
            {
                source.ssh.get_or_insert_with(|| id.to_string());
            }
        }
    }
}

fn connections(sources: HashMap<String, Source>, hops: &HashMap<String, SshHop>) -> Vec<Connection> {
    let mut out: Vec<Connection> = sources
        .into_iter()
        .filter_map(|(uuid, source)| {
            let (driver, host, port, database) = parse_jdbc(source.url.as_deref()?)?;
            Some(Connection {
                id: format!("datagrip-{uuid}"),
                name: source.name.unwrap_or_else(|| host.clone()),
                driver,
                host,
                port,
                user: source.user.unwrap_or_default(),
                database,
                ssh: source.ssh.and_then(|id| hops.get(&id).cloned()),
                bastion: None,
                tls: Default::default(),
                color: None,
                production: false,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn files_under(dir: &Path, wanted: &dyn Fn(&str) -> bool, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() && depth > 0 {
            files_under(&path, wanted, depth - 1, out);
        } else if kind.is_file() && path.file_name().and_then(|n| n.to_str()).is_some_and(wanted) {
            out.push(path);
        }
    }
}

/// Where JetBrains keeps its settings on this system.
fn config_root(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/JetBrains")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map_or_else(|| home.join("AppData/Roaming"), PathBuf::from).join("JetBrains")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from).join("JetBrains")
    }
}

/// The projects a JetBrains IDE opened lately, from its `recentProjects.xml`.
fn recent_projects(xml: &str, home: &Path) -> Vec<PathBuf> {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return Vec::new() };
    doc.descendants()
        .filter(|n| n.has_tag_name("option") && n.attribute("name") == Some("additionalInfo"))
        .flat_map(|option| option.descendants().filter(|n| n.has_tag_name("entry")).filter_map(|n| n.attribute("key")))
        .map(|key| home_in(key, home))
        .collect()
}

/// The folders of the projects ideae has, read from its state as it is on disk.
fn ideae_projects() -> Vec<PathBuf> {
    let Ok(bytes) = std::fs::read(crate::state::state_path()) else { return Vec::new() };
    let Ok(state) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Vec::new() };
    state["projects"].as_array().into_iter().flatten().filter_map(|p| p["path"].as_str()).map(PathBuf::from).collect()
}

/// A project's own data sources: in its `.idea`, where an IDE keeps those added to the project.
fn project_sources(projects: impl IntoIterator<Item = PathBuf>, out: &mut Vec<PathBuf>) {
    let mut seen = std::collections::HashSet::new();
    for project in projects {
        if !seen.insert(project.clone()) {
            continue;
        }
        for name in ["dataSources.xml", "dataSources.local.xml"] {
            let file = project.join(".idea").join(name);
            if file.is_file() {
                out.push(file);
            }
        }
    }
}

/// What DataGrip knows, and the JetBrains IDEs' projects. Reads files: not for the UI thread.
pub fn find(existing: &[Connection]) -> Vec<Found> {
    let Some(home) = home() else { return Vec::new() };
    let root = config_root(&home);
    let is_sources = |name: &str| matches!(name, "dataSources.xml" | "dataSources.local.xml" | "data_sources_history.xml");
    let mut files = Vec::new();
    files_under(&home.join("DataGripProjects"), &is_sources, 4, &mut files);
    let mut recent = Vec::new();
    files_under(&root, &|name| name == "recentProjects.xml", 2, &mut recent);
    let recent = recent.into_iter().filter_map(|file| std::fs::read_to_string(file).ok()).flat_map(|xml| recent_projects(&xml, &home));
    project_sources(ideae_projects().into_iter().chain(recent), &mut files);
    // The projects' own files come first: what they say is newer than a history's.
    files_under(&root, &is_sources, 3, &mut files);
    found_in(files, &home, existing)
}

/// The data sources of the projects in `folder` and the folders under it,
/// for those no IDE opened lately. Reads files: not for the UI thread.
/// How many folders a look goes through at most: a home folder or `/`
/// chosen ends there, not after minutes.
const FOLDERS_LOOKED: usize = 20_000;

/// The data sources of the projects in `folder` and under it; whether the
/// look stopped at its limit before seeing all of them.
pub fn find_in(folder: &Path, existing: &[Connection]) -> (Vec<Found>, bool) {
    let Some(home) = home() else { return (Vec::new(), false) };
    let mut projects = Vec::new();
    let mut left = FOLDERS_LOOKED;
    projects_under(folder, 4, &mut left, &mut projects);
    let mut files = Vec::new();
    project_sources(projects, &mut files);
    (found_in(files, &home, existing), left == 0)
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Folders that have a `.idea`, down to `depth`; not into hidden folders or
/// what builds and package managers fill.
fn projects_under(dir: &Path, depth: usize, left: &mut usize, out: &mut Vec<PathBuf>) {
    if *left == 0 {
        return;
    }
    *left -= 1;
    if dir.join(".idea").is_dir() {
        out.push(dir.to_path_buf());
    }
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || matches!(name.as_ref(), "node_modules" | "vendor" | "target" | "build" | "dist" | "var") {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            projects_under(&entry.path(), depth - 1, left, out);
        }
    }
}

fn found_in(files: Vec<PathBuf>, home: &Path, existing: &[Connection]) -> Vec<Found> {
    let mut ssh_files = Vec::new();
    files_under(&config_root(home), &|name| name == "sshConfigs.xml", 2, &mut ssh_files);
    let mut hops = HashMap::new();
    for file in ssh_files {
        if let Ok(xml) = std::fs::read_to_string(file) {
            hops.extend(ssh_configs(&xml, home));
        }
    }
    let mut sources = HashMap::new();
    for file in files {
        if let Ok(xml) = std::fs::read_to_string(file) {
            read_sources(&xml, &mut sources);
        }
    }
    connections(sources, &hops)
        .into_iter()
        .map(|connection| {
            let known = existing.iter().any(|c| c.id == connection.id);
            Found { connection, known }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jdbc_urls_give_their_server() {
        assert_eq!(parse_jdbc("jdbc:mysql://db-internal:3307/app?useSSL=false"), Some((Driver::MySql, "db-internal".into(), 3307, Some("app".into()))));
        assert_eq!(parse_jdbc("jdbc:postgresql://pg/shop"), Some((Driver::Postgres, "pg".into(), 5432, Some("shop".into()))));
        assert_eq!(parse_jdbc("jdbc:mariadb://[fd00::1]:3306"), Some((Driver::MySql, "fd00::1".into(), 3306, None)));
        assert_eq!(parse_jdbc("jdbc:oracle:thin:@x:1521:y"), None);
        assert_eq!(parse_jdbc("jdbc:mysql://a:1,b:2/app"), None);
    }

    #[test]
    fn a_source_comes_together_from_its_files_with_its_tunnel() {
        let hops = ssh_configs(
            r#"<application><component name="SshConfigs"><configs>
                <sshConfig host="bastion.example" id="s1" keyPath="$USER_HOME$/.ssh/id_ed25519" port="2222" username="me"/>
            </configs></component></application>"#,
            Path::new("/home/someone"),
        );
        let mut sources = HashMap::new();
        read_sources(r#"<project><component><data-source name="Stage" uuid="u1"><jdbc-url>jdbc:mysql://db-internal:3306/app</jdbc-url></data-source></component></project>"#, &mut sources);
        read_sources(r#"<project><component><data-source name="Stage" uuid="u1"><user-name>reader</user-name><ssh-properties><enabled>true</enabled><ssh-config-id>s1</ssh-config-id></ssh-properties></data-source></component></project>"#, &mut sources);
        let found = connections(sources, &hops);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].name.as_str(), found[0].user.as_str(), found[0].id.as_str()), ("Stage", "reader", "datagrip-u1"));
        let hop = found[0].ssh.as_ref().unwrap();
        assert_eq!((hop.host.as_str(), hop.port), ("bastion.example", 2222));
        assert_eq!(hop.key.as_deref(), Some(Path::new("/home/someone/.ssh/id_ed25519")));
    }

    /// Data sources added to a project live in its `.idea`: the projects an
    /// IDE opened lately are where to look for them.
    #[test]
    fn a_recent_project_s_data_sources_are_found() {
        let dir = std::env::temp_dir().join(format!("ideae-recent-{}", std::process::id()));
        let project = dir.join("work/shop");
        std::fs::create_dir_all(project.join(".idea")).unwrap();
        std::fs::write(project.join(".idea/dataSources.xml"), r#"<project><component><data-source name="Shop" uuid="p1"><jdbc-url>jdbc:postgresql://pg:5433/shop</jdbc-url></data-source></component></project>"#).unwrap();
        std::fs::write(project.join(".idea/dataSources.local.xml"), r#"<project><component><data-source name="Shop" uuid="p1"><user-name>app</user-name></data-source></component></project>"#).unwrap();
        let recent = recent_projects(
            r#"<application><component name="RecentProjectsManager"><option name="additionalInfo"><map>
                <entry key="$USER_HOME$/work/shop"><value><RecentProjectMetaInfo/></value></entry>
                <entry key="$USER_HOME$/work/gone"><value><RecentProjectMetaInfo/></value></entry>
            </map></option></component></application>"#,
            &dir,
        );
        assert_eq!(recent, [dir.join("work/shop"), dir.join("work/gone")]);
        let mut files = Vec::new();
        project_sources(recent.into_iter().chain([project.clone()]), &mut files);
        assert_eq!(files.len(), 2, "each file once, none of a project that is gone: {files:?}");
        let mut sources = HashMap::new();
        for file in files {
            read_sources(&std::fs::read_to_string(file).unwrap(), &mut sources);
        }
        let found = connections(sources, &HashMap::new());
        assert_eq!((found[0].name.as_str(), found[0].host.as_str(), found[0].port, found[0].user.as_str()), ("Shop", "pg", 5433, "app"));
        // A folder chosen by hand: its projects, not what is inside their dependencies.
        std::fs::create_dir_all(dir.join("work/shop/node_modules/lib/.idea")).unwrap();
        std::fs::create_dir_all(dir.join("work/old/api/.idea")).unwrap();
        let (mut projects, mut left) = (Vec::new(), 100);
        projects_under(&dir.join("work"), 4, &mut left, &mut projects);
        projects.sort();
        assert_eq!(projects, [dir.join("work/old/api"), dir.join("work/shop")]);
        // Its limit reached, a look stops where it is.
        let (mut projects, mut left) = (Vec::new(), 1);
        projects_under(&dir.join("work"), 4, &mut left, &mut projects);
        assert!(projects.is_empty() && left == 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
