//! `.http` files, as JetBrains' HTTP Client writes them (its format is open:
//! github.com/JetBrains/http-request-in-editor-spec, CC BY 4.0): requests
//! parted by `###`, `{{variables}}` from `http-client.env.json` and its
//! private twin, their scripts (http_script.rs), sent off the UI thread.
//! No gpui here.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

pub use crate::http_script::Test;

/// The most of an answer read.
const MOST: u64 = 32 << 20;
/// How long a request may take unless it says (`# @timeout 30`).
const TIMEOUT: Duration = Duration::from_secs(60);
/// The longest a request's own timeout may be.
const LONGEST: Duration = Duration::from_secs(3600);

pub const ENV_FILE: &str = "http-client.env.json";
pub const PRIVATE_ENV_FILE: &str = "http-client.private.env.json";

pub fn is_http(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("http") || e.eq_ignore_ascii_case("rest"))
}

/// One request of a file, as written: variables not yet put in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    /// From `###` to the next: where the cursor picks it.
    pub range: Range<usize>,
    /// Where its request line is: a run mark goes there.
    pub line: Range<usize>,
    /// `# @name`, else the words after `###`.
    pub name: Option<String>,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<BodyPart>,
    pub no_redirect: bool,
    /// `# @no-cookie-jar`: neither sends the cookies kept nor keeps its answer's.
    pub no_cookies: bool,
    pub timeout: Option<Duration>,
    pub connect_timeout: Option<Duration>,
    /// `< {% … %}`: JavaScript run before it goes; `< file.js` likewise.
    pub pre_script: String,
    pub pre_files: Vec<String>,
    /// `> {% … %}`: JavaScript run on its answer; `> file.js` likewise.
    pub post_script: String,
    pub post_files: Vec<String>,
    /// What is in it that does not run here: said with its answer.
    pub skipped: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BodyPart {
    Text(String),
    /// `< ./file`: a file's content, relative to the `.http` file.
    File(String),
}

impl Request {
    /// How a list or a tab names it.
    pub fn title(&self) -> String {
        match &self.name {
            Some(name) => name.clone(),
            None => format!("{} {}", self.method, self.url.split_whitespace().collect::<String>()),
        }
    }
}

const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "DELETE", "CONNECT", "PATCH", "OPTIONS", "TRACE"];

/// The lines of `text` with where each begins.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    text.split_inclusive('\n').map(move |line| {
        let start = at;
        at += line.len();
        (start, line.trim_end_matches(['\n', '\r']))
    })
}

fn is_comment(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with('#') || line.starts_with("//")
}

/// `# @name value`, `// @name value` or `# @name=value`.
fn directive(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    let rest = line.strip_prefix('#').or_else(|| line.strip_prefix("//"))?.trim_start().strip_prefix('@')?;
    let (key, value) = rest.split_once(|c: char| c.is_whitespace() || c == '=').unwrap_or((rest, ""));
    Some((key, value.trim().trim_start_matches('=').trim()))
}

/// `@host = example.com`: a variable of the file.
fn file_variable(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix('@')?;
    let (name, value) = rest.split_once('=')?;
    let name = name.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')).then(|| (name.to_string(), value.trim().to_string()))
}

/// `30`, `30 s`, `600 ms`, `2 m`: seconds unless it says.
fn duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    let digits = value.find(|c: char| !c.is_ascii_digit()).unwrap_or(value.len());
    let n: u64 = value.get(..digits)?.parse().ok()?;
    let made = match value.get(digits..)?.trim() {
        "" | "s" => Duration::from_secs(n),
        "ms" => Duration::from_millis(n),
        "m" => Duration::from_secs(n.saturating_mul(60)),
        _ => return None,
    };
    Some(made.min(LONGEST))
}

/// `GET url`, `get url`: a line that begins a request by naming its method.
fn method_line(line: &str) -> Option<(String, &str)> {
    let (word, rest) = line.trim_start().split_once(char::is_whitespace)?;
    let upper = word.to_ascii_uppercase();
    (METHODS.contains(&upper.as_str()) && !rest.trim().is_empty()).then(|| (upper, rest.trim()))
}

/// The requests of a file, and the variables it sets itself.
pub fn parse(text: &str) -> (Vec<Request>, BTreeMap<String, String>) {
    let mut requests = Vec::new();
    let mut variables = BTreeMap::new();
    // Each part from a `###` line, or the beginning, to the next.
    let mut parts: Vec<(usize, Option<String>)> = vec![(0, None)];
    for (at, line) in lines(text) {
        if let Some(rest) = line.strip_prefix("###") {
            let name = rest.trim();
            parts.push((at, (!name.is_empty()).then(|| name.to_string())));
        }
    }
    for (ix, (start, title)) in parts.iter().enumerate() {
        let end = parts.get(ix + 1).map_or(text.len(), |(next, _)| *next);
        let (mut from, mut title) = (*start, title.clone());
        // A part may hold more than one: a request written after another's
        // handler, with no `###` between.
        loop {
            let (request, next) = parse_part(text, from..end, title.take(), &mut variables);
            requests.extend(request);
            match next {
                Some(next) if next > from => from = next,
                _ => break,
            }
        }
    }
    (requests, variables)
}

#[derive(PartialEq)]
enum Stage {
    Before,
    Headers,
    Body,
    /// Its handler was read: nothing more is of its body.
    After,
}

/// A script being read: the one before the request, or its answer's.
#[derive(Clone, Copy, PartialEq)]
enum Script {
    Before,
    After,
}

/// `< {%` or `> {%` at a line's start, however spaced: which, and the rest.
fn script_opens(trimmed: &str) -> Option<(Script, &str)> {
    let (which, rest) = match trimmed.as_bytes().first()? {
        b'<' => (Script::Before, trimmed.get(1..)?),
        b'>' => (Script::After, trimmed.get(1..)?),
        _ => return None,
    };
    rest.trim_start().strip_prefix("{%").map(|rest| (which, rest))
}

/// The request of `range`, and where the next one begins when the part has another.
fn parse_part(text: &str, range: Range<usize>, title: Option<String>, variables: &mut BTreeMap<String, String>) -> (Option<Request>, Option<usize>) {
    let mut request = Request { range: range.clone(), name: title, ..Default::default() };
    let mut stage = Stage::Before;
    let mut body = String::new();
    let mut script: Option<Script> = None;
    let mut next = None;
    let Some(part) = text.get(range.clone()) else { return (None, None) };
    for (offset, line) in lines(part) {
        let at = range.start + offset;
        if offset == 0 && line.starts_with("###") {
            continue;
        }
        let trimmed = line.trim_start();
        // Inside `{% … %}`: the script's own lines, whatever they look like.
        // It ends where a line ends with `%}`: one inside a string does not end it.
        if let Some(which) = script {
            let (code, ends) = match line.trim_end().strip_suffix("%}") {
                Some(code) => (code, true),
                None => (line, false),
            };
            let into = if which == Script::Before { &mut request.pre_script } else { &mut request.post_script };
            into.push_str(code);
            into.push('\n');
            if ends {
                script = None;
            }
            continue;
        }
        if let Some((which, rest)) = script_opens(trimmed) {
            let (code, ends) = match rest.trim_end().strip_suffix("%}") {
                Some(code) => (code, true),
                None => (rest, false),
            };
            let into = if which == Script::Before { &mut request.pre_script } else { &mut request.post_script };
            into.push_str(code);
            into.push('\n');
            if !ends {
                script = Some(which);
            }
            if which == Script::After {
                stage = Stage::After;
            }
            continue;
        }
        match stage {
            Stage::Before => {
                if line.trim().is_empty() {
                    continue;
                }
                if let Some((key, value)) = directive(line) {
                    match key {
                        "name" if !value.is_empty() => request.name = Some(value.to_string()),
                        "no-redirect" => request.no_redirect = true,
                        "no-cookie-jar" => request.no_cookies = true,
                        "timeout" | "readTimeout" | "read-timeout" => request.timeout = duration(value),
                        "connectTimeout" | "connection-timeout" | "connect-timeout" => request.connect_timeout = duration(value),
                        _ => {}
                    }
                    continue;
                }
                if is_comment(line) {
                    continue;
                }
                if let Some((name, value)) = file_variable(line) {
                    variables.insert(name, value);
                    continue;
                }
                if let Some(file) = line.strip_prefix("< ").map(str::trim).filter(|file| file.ends_with(".js")) {
                    request.pre_files.push(file.to_string());
                    continue;
                }
                if let Some(word) = trimmed.split_whitespace().next().filter(|w| matches!(*w, "import" | "run" | "GRPC" | "WEBSOCKET" | "GRAPHQL")) {
                    request.skipped.push(format!("a line that begins with {word}"));
                    continue;
                }
                let (method, rest) = match method_line(line) {
                    Some(found) => found,
                    None => match trimmed.split_once(char::is_whitespace) {
                        // A method of the server's own: all capitals before the URL.
                        Some((word, rest)) if word.len() > 1 && word.chars().all(|c| c.is_ascii_uppercase()) && !rest.trim().is_empty() => (word.to_string(), rest.trim()),
                        _ => ("GET".to_string(), line.trim()),
                    },
                };
                let url = match rest.rsplit_once(char::is_whitespace) {
                    Some((url, version)) if version.starts_with("HTTP/") => url,
                    _ => rest,
                };
                request.method = method;
                request.url = url.trim().to_string();
                request.line = at..at + line.len();
                stage = Stage::Headers;
            }
            Stage::Headers => {
                if line.trim().is_empty() {
                    stage = Stage::Body;
                    continue;
                }
                // The URL goes on, indented: `  ?page=2` or `  &size=10`.
                if request.headers.is_empty() && line.starts_with(char::is_whitespace) {
                    request.url.push_str(line.trim());
                    continue;
                }
                if is_comment(line) {
                    continue;
                }
                if let Some((name, value)) = line.split_once(':') {
                    request.headers.push((name.trim().to_string(), value.trim().to_string()));
                }
            }
            Stage::Body | Stage::After => {
                if trimmed.starts_with(">> ") || trimmed.starts_with(">>! ") {
                    request.skipped.push("saving the answer to a file".into());
                    continue;
                }
                // What an IDE noted of an answer it once got.
                if trimmed.starts_with("<> ") {
                    continue;
                }
                if let Some(file) = trimmed.strip_prefix("> ").map(str::trim).filter(|file| file.ends_with(".js")) {
                    request.post_files.push(file.to_string());
                    stage = Stage::After;
                    continue;
                }
                if stage == Stage::After {
                    if method_line(line).is_some() && !line.starts_with(char::is_whitespace) {
                        next = Some(at);
                        request.range.end = at;
                        break;
                    }
                    continue;
                }
                // Only at the line's start: `  < 5` in a body is the body's.
                if let Some(file) = line.strip_prefix("< ") {
                    if !body.is_empty() {
                        request.body.push(BodyPart::Text(std::mem::take(&mut body)));
                    }
                    request.body.push(BodyPart::File(file.trim().to_string()));
                    continue;
                }
                body.push_str(line);
                body.push('\n');
            }
        }
    }
    if request.method.is_empty() {
        return (None, next);
    }
    let body = body.trim_end().to_string();
    if !body.is_empty() {
        request.body.push(BodyPart::Text(body));
    }
    if let Some(BodyPart::Text(first)) = request.body.first_mut() {
        *first = first.trim_start_matches(['\n', '\r']).to_string();
    }
    (Some(request), next)
}

/// The request whose part holds `cursor`.
pub fn at(requests: &[Request], cursor: usize) -> Option<&Request> {
    requests.iter().find(|r| r.range.contains(&cursor)).or_else(|| requests.last().filter(|r| cursor >= r.range.end))
}

// ---------------------------------------------------------------- environments

/// Environments by name, each a set of variables; `$shared` goes into all.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Environments {
    pub names: Vec<String>,
    values: BTreeMap<String, BTreeMap<String, String>>,
    /// Those whose private file says not to check the server's certificate
    /// (`"SSLConfiguration": {"verifyHostCertificate": false}`).
    unchecked: BTreeSet<String>,
    /// The files read, for the bar to say where they are.
    pub files: Vec<PathBuf>,
}

impl Environments {
    pub fn variables(&self, env: Option<&str>) -> BTreeMap<String, String> {
        let mut all = self.values.get("$shared").cloned().unwrap_or_default();
        if let Some(own) = env.and_then(|e| self.values.get(e)) {
            all.extend(own.clone());
        }
        all
    }

    pub fn unchecked(&self, env: Option<&str>) -> bool {
        env.is_some_and(|env| self.unchecked.contains(env))
    }
}

/// An environment that is the real thing by its name: asked before a
/// request goes there, and never chosen by itself.
pub fn is_production(env: &str) -> bool {
    let name = env.to_ascii_lowercase();
    name.starts_with("prod") || name.starts_with("live") || name.ends_with("prod") || name.ends_with("production") || name.ends_with("live")
}

/// A value under its name; an object's fields under `name.field`, a list's
/// items under `name.0` and `name[0]`, and the whole as JSON under `name`.
fn put_value(into: &mut BTreeMap<String, String>, key: String, value: &Value) {
    match value {
        Value::String(s) => drop(into.insert(key, s.clone())),
        Value::Number(n) => drop(into.insert(key, n.to_string())),
        Value::Bool(b) => drop(into.insert(key, b.to_string())),
        Value::Null => {}
        Value::Object(fields) => {
            for (name, field) in fields {
                put_value(into, format!("{key}.{name}"), field);
            }
            into.insert(key, value.to_string());
        }
        Value::Array(items) => {
            for (ix, item) in items.iter().enumerate() {
                put_value(into, format!("{key}.{ix}"), item);
                put_value(into, format!("{key}[{ix}]"), item);
            }
            into.insert(key, value.to_string());
        }
    }
}

fn read_env(text: &str, private: bool, into: &mut BTreeMap<String, BTreeMap<String, String>>, unchecked: &mut BTreeSet<String>) -> Result<(), String> {
    let json: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Some(envs) = json.as_object() else { return Err("not an object of environments".into()) };
    for (name, vars) in envs {
        let Some(vars) = vars.as_object() else { continue };
        let env = into.entry(name.clone()).or_default();
        for (key, value) in vars {
            // How to connect, not variables. Only a file kept out of git may
            // say a certificate goes unchecked, as in JetBrains' client.
            if key == "SSLConfiguration" || key == "Security" {
                if private && key == "SSLConfiguration" && value.get("verifyHostCertificate") == Some(&Value::Bool(false)) {
                    unchecked.insert(name.clone());
                }
                continue;
            }
            put_value(env, key.clone(), value);
        }
    }
    Ok(())
}

/// The environment files from `root` down to the folder of `file`, a nearer
/// one over a farther (a folder's own beside the project's shared secrets):
/// read off the UI thread.
pub fn environments(file: &Path, root: &Path) -> (Environments, Vec<String>) {
    let mut values = BTreeMap::new();
    let mut unchecked = BTreeSet::new();
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let mut dirs = Vec::new();
    let mut dir = file.parent();
    while let Some(here) = dir {
        dirs.push(here);
        if here == root || !here.starts_with(root) {
            break;
        }
        dir = here.parent();
    }
    for here in dirs.into_iter().rev() {
        // Through hostfs: a remote task's files are on its host.
        for (path, private) in [(here.join(ENV_FILE), false), (here.join(PRIVATE_ENV_FILE), true)] {
            let Ok(text) = crate::hostfs::read_to_string(&path) else { continue };
            if let Err(e) = read_env(&text, private, &mut values, &mut unchecked) {
                errors.push(format!("{}: {e}", path.display()));
            }
            files.push(path);
        }
    }
    let names = values.keys().filter(|k| !k.starts_with('$')).cloned().collect();
    (Environments { names, values, unchecked, files }, errors)
}

// ---------------------------------------------------------------- variables

fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    hasher.finish()
}

fn uuid() -> String {
    let (a, b) = (random_u64(), random_u64());
    let a = (a & 0xffff_ffff_ffff_0fff) | 0x4000;
    let b = (b & 0x3fff_ffff_ffff_ffff) | 0x8000_0000_0000_0000;
    format!("{:08x}-{:04x}-{:04x}-{:04x}-{:012x}", a >> 32, (a >> 16) & 0xffff, a & 0xffff, b >> 48, b & 0xffff_ffff_ffff)
}

/// Seconds since 1970 as `2024-05-01T12:00:00Z`.
fn iso(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `n` characters drawn from `of`.
fn random_of(of: &[u8], n: usize) -> String {
    (0..n.min(4096)).map(|_| of[(random_u64() % of.len() as u64) as usize] as char).collect()
}

/// `$uuid`, `$timestamp`, `$random.integer(1, 10)` and the like: made anew for each use.
fn dynamic(name: &str) -> Option<String> {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const ALPHANUMERIC: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let (name, arguments) = match name.split_once('(') {
        Some((name, rest)) => (name.trim(), rest.trim_end().trim_end_matches(')')),
        None => (name, ""),
    };
    let numbers: Vec<i64> = arguments.split(',').filter_map(|a| a.trim().parse().ok()).collect();
    let count = numbers.first().map_or(8, |n| (*n).max(0) as usize);
    Some(match name {
        "$uuid" | "$random.uuid" => uuid(),
        "$timestamp" => now.as_secs().to_string(),
        "$isoTimestamp" => iso(now.as_secs()),
        "$randomInt" => (random_u64() % 1000).to_string(),
        "$random.integer" => {
            let (from, to) = match numbers.as_slice() {
                [from, to] if to > from => (*from, *to),
                _ => (0, 1000),
            };
            (from + (random_u64() % (to - from) as u64) as i64).to_string()
        }
        "$random.alphabetic" => random_of(LETTERS, count),
        "$random.alphanumeric" => random_of(ALPHANUMERIC, count),
        "$random.hexadecimal" => random_of(b"0123456789abcdef", count),
        "$random.email" => format!("{}@example.com", random_of(b"abcdefghijklmnopqrstuvwxyz", 10)),
        _ => return name.strip_prefix("$env.").and_then(|name| std::env::var(name).ok()),
    })
}

/// `{{name}}` put in from `variables`; those not known are kept as they are
/// and listed.
pub fn substitute(text: &str, variables: &BTreeMap<String, String>, unknown: &mut Vec<String>) -> String {
    substitute_within(text, variables, unknown, &mut Vec::new())
}

/// How deep a variable may name another: `@url = {{host}}/api`.
const NESTED: usize = 8;

/// `within` are the variables being put in: one that names itself is not known.
fn substitute_within<'a>(text: &str, variables: &'a BTreeMap<String, String>, unknown: &mut Vec<String>, within: &mut Vec<&'a str>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = after[..close].trim();
        let known = variables.get_key_value(name).filter(|(key, _)| within.len() < NESTED && !within.contains(&key.as_str()));
        match known {
            Some((key, value)) if value.contains("{{") => {
                within.push(key);
                out.push_str(&substitute_within(value, variables, unknown, within));
                within.pop();
            }
            Some((_, value)) => out.push_str(value),
            None => match dynamic(name) {
                Some(value) => out.push_str(&value),
                None => {
                    if !unknown.iter().any(|u| u == name) {
                        unknown.push(name.to_string());
                    }
                    out.push_str(&rest[open..open + 2 + close + 2]);
                }
            },
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------- sending

/// A request with its variables in, ready to go.
#[derive(Clone, Debug, PartialEq)]
pub struct Ready {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub no_redirect: bool,
    pub no_cookies: bool,
    pub timeout: Duration,
    pub connect_timeout: Option<Duration>,
    /// Off for a stand with a certificate of its own making.
    pub verify_tls: bool,
    /// `{{names}}` of the body that nothing set, sent as written.
    pub left: Vec<String>,
}

/// How requests go, from Settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub timeout: Duration,
    pub follow_redirects: bool,
    /// Off, the certificates of this machine's and a private network's hosts
    /// go unchecked; any other host's is checked whatever this says.
    pub verify_local_tls: bool,
    pub scripts: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { timeout: TIMEOUT, follow_redirects: true, verify_local_tls: true, scripts: true }
    }
}

/// Where a request is run from.
#[derive(Clone, Copy, Debug)]
pub struct Place<'a> {
    /// The folder of its file: what `< ./file` is relative to.
    pub dir: &'a Path,
    /// The project: files are read inside it only; scripts' globals and cookies are kept for it.
    pub root: &'a Path,
    pub env: Option<&'a str>,
    /// The environment's private file says its certificate goes unchecked.
    pub unchecked: bool,
}

/// The host of a URL: after `scheme://` and `user@`, before the port and the path.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    }
}

/// This machine, or a host of a private network: where a certificate of
/// one's own making is at home.
fn is_local_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
            std::net::IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        };
    }
    host == "localhost" || [".localhost", ".local", ".test", ".internal", ".lan"].iter().any(|end| host.ends_with(end))
}

fn has_scheme(url: &str) -> bool {
    url.split_once("://").is_some_and(|(scheme, _)| !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')))
}

/// Spaces and what is not ASCII as `%XX`; what is encoded already stays.
fn encode_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        if c.is_ascii() && c != ' ' {
            out.push(c);
        } else {
            let mut bytes = [0; 4];
            for byte in c.encode_utf8(&mut bytes).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// `path` with its `.` and `..` taken out, without asking the disk.
fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            part => out.push(part),
        }
    }
    out
}

/// A file a request names, read: only inside the project, so that a request
/// file from elsewhere cannot send off what else is on the machine.
fn read_within(place: &Place, name: &str) -> Result<Vec<u8>, String> {
    let path = normalized(&place.dir.join(name));
    if !path.starts_with(normalized(place.root)) {
        return Err(format!("{name} is outside the project: a request reads files inside it only. Copy the file into the project."));
    }
    crate::hostfs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// `Basic user password` or `Basic user:password`, as written by hand, encoded.
fn basic(value: &str) -> Option<String> {
    use base64::Engine as _;
    let rest = value.strip_prefix("Basic ")?.trim();
    let (user, password) = rest.split_once(' ').or_else(|| rest.split_once(':'))?;
    Some(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{}", password.trim()))))
}

/// Puts the variables in; reads the files the body names (off the UI thread).
pub fn prepare(request: &Request, variables: &BTreeMap<String, String>, place: &Place, options: &Options) -> Result<Ready, String> {
    let mut unknown = Vec::new();
    let url = substitute(&request.url, variables, &mut unknown);
    let headers: Vec<_> = request
        .headers
        .iter()
        .map(|(k, v)| {
            let (name, value) = (substitute(k, variables, &mut unknown), substitute(v, variables, &mut unknown));
            let value = if name.eq_ignore_ascii_case("authorization") { basic(&value).unwrap_or(value) } else { value };
            (name, value)
        })
        .collect();
    if !unknown.is_empty() {
        return Err(format!("Not set: {}. Choose an environment, or set them in {ENV_FILE} or with @name = value.", unknown.join(", ")));
    }
    // A form's parts are parted by CR LF, and a file is followed by one.
    let form = headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("content-type") && value.to_ascii_lowercase().starts_with("multipart/"));
    // In the body a `{{name}}` nothing set may be the body's own (a template): it goes as written.
    let mut left = Vec::new();
    let mut body = Vec::new();
    for part in &request.body {
        match part {
            BodyPart::Text(text) => {
                let text = substitute(text, variables, &mut left);
                if form {
                    let text = text.replace("\r\n", "\n");
                    body.extend(text.strip_suffix('\n').unwrap_or(&text).replace('\n', "\r\n").into_bytes());
                    body.extend(b"\r\n");
                } else {
                    body.extend(text.into_bytes());
                }
            }
            BodyPart::File(name) => {
                body.extend(read_within(place, &substitute(name, variables, &mut unknown))?);
                if form {
                    body.extend(b"\r\n");
                }
            }
        }
    }
    if !unknown.is_empty() {
        return Err(format!("Not set: {}.", unknown.join(", ")));
    }
    let url = encode_url(&if has_scheme(&url) { url } else { format!("http://{url}") });
    let local = is_local_host(host_of(&url));
    Ok(Ready {
        method: request.method.clone(),
        verify_tls: !(place.unchecked || (local && !options.verify_local_tls)),
        url,
        headers,
        body,
        no_redirect: request.no_redirect || !options.follow_redirects,
        no_cookies: request.no_cookies,
        timeout: request.timeout.unwrap_or(options.timeout),
        connect_timeout: request.connect_timeout,
        left,
    })
}

/// What scripts keep from one request to the next (`client.global`), for
/// each project root, while the app runs.
static GLOBALS: Mutex<BTreeMap<PathBuf, BTreeMap<String, Value>>> = Mutex::new(BTreeMap::new());
/// Who sends for each project root and environment, and whether it checks
/// certificates: it holds the cookies answers set, for the requests after.
static AGENTS: Mutex<BTreeMap<(PathBuf, String, bool), ureq::Agent>> = Mutex::new(BTreeMap::new());

pub fn globals(scope: &Path) -> BTreeMap<String, Value> {
    GLOBALS.lock().unwrap_or_else(|e| e.into_inner()).get(scope).cloned().unwrap_or_default()
}

/// A kept value as a variable's text: a string as it is, anything else as JSON.
pub fn global_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// What a script changed of `before`, put into what is kept now: another
/// request of the project may have kept something meanwhile.
fn keep_globals(scope: &Path, before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>) {
    let mut all = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let kept = all.entry(scope.to_path_buf()).or_default();
    for (name, value) in after {
        if before.get(name) != Some(value) {
            kept.insert(name.clone(), value.clone());
        }
    }
    for name in before.keys().filter(|name| !after.contains_key(*name)) {
        kept.remove(name);
    }
}

/// Forgets what scripts kept and the cookies of the project.
pub fn forget(scope: &Path) {
    GLOBALS.lock().unwrap_or_else(|e| e.into_inner()).remove(scope);
    AGENTS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(root, _, _), _| root != scope);
}

/// A request run whole: its script, itself, its answer's script.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// As it went; none when it could not be made ready.
    pub ready: Option<Ready>,
    pub answer: Result<Answer, String>,
    pub tests: Vec<Test>,
    pub logs: Vec<String>,
    pub script_error: Option<String>,
    pub skipped: Vec<String>,
}

impl Outcome {
    /// Whether a run of several stops here.
    pub fn failed(&self) -> bool {
        self.answer.is_err() || self.script_error.is_some() || self.tests.iter().any(|t| !t.ok)
    }
}

/// The scripts of `inline` and of the files named, as one.
fn script_of(inline: &str, files: &[String], place: &Place, skipped: &mut Vec<String>) -> String {
    let mut code = inline.to_string();
    for name in files {
        match read_within(place, name).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()) {
            Ok(file) => code.push_str(&format!("\n{file}\n")),
            Err(e) => skipped.push(e),
        }
    }
    code
}

/// Runs `request`: blocking, never on the UI thread. A variable is the
/// environment's, else what a script kept, else the file's own, else what
/// the script before the request set: the later over the earlier.
pub fn run(request: &Request, file_vars: &BTreeMap<String, String>, env: &BTreeMap<String, String>, place: &Place, options: &Options) -> Outcome {
    use crate::http_script::{self as script, Call, Given, Reply};
    let mut outcome = Outcome { ready: None, answer: Err(String::new()), tests: Vec::new(), logs: Vec::new(), script_error: None, skipped: request.skipped.clone() };
    let mut kept = globals(place.root);
    let mut own = BTreeMap::new();
    let pre = script_of(&request.pre_script, &request.pre_files, place, &mut outcome.skipped);
    let post = script_of(&request.post_script, &request.post_files, place, &mut outcome.skipped);
    if !options.scripts && (!pre.trim().is_empty() || !post.trim().is_empty()) {
        outcome.skipped.push("its scripts (off in Settings)".into());
    }
    if options.scripts && !pre.trim().is_empty() {
        let body: String = request.body.iter().filter_map(|part| if let BodyPart::Text(text) = part { Some(text.as_str()) } else { None }).collect();
        let given = Given { env, globals: &kept, variables: &own, request: Call { method: &request.method, url: &request.url, headers: &request.headers, body: &body }, response: None };
        let ran = script::run(&pre, &given);
        outcome.logs.extend(ran.logs);
        outcome.tests.extend(ran.tests);
        keep_globals(place.root, &kept, &ran.globals);
        (kept, own) = (ran.globals, ran.variables);
        if let Some(error) = ran.error {
            outcome.script_error = Some(format!("The script before the request: {error}"));
            outcome.answer = Err("Not sent: its script failed.".into());
            return outcome;
        }
    }
    let mut variables = env.clone();
    variables.extend(kept.iter().map(|(k, v)| (k.clone(), global_text(v))));
    variables.extend(file_vars.iter().map(|(k, v)| (k.clone(), v.clone())));
    variables.extend(own.iter().map(|(k, v)| (k.clone(), v.clone())));
    let ready = match prepare(request, &variables, place, options) {
        Ok(ready) => ready,
        Err(error) => {
            outcome.answer = Err(error);
            return outcome;
        }
    };
    if !ready.left.is_empty() {
        outcome.skipped.push(format!("{} in the body (nothing sets them: sent as written)", ready.left.iter().map(|name| format!("{{{{{name}}}}}")).collect::<Vec<_>>().join(", ")));
    }
    // One that keeps no cookies is sent by somebody new, who has none.
    let sender = if ready.no_cookies {
        agent(ready.verify_tls)
    } else {
        let key = (place.root.to_path_buf(), place.env.unwrap_or_default().to_string(), ready.verify_tls);
        let mut agents = AGENTS.lock().unwrap_or_else(|e| e.into_inner());
        if !agents.contains_key(&key) {
            // The cookies and globals of a task whose worktree is gone go with it.
            let gone: Vec<PathBuf> = agents.keys().map(|(root, _, _)| root.clone()).filter(|root| !crate::hosts::is_remote(root) && !root.exists()).collect();
            agents.retain(|(root, _, _), _| !gone.contains(root));
            GLOBALS.lock().unwrap_or_else(|e| e.into_inner()).retain(|root, _| !gone.contains(root));
        }
        agents.entry(key).or_insert_with(|| agent(ready.verify_tls)).clone()
    };
    outcome.answer = send(&ready, &sender);
    if let (true, false, Ok(answer)) = (options.scripts, post.trim().is_empty(), &outcome.answer) {
        let kind = answer.content_type().unwrap_or_default();
        let mime = kind.split(';').next().unwrap_or_default().trim();
        let charset = kind.split(';').skip(1).find_map(|p| p.trim().strip_prefix("charset=")).unwrap_or("utf-8");
        let (sent, got) = (String::from_utf8_lossy(&ready.body), String::from_utf8_lossy(&answer.body));
        let given = Given {
            env,
            globals: &kept,
            variables: &own,
            request: Call { method: &ready.method, url: &ready.url, headers: &ready.headers, body: &sent },
            response: Some(Reply { status: answer.status, headers: &answer.headers, body: &got, mime, charset }),
        };
        let ran = script::run(&post, &given);
        outcome.logs.extend(ran.logs);
        outcome.tests.extend(ran.tests);
        keep_globals(place.root, &kept, &ran.globals);
        outcome.script_error = ran.error.map(|error| format!("The script on the answer: {error}"));
    }
    outcome.ready = Some(ready);
    outcome
}

#[derive(Clone, Debug)]
pub struct Answer {
    pub status: u16,
    pub reason: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Longer than `MOST`: the rest was not read.
    pub cut: bool,
    pub took: Duration,
}

impl Answer {
    pub fn content_type(&self) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.as_str())
    }
}

fn agent(verify_tls: bool) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).disable_verification(!verify_tls).build();
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .max_redirects_will_error(false)
        // To the same host a redirect keeps who asks; to another it does not.
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
        .tls_config(tls)
        .user_agent(format!("ideae/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Sends it and reads the answer: blocking, never on the UI thread. The
/// cookies `agent` holds go with it, and it keeps those the answer sets.
pub fn send(ready: &Ready, agent: &ureq::Agent) -> Result<Answer, String> {
    let (url, body) = (ready.url.as_str(), ready.body.as_slice());
    let redirects = if ready.no_redirect { 0 } else { 10 };
    macro_rules! made {
        ($builder:expr) => {{
            let mut builder = $builder.config().timeout_global(Some(ready.timeout)).timeout_connect(ready.connect_timeout).max_redirects(redirects).build();
            for (name, value) in &ready.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            builder
        }};
    }
    // GET with a body is unusual, not wrong: an Elasticsearch query is one.
    macro_rules! without_body {
        ($builder:expr) => {{
            let builder = made!($builder);
            if body.is_empty() { builder.call() } else { builder.force_send_body().send(body) }
        }};
    }
    let started = Instant::now();
    let response = match ready.method.as_str() {
        "GET" => without_body!(agent.get(url)),
        "HEAD" => without_body!(agent.head(url)),
        "DELETE" => without_body!(agent.delete(url)),
        "OPTIONS" => without_body!(agent.options(url)),
        "TRACE" => without_body!(agent.trace(url)),
        "POST" => made!(agent.post(url)).send(body),
        "PUT" => made!(agent.put(url)).send(body),
        "PATCH" => made!(agent.patch(url)).send(body),
        method => {
            let mut builder = ureq::http::Request::builder().method(method).uri(url);
            for (name, value) in &ready.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            let request = builder.body(body.to_vec()).map_err(|e| e.to_string())?;
            let request = agent.configure_request(request).timeout_global(Some(ready.timeout)).timeout_connect(ready.connect_timeout).max_redirects(redirects).build();
            agent.run(request)
        }
    }
    .map_err(|e| e.to_string())?;
    read(response, started)
}

fn read(mut response: ureq::http::Response<ureq::Body>, started: Instant) -> Result<Answer, String> {
    use std::io::Read as _;
    let status = response.status();
    let version = format!("{:?}", response.version());
    let headers = response.headers().iter().map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
    let mut body = Vec::new();
    let mut reader = response.body_mut().with_config().limit(u64::MAX).reader().take(MOST + 1);
    reader.read_to_end(&mut body).map_err(|e| e.to_string())?;
    let cut = body.len() as u64 > MOST;
    body.truncate(MOST as usize);
    Ok(Answer { status: status.as_u16(), reason: status.canonical_reason().unwrap_or("").to_string(), version, headers, body, cut, took: started.elapsed() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here<'a>(root: &'a Path) -> Place<'a> {
        Place { dir: root, root, env: None, unchecked: false }
    }

    const FILE: &str = r#"@host = localhost:8080
### List users
GET http://{{host}}/users
  ?page=2
  &size=10
Accept: application/json

###
# @name create
# @no-redirect
POST http://{{host}}/users HTTP/1.1
Content-Type: application/json

{
  "name": "{{name}}"
}

> {%
  client.test("ok", () => {});
%}
###
example.com/health
"#;

    #[test]
    fn a_file_is_its_requests_parted_by_three_hashes() {
        let (requests, variables) = parse(FILE);
        assert_eq!(variables.get("host").map(String::as_str), Some("localhost:8080"));
        assert_eq!(requests.len(), 3);
        let list = &requests[0];
        assert_eq!(list.name.as_deref(), Some("List users"));
        assert_eq!((list.method.as_str(), list.url.as_str()), ("GET", "http://{{host}}/users?page=2&size=10"));
        assert_eq!(list.headers, [("Accept".to_string(), "application/json".to_string())]);
        assert!(list.body.is_empty());
        let create = &requests[1];
        assert_eq!(create.name.as_deref(), Some("create"));
        assert!(create.no_redirect);
        assert_eq!(create.url, "http://{{host}}/users");
        assert_eq!(create.body, [BodyPart::Text("{\n  \"name\": \"{{name}}\"\n}".into())]);
        assert!(create.post_script.contains("client.test(\"ok\""), "{}", create.post_script);
        assert!(create.skipped.is_empty());
        let health = &requests[2];
        assert_eq!((health.method.as_str(), health.url.as_str(), health.title().as_str()), ("GET", "example.com/health", "GET example.com/health"));
        assert_eq!(&FILE[health.line.clone()], "example.com/health");
    }

    #[test]
    fn the_cursor_picks_the_request_it_is_in() {
        let (requests, _) = parse(FILE);
        assert_eq!(at(&requests, FILE.find("Accept").unwrap()).unwrap().name.as_deref(), Some("List users"));
        assert_eq!(at(&requests, FILE.find("\"name\"").unwrap()).unwrap().name.as_deref(), Some("create"));
        assert_eq!(at(&requests, FILE.len()).unwrap().url, "example.com/health");
        assert!(at(&requests, 0).is_none_or(|r| r.name.as_deref() == Some("List users")));
    }

    #[test]
    fn variables_come_from_the_environment_and_the_file() {
        let (requests, file_vars) = parse("@base = {{host}}/api\nGET {{base}}/x?id={{$uuid}}\n");
        let env = BTreeMap::from([("host".to_string(), "https://example.com".to_string())]);
        let all = |env: &BTreeMap<String, String>| env.iter().chain(&file_vars).map(|(k, v)| (k.clone(), v.clone())).collect::<BTreeMap<_, _>>();
        let ready = prepare(&requests[0], &all(&env), &here(Path::new(".")), &Options::default()).unwrap();
        assert!(ready.url.starts_with("https://example.com/api/x?id="), "{}", ready.url);
        assert_eq!(ready.url.len(), "https://example.com/api/x?id=".len() + 36);
        let missing = prepare(&requests[0], &all(&BTreeMap::new()), &here(Path::new(".")), &Options::default()).unwrap_err();
        assert!(missing.starts_with("Not set: host"), "{missing}");
    }

    #[test]
    fn environments_are_read_from_the_nearest_folder_the_private_one_over() {
        let dir = std::env::temp_dir().join(format!("ideae-http-env-{}", std::process::id()));
        let api = dir.join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(dir.join(ENV_FILE), r#"{"dev": {"host": "localhost", "port": 8080}, "$shared": {"v": "1"}, "prod": {"host": "example.com"}}"#).unwrap();
        std::fs::write(dir.join(PRIVATE_ENV_FILE), r#"{"dev": {"token": "secret"}}"#).unwrap();
        // A folder's own file, over the project's: its secrets still come from above.
        std::fs::write(api.join(ENV_FILE), r#"{"dev": {"port": 9090}, "local": {}}"#).unwrap();
        let (envs, errors) = environments(&api.join("a.http"), &dir);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(envs.names, ["dev", "local", "prod"]);
        let dev = envs.variables(Some("dev"));
        assert_eq!((dev["host"].as_str(), dev["port"].as_str(), dev["token"].as_str(), dev["v"].as_str()), ("localhost", "9090", "secret", "1"));
        assert_eq!(envs.variables(None).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_timestamp_is_told_in_utc() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_714_564_800), "2024-05-01T12:00:00Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn a_request_goes_and_its_answer_comes_back() {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let answer = format!("{{\"got\": {}}}", String::from_utf8(body).unwrap());
            write!(&stream, "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len()).unwrap();
            head
        });
        let text = format!("GET http://127.0.0.1:{port}/q\nX-Test: {{{{who}}}}\n\n{{\"a\": 1}}\n");
        let (requests, _) = parse(&text);
        let env = BTreeMap::from([("who".to_string(), "me".to_string())]);
        let answer = send(&prepare(&requests[0], &env, &here(Path::new(".")), &Options::default()).unwrap(), &agent(true)).unwrap();
        let head = server.join().unwrap();
        assert!(head.starts_with("GET /q HTTP/1.1"), "{head}");
        assert!(head.to_ascii_lowercase().contains("x-test: me"), "{head}");
        assert_eq!((answer.status, answer.reason.as_str()), (201, "Created"));
        assert_eq!(answer.content_type(), Some("application/json"));
        assert_eq!(String::from_utf8(answer.body).unwrap(), "{\"got\": {\"a\": 1}}");
    }

    /// Answers each request with its path and headers, as JSON.
    fn echo(requests: usize) -> u16 {
        use std::io::{BufRead as _, BufReader, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..requests {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut first, mut signed) = (String::new(), String::new());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if first.is_empty() {
                        first = line.split_whitespace().nth(1).unwrap_or_default().to_string();
                    }
                    if let Some(v) = line.strip_prefix("x-sig: ").or_else(|| line.strip_prefix("X-Sig: ")) {
                        signed = v.trim().to_string();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let answer = format!("{{\"path\": \"{first}\", \"sig\": \"{signed}\", \"token\": \"t-7\"}}");
                write!(&stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len()).unwrap();
            }
        });
        port
    }

    #[test]
    fn a_request_s_script_hands_what_it_read_to_the_next() {
        let port = echo(2);
        let text = format!(
            r#"### login
GET http://127.0.0.1:{port}/login

> {{%
    client.test("ok", () => client.assert(response.status === 200, "got " + response.status));
    client.global.set("token", response.body.token);
%}}

### signed
< {{%
    request.variables.set("sig", crypto.hmac.sha256().withTextSecret(request.environment.get("secret")).updateWithText(client.global.get("token")).digest().toHex());
%}}
GET http://127.0.0.1:{port}/me?token={{{{token}}}}
X-Sig: {{{{sig}}}}

> {{%
    client.test("the path", () => client.assert(response.body.path === "/me?token=t-7", response.body.path));
    client.test("wrong on purpose", () => client.assert(response.body.sig === "", "signed with " + response.body.sig.length + " characters"));
%}}
"#
        );
        let (requests, file_vars) = parse(&text);
        assert_eq!(requests.len(), 2);
        let scope = std::env::temp_dir().join(format!("ideae-http-chain-{}", std::process::id()));
        // There: what is kept for a worktree that is gone is dropped.
        std::fs::create_dir_all(&scope).unwrap();
        let env = BTreeMap::from([("secret".to_string(), "key".to_string())]);
        let first = run(&requests[0], &file_vars, &env, &here(&scope), &Options::default());
        assert!(!first.failed(), "{first:?}");
        assert_eq!(globals(&scope).get("token"), Some(&serde_json::json!("t-7")));
        let second = run(&requests[1], &file_vars, &env, &here(&scope), &Options::default());
        assert_eq!(second.script_error, None);
        assert_eq!(second.tests.iter().map(|t| (t.name.as_str(), t.ok)).collect::<Vec<_>>(), [("the path", true), ("wrong on purpose", false)]);
        assert_eq!(second.tests[1].message, "signed with 64 characters");
        assert!(second.failed());
        // With scripts off the variable they would set is missing: nothing goes.
        forget(&scope);
        let off = run(&requests[1], &file_vars, &env, &here(&scope), &Options { scripts: false, ..Default::default() });
        assert!(off.answer.unwrap_err().starts_with("Not set: token, sig"));
        std::fs::remove_dir_all(&scope).unwrap();
    }

    #[test]
    fn what_follows_a_handler_is_not_the_body() {
        let text = "POST http://x/a\nContent-Type: application/json\n\n{\"a\": 1}\n\n> {%\n  client.log(\"100%} done\");\n%}\n\n# a note\n<> 2024-05-01T120000.200.json\n\nGET http://x/b\nAccept: */*\n\n<> 2024-05-01T120001.200.json\n\n> {% client.log(2) %}\n";
        let (requests, _) = parse(text);
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert_eq!(requests[0].body, [BodyPart::Text("{\"a\": 1}".into())]);
        assert!(requests[0].post_script.contains("100%} done"), "{}", requests[0].post_script);
        assert_eq!((requests[1].url.as_str(), requests[1].body.len(), requests[1].post_script.trim()), ("http://x/b", 0, "client.log(2)"));
        // The cursor in the second is in the second.
        assert_eq!(at(&requests, text.find("Accept").unwrap()).unwrap().url, "http://x/b");

        let (requests, _) = parse("# @connectTimeout=1200000\n# @timeout 600 ms\n# @no-cookie-jar\nget http://x/a HTTP/2\n\n<root>\n  < 5\n</root>\n");
        let request = &requests[0];
        assert_eq!((request.method.as_str(), request.url.as_str()), ("GET", "http://x/a"));
        assert_eq!((request.connect_timeout, request.timeout, request.no_cookies), (Some(LONGEST), Some(Duration::from_millis(600)), true));
        assert_eq!(request.body, [BodyPart::Text("<root>\n  < 5\n</root>".into())]);
    }

    #[test]
    fn an_environment_s_objects_are_read_by_their_paths() {
        let dir = std::env::temp_dir().join(format!("ideae-http-nested-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ENV_FILE), r#"{"dev": {"card": {"id": 7, "tags": ["a", "b"]}, "SSLConfiguration": {"verifyHostCertificate": false}}, "prod": {"x": "1"}}"#).unwrap();
        std::fs::write(dir.join(PRIVATE_ENV_FILE), r#"{"prod": {"SSLConfiguration": {"verifyHostCertificate": false}}}"#).unwrap();
        let (envs, errors) = environments(&dir.join("a.http"), &dir);
        assert!(errors.is_empty(), "{errors:?}");
        let dev = envs.variables(Some("dev"));
        let mut unknown = Vec::new();
        assert_eq!(substitute("{{card.id}} {{card.tags.1}} {{card.tags[0]}} {{card}}", &dev, &mut unknown), r#"7 b a {"id":7,"tags":["a","b"]}"#);
        assert!(unknown.is_empty() && !dev.contains_key("SSLConfiguration"));
        // Only the file kept out of git may say a certificate goes unchecked.
        assert_eq!((envs.unchecked(Some("dev")), envs.unchecked(Some("prod")), envs.unchecked(None)), (false, true, false));
        assert!(is_production("prod") && is_production("Production-EU") && is_production("live") && !is_production("dev") && !is_production("staging"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_request_is_made_ready_as_it_is_meant() {
        let root = std::env::temp_dir().join(format!("ideae-http-ready-{}", std::process::id()));
        std::fs::create_dir_all(root.join("api")).unwrap();
        std::fs::write(root.join("api/a.bin"), [1u8, 2, 3]).unwrap();
        let place = Place { dir: &root.join("api"), root: &root, env: None, unchecked: false };
        let ready = |text: &str, variables: &[(&str, &str)], options: &Options| {
            let (requests, file_vars) = parse(text);
            let mut all: BTreeMap<String, String> = variables.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            all.extend(file_vars);
            prepare(&requests[0], &all, &place, options)
        };
        let plain = Options::default();

        // A form: CR LF between its lines and after a file.
        let form = ready("POST http://x/up\nContent-Type: multipart/form-data; boundary=b\n\n--b\nContent-Disposition: form-data; name=\"f\"; filename=\"a.bin\"\n\n< ./a.bin\n--b--\n", &[], &plain).unwrap();
        assert_eq!(form.body, b"--b\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.bin\"\r\n\r\n\x01\x02\x03\r\n--b--\r\n");

        // A file outside the project is not read.
        assert!(ready("POST http://x/up\n\n< ../../outside.txt\n", &[], &plain).unwrap_err().contains("outside the project"));

        // A name nothing set: in the URL the request does not go, in the body it goes as written.
        assert!(ready("GET http://x/{{id}}\n", &[], &plain).unwrap_err().starts_with("Not set: id"));
        let template = ready("POST http://x/t\n\n{\"tpl\": \"Hello {{user.name}}\", \"id\": \"{{id}}\"}\n", &[("id", "7")], &plain).unwrap();
        assert_eq!((String::from_utf8(template.body).unwrap().as_str(), template.left.as_slice()), ("{\"tpl\": \"Hello {{user.name}}\", \"id\": \"7\"}", ["user.name".to_string()].as_slice()));

        // One that names itself is not known, and does not grow without end.
        assert!(ready("@a = {{a}}{{a}}\nGET http://x/{{a}}\n", &[], &plain).unwrap_err().starts_with("Not set: a"));

        // The URL: its own scheme in a query is not the request's; other scripts and spaces are encoded.
        assert_eq!(ready("GET 127.0.0.1:8080/cb?next=http://o/привет мир\n", &[], &plain).unwrap().url, "http://127.0.0.1:8080/cb?next=http://o/%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82%20%D0%BC%D0%B8%D1%80");

        // Basic written by hand is encoded; one encoded already is left.
        let auth = |value: &str| ready(&format!("GET http://x/\nAuthorization: {value}\n"), &[("u", "me"), ("p", "secret")], &plain).unwrap().headers[0].1.clone();
        assert_eq!((auth("Basic {{u}} {{p}}"), auth("Basic {{u}}:{{p}}"), auth("Basic bWU6c2VjcmV0"), auth("Bearer a:b c")), ("Basic bWU6c2VjcmV0".into(), "Basic bWU6c2VjcmV0".into(), "Basic bWU6c2VjcmV0".into(), "Bearer a:b c".into()));

        // A certificate goes unchecked only at home, or where the environment's private file says so.
        let loose = Options { verify_local_tls: false, ..plain };
        let checked = |url: &str, options: &Options| ready(&format!("GET {url}\n"), &[], options).unwrap().verify_tls;
        assert!(checked("https://example.com/", &loose) && checked("https://localhost:8443/", &plain));
        assert!(!checked("https://localhost:8443/", &loose) && !checked("https://client.app.localhost/", &loose) && !checked("https://10.0.0.5/", &loose) && !checked("https://[::1]:8443/", &loose));
        let unchecked = Place { unchecked: true, ..place };
        let (requests, _) = parse("GET https://example.com/\n");
        assert!(!prepare(&requests[0], &BTreeMap::new(), &unchecked, &plain).unwrap().verify_tls);
        std::fs::remove_dir_all(&root).unwrap();

        let mut unknown = Vec::new();
        let made = substitute("{{$random.integer(5, 7)}} {{$random.alphabetic(4)}} {{$random.hexadecimal(6)}}", &BTreeMap::new(), &mut unknown);
        let parts: Vec<&str> = made.split(' ').collect();
        assert!(unknown.is_empty() && matches!(parts[0], "5" | "6") && parts[1].len() == 4 && parts[2].len() == 6, "{made}");
    }

    #[test]
    fn a_cookie_an_answer_set_goes_with_the_next_request() {
        use std::io::{BufRead as _, BufReader, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..3 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut cookie = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("cookie:") {
                        cookie = value.trim().to_string();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                seen.push(cookie);
                write!(&stream, "HTTP/1.1 200 OK\r\nSet-Cookie: session=s-1; Path=/\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
            }
            seen
        });
        let text = format!("GET http://127.0.0.1:{port}/login\n\n###\nGET http://127.0.0.1:{port}/me\n\n###\n# @no-cookie-jar\nGET http://127.0.0.1:{port}/anonymous\n");
        let (requests, file_vars) = parse(&text);
        let root = std::env::temp_dir().join(format!("ideae-http-cookies-{}", std::process::id()));
        // There: what is kept for a worktree that is gone is dropped.
        std::fs::create_dir_all(&root).unwrap();
        let place = Place { dir: &root, root: &root, env: Some("dev"), unchecked: false };
        for request in &requests {
            assert!(!run(request, &file_vars, &BTreeMap::new(), &place, &Options::default()).failed());
        }
        assert_eq!(server.join().unwrap(), ["", "session=s-1", ""]);
        forget(&root);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scripts_keep_what_they_set_as_it_was_and_beside_each_other() {
        let root = std::env::temp_dir().join(format!("ideae-http-kept-{}", std::process::id()));
        // There: what is kept for a worktree that is gone is dropped.
        std::fs::create_dir_all(&root).unwrap();
        let before = globals(&root);
        // Two runs that began from the same state: neither undoes the other.
        keep_globals(&root, &before, &BTreeMap::from([("token".to_string(), serde_json::json!("t"))]));
        keep_globals(&root, &before, &BTreeMap::from([("id".to_string(), serde_json::json!(42))]));
        let kept = globals(&root);
        assert_eq!((kept.get("token"), kept.get("id")), (Some(&serde_json::json!("t")), Some(&serde_json::json!(42))));
        assert_eq!((global_text(&kept["token"]), global_text(&kept["id"])), ("t".to_string(), "42".to_string()));
        // One cleared is gone.
        keep_globals(&root, &kept, &BTreeMap::from([("id".to_string(), serde_json::json!(42))]));
        assert_eq!(globals(&root).keys().collect::<Vec<_>>(), ["id"]);
        forget(&root);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
