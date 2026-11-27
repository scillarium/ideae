//! A browser tab: a page in headless Chrome, shown through CDP screencast
//! frames, with mouse and keyboard sent back over CDP.
//!
//! Every task has a Chrome of its own: a profile kept between runs (logins
//! stay), apart from other tasks' (variants on other ports of localhost
//! share no cookies), and a DevTools port that stays the task's. Its
//! terminals get it as IDEAE_BROWSER_CDP, so Playwright MCP
//! (`--cdp-endpoint`) drives the pages you see; a page opened there, or by
//! `window.open` and `target=_blank`, comes up as a tab of the task.
//!
//! The user's own browser (`Owner::User`), one per space, is spoken to over
//! a pipe instead (`pipe.rs`): it has no port, nothing an agent could call.

mod pipe;
mod seed;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::ErrorKind,
    net::{TcpListener, TcpStream, ToSocketAddrs as _},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use base64::Engine as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, protocol::Role, protocol::WebSocketConfig};

// ---------------------------------------------------------------- chrome

/// Whom a Chrome runs for: a task, by its worktree; the user, for the
/// sites they sign in to, out of agents' reach; or the pictures of
/// variants' sites, which want no profile kept.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Owner {
    Task(PathBuf),
    User(UserBrowser),
    Pictures,
}

/// The user's browsers: one for each space, its logins apart from the
/// others', and one for projects in no space.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum UserBrowser {
    Shared,
    Space(String),
}

/// A project's own browser as its tabs show it: whose it is and the mark
/// it is known by, its space's or that of projects in none.
#[derive(Clone, Debug, PartialEq)]
pub struct UserMark {
    pub browser: UserBrowser,
    pub mark: String,
    pub color: u8,
    pub name: String,
}

impl Default for UserMark {
    fn default() -> Self {
        Self { browser: UserBrowser::Shared, mark: crate::workspace::spaces::ALL.into(), color: 7, name: "Shared".into() }
    }
}

/// Each project's own browser, by the project's path: set as its space is
/// known or changes, read by its tasks as they open one of its tabs.
static USER_MARKS: std::sync::RwLock<Option<HashMap<PathBuf, UserMark>>> = std::sync::RwLock::new(None);

pub fn set_user_mark(project: &Path, mark: UserMark) {
    USER_MARKS.write().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(project.to_path_buf(), mark);
}

/// The browser of the project a task's root is of; the shared one when its project is not known.
pub fn user_mark(root: &Path) -> UserMark {
    let project = crate::project_config::project_of(root);
    USER_MARKS.read().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|all| all.get(&project).or_else(|| all.get(root)).cloned()).unwrap_or_default()
}

struct Chrome {
    /// 0 for the user's: spoken to over `pipe`.
    port: u16,
    pipe: Option<Arc<pipe::Pipe>>,
    ws_path: String,
    child: Mutex<Child>,
    profile: PathBuf,
    temporary: bool,
    /// Pages a tab opened or took: the others are offered to the task.
    claimed: Mutex<HashSet<String>>,
    /// Its major version, for a phone's user agent.
    major: String,
    /// The pages it opened as it started, before any tab's: no tab's own.
    first: Mutex<Vec<String>>,
}

impl Chrome {
    fn alive(&self) -> bool {
        self.child.lock().is_ok_and(|mut child| matches!(child.try_wait(), Ok(None)))
    }

    fn claim(&self, target: &str) {
        self.claimed.lock().unwrap().insert(target.to_string());
    }
}

impl Chrome {
    /// Asked to close, so it writes its profile out (its cookies are in an
    /// open transaction a while): killed, the last logins are lost. Killed
    /// when it takes too long.
    fn close(&self) {
        if !self.temporary && self.alive() {
            if let Ok(mut socket) = connect(self) {
                let _ = socket.send("Browser.close", json!({}));
            }
            let started = Instant::now();
            while self.alive() && started.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        self.close();
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.profile);
        } else {
            let _ = std::fs::remove_file(self.profile.join("DevToolsActivePort"));
        }
    }
}

/// One owner's Chrome, the pages open in it, and where the pages nobody
/// asked for go.
#[derive(Default)]
struct Slot {
    chrome: Mutex<Option<Arc<Chrome>>>,
    pages: AtomicUsize,
    /// Counts closings, so only the last one's idle time ends Chrome.
    closed: AtomicU64,
    adopt: Mutex<Option<async_channel::Sender<PageEvent>>>,
    /// A user's Chrome serves the tabs of many tasks: whose each page is
    /// (by its id, a tab's or one a tab's page opened), and where each
    /// task is told of the pages its pages open and of their downloads.
    page_tasks: Mutex<HashMap<String, PathBuf>>,
    tasks: Mutex<HashMap<PathBuf, async_channel::Sender<PageEvent>>>,
    /// Listening at the task's port while its Chrome is ended: told to stop
    /// when a tab starts one.
    waker: Mutex<Option<Arc<AtomicBool>>>,
    /// Its page last pressed so as to open a link apart (`opens_a_tab`), and
    /// when: a page of no opener a moment after is that page's.
    clicked: Mutex<Option<(String, Instant)>>,
}

/// What the task's Chrome tells the task, beside what its tabs show.
pub enum PageEvent {
    /// A page no tab opened: by a page of the task's (`window.open`, a link
    /// to a new window) when `by_page`, else from outside, as an agent's
    /// Playwright makes one, to show without taking the user's eyes.
    Opened { target: String, by_page: bool },
    /// A download done, where it is.
    Downloaded(PathBuf),
    DownloadFailed(String),
}

static SLOTS: OnceLock<Mutex<HashMap<Owner, Arc<Slot>>>> = OnceLock::new();

fn slot(owner: &Owner) -> Arc<Slot> {
    SLOTS.get_or_init(Default::default).lock().unwrap().entry(owner.clone()).or_default().clone()
}

/// The system asks for less motion: read as a Chrome starts, off the UI thread.
static SYSTEM_CALM: AtomicBool = AtomicBool::new(false);

/// GNOME's animations turned off, macOS's Reduce motion.
fn system_asks_less_motion() -> bool {
    let read = |program: &str, args: &[&str]| {
        crate::procs::command(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok().map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    if cfg!(target_os = "macos") {
        read("defaults", &["read", "com.apple.universalaccess", "reduceMotion"]).as_deref() == Some("1")
    } else if cfg!(unix) {
        read("gsettings", &["get", "org.gnome.desktop.interface", "enable-animations"]).as_deref() == Some("false")
    } else {
        false
    }
}

/// Pages told to keep still (`prefers-reduced-motion`), and scrolled without
/// animation: as Settings say, else as the system asks.
pub fn calm() -> bool {
    crate::state::with_settings(|s| match s.web_motion.as_str() {
        "reduce" => true,
        "full" => false,
        _ => SYSTEM_CALM.load(Ordering::Relaxed),
    })
}

#[cfg(unix)]
fn find_chrome() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    for app in
        ["Google Chrome.app/Contents/MacOS/Google Chrome", "Chromium.app/Contents/MacOS/Chromium", "Microsoft Edge.app/Contents/MacOS/Microsoft Edge"]
    {
        let path = PathBuf::from("/Applications").join(app);
        if path.is_file() {
            return Some(path);
        }
    }
    ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]
        .into_iter()
        .find(|p| crate::procs::command(p).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()))
        .map(PathBuf::from)
}

/// `chrome.exe --version` opens a window instead of answering, so the
/// usual install places are looked at. Edge, which every Windows has, is
/// Chromium too.
#[cfg(windows)]
fn find_chrome() -> Option<PathBuf> {
    let roots: Vec<PathBuf> = ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
    let in_roots = |rel: &str| roots.iter().map(|r| r.join(rel)).find(|p| p.is_file());
    in_roots(r"Google\Chrome\Application\chrome.exe")
        .or_else(|| crate::procs::which("chrome.exe"))
        .or_else(|| in_roots(r"Chromium\Application\chrome.exe"))
        .or_else(|| in_roots(r"Microsoft\Edge\Application\msedge.exe"))
}

/// An address of the task's own site: a service of it, or a port one of
/// its processes listens on.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub label: String,
    pub port: u16,
    /// Something listens there now.
    pub up: bool,
}

impl Place {
    pub fn address(&self) -> String {
        format!("http://localhost:{}", self.port)
    }
}

/// The task's own addresses, as the task finds them: asked off the UI thread.
pub type Places = Arc<dyn Fn() -> Vec<Place> + Send + Sync>;

/// The port of an address of this machine.
pub fn local_port(url: &str) -> Option<u16> {
    local_address(url)?.rsplit_once(':')?.1.parse().ok()
}

/// The pixels to one of the screen the window is on, in hundredths, as last
/// drawn: what a Chrome started from here is told its screen has.
static SCREEN_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);

/// Told by the window when it is made and when a page is put in a tab: a
/// task's Chrome can be started before any tab draws, by its agent.
pub fn note_screen(scale: f32) {
    // Not by the windows the tests make, many at a time: the one test of this sets it itself.
    if !cfg!(test) {
        SCREEN_SCALE.store((scale * 100.).round() as u32, std::sync::atomic::Ordering::Relaxed);
    }
}

/// FNV-1a: the same in every build, as a port given to a running agent
/// must stay right for the window of the next build.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn task_key(root: &Path) -> String {
    format!("{}\0{}", crate::state::state_dir().display(), root.display())
}

/// The task's DevTools port, for an agent of it on another machine.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn cdp_port(root: &Path) -> u16 {
    task_port(root)
}

/// The task's DevTools port: below the ephemeral range, where the system
/// gives no program one of its own choosing.
fn task_port(root: &Path) -> u16 {
    port_of(&task_key(root))
}

fn port_of(key: &str) -> u16 {
    21000 + (fnv(key) % 9000) as u16
}

/// What a task's terminals are told: where its browser is, once a Web tab
/// opened it.
pub fn task_env(root: &Path) -> (String, String) {
    ("IDEAE_BROWSER_CDP".to_string(), format!("http://127.0.0.1:{}", task_port(root)))
}

/// The repository a worktree belongs to: tasks of one share their first
/// logins.
fn project_of(root: &Path) -> String {
    let git = root.join(".git");
    let common = match std::fs::read_to_string(&git) {
        // A worktree: `gitdir: <repo>/.git/worktrees/<name>`.
        Ok(text) => text
            .trim()
            .strip_prefix("gitdir:")
            .map(|dir| PathBuf::from(dir.trim()))
            .and_then(|dir| dir.parent()?.parent().map(Path::to_path_buf))
            .unwrap_or(git),
        Err(_) => git,
    };
    common.canonicalize().unwrap_or(common).display().to_string()
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to).map(|_| ())
    }
}

fn profiles_dir() -> PathBuf {
    // Tests keep theirs out of the state of the app that runs them.
    if cfg!(test) {
        std::env::temp_dir().join(format!("ideae-web-test-profiles-{}", std::process::id()))
    } else {
        crate::state::state_dir().join("web")
    }
}

fn profile_of(root: &Path) -> PathBuf {
    profiles_dir().join(format!("{:016x}", fnv(&task_key(root))))
}

/// A task gone (archived, deleted): its Chrome ends and its profile, the
/// logins it held, goes at once, not when another task next makes one.
pub fn forget(root: &Path) {
    let owner = Owner::Task(root.to_path_buf());
    let slot = SLOTS.get().and_then(|slots| slots.lock().ok()?.remove(&owner));
    if let Some(slot) = slot {
        if let Some(stop) = slot.waker.lock().unwrap().take() {
            stop.store(true, Ordering::SeqCst);
            // Out of its wait for a call, it lets go of the port.
            let _ = TcpStream::connect_timeout(&([127, 0, 0, 1], task_port(root)).into(), Duration::from_millis(200));
        }
        let chrome = slot.chrome.lock().ok().and_then(|mut c| c.take());
        if let Some(chrome) = chrome {
            chrome.close();
        }
    }
    let _ = std::fs::remove_dir_all(profile_of(root));
}

/// The user's browsers' profiles, in the state's folder: a run with a state
/// of its own (IDEAE_STATE, a debug build's) sees none of the user's logins.
fn user_profiles_dir() -> PathBuf {
    if cfg!(test) {
        std::env::temp_dir().join(format!("ideae-web-user-test-profiles-{}", std::process::id()))
    } else {
        crate::state::state_dir().join("web-user")
    }
}

/// Where a user's browser's profile is, made or not.
fn user_profile_path(browser: &UserBrowser) -> PathBuf {
    let name = match browser {
        UserBrowser::Shared => "shared".to_string(),
        UserBrowser::Space(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => format!("space-{id}"),
        UserBrowser::Space(id) => format!("space-{:016x}", fnv(id)),
    };
    user_profiles_dir().join(name)
}

/// A user's browser's profile, made when it has none: no logins seeded
/// from any other, and only the user may read it.
fn user_profile(browser: &UserBrowser) -> Result<PathBuf> {
    let dir = user_profile_path(browser);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        let all = user_profiles_dir();
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        for made in [&all, &dir] {
            std::fs::set_permissions(made, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A user's browser forgotten: its Chrome ended and its profile, every
/// login in it, removed. Not for the UI thread: Chrome is given a moment to end.
pub fn forget_user(browser: &UserBrowser) {
    let owner = Owner::User(browser.clone());
    let slot = SLOTS.get().and_then(|slots| slots.lock().ok()?.remove(&owner));
    if let Some(chrome) = slot.and_then(|slot| slot.chrome.lock().ok().and_then(|mut c| c.take())) {
        chrome.close();
    }
    let _ = std::fs::remove_dir_all(user_profile_path(browser));
}

/// The task's profile, made when it has none; profiles of worktrees gone
/// are removed on the way.
fn task_profile(root: &Path) -> Result<PathBuf> {
    let all = profiles_dir();
    let dir = profile_of(root);
    let project = project_of(root);
    let mut seed: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&all).into_iter().flatten().flatten() {
        let other = entry.path();
        if other == dir {
            continue;
        }
        let of = std::fs::read_to_string(other.join("root")).unwrap_or_default();
        // One with no root yet may be another task's being made this moment: left a while.
        let young = || std::fs::metadata(&other).and_then(|m| m.modified()).ok().and_then(|at| at.elapsed().ok()).is_some_and(|age| age < Duration::from_secs(60));
        if of.is_empty() && young() {
            continue;
        }
        if of.is_empty() || !Path::new(&of).exists() {
            let _ = std::fs::remove_dir_all(&other);
            continue;
        }
        if std::fs::read_to_string(other.join("project")).ok().as_deref() != Some(project.as_str()) {
            continue;
        }
        let used = ["Default/Network/Cookies", "Default/Cookies"]
            .iter()
            .filter_map(|f| std::fs::metadata(other.join(f)).and_then(|m| m.modified()).ok())
            .max();
        if let Some(used) = used.filter(|used| seed.as_ref().is_none_or(|(at, _)| used > at)) {
            seed = Some((used, other));
        }
    }
    if !dir.join("root").exists() {
        std::fs::create_dir_all(&dir)?;
        // Logged in to the project's stands as its last task was; to nothing else.
        if let Some((_, from)) = seed {
            seed::carry(&from, &dir, &seed::named_stands(root));
        }
        std::fs::write(dir.join("project"), &project)?;
        std::fs::write(dir.join("root"), root.display().to_string())?;
    }
    Ok(dir)
}

/// The owner's Chrome, started on first use, or again when it ended.
fn chrome(owner: &Owner) -> Result<Arc<Chrome>> {
    let slot = slot(owner);
    let mut held = slot.chrome.lock().unwrap();
    if let Some(chrome) = held.as_ref().filter(|c| c.alive()) {
        return Ok(chrome.clone());
    }
    *held = None;
    // The port is the waker's while Chrome sleeps: it lets go of it.
    if let Some(stop) = slot.waker.lock().unwrap().take() {
        stop.store(true, Ordering::SeqCst);
        if let Owner::Task(root) = owner {
            let port = task_port(root);
            let _ = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200));
            let started = Instant::now();
            while TcpListener::bind(("127.0.0.1", port)).is_err() && started.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let program = std::env::var_os("IDEAE_CHROME")
        .map(PathBuf::from)
        .or_else(find_chrome)
        .ok_or_else(|| anyhow!("No Chrome or Chromium found: install one, or name it in IDEAE_CHROME"))?;
    let program = program.as_path();
    let (profile, temporary, port) = match owner {
        Owner::Task(root) => (task_profile(root)?, false, task_port(root)),
        Owner::User(browser) => (user_profile(browser)?, false, 0),
        Owner::Pictures => (crate::state::runtime_dir().join(format!("chrome-{}", std::process::id())), true, 0),
    };
    if temporary {
        let _ = std::fs::remove_dir_all(&profile);
    }
    std::fs::create_dir_all(&profile)?;
    SYSTEM_CALM.store(system_asks_less_motion(), Ordering::Relaxed);
    let mut command = crate::procs::command(program);
    command
        .args(["--headless=new", "--no-first-run", "--no-default-browser-check", "--hide-scrollbars"])
        .args(if calm() { &["--force-prefers-reduced-motion", "--disable-smooth-scrolling"][..] } else { &[] })
        .args(cookie_key(owner))
        .args(as_ordinary(program))
        // As many pixels to one as the window's screen has: the frames of a
        // page's stream are of the screen Chrome thinks it is on, whatever
        // the page is told, and on one of two to one they came with half.
        .args(Some(SCREEN_SCALE.load(std::sync::atomic::Ordering::Relaxed)).filter(|scale| *scale > 100).map(|scale| format!("--force-device-scale-factor={}", scale as f32 / 100.)))
        .arg(format!("--user-data-dir={}", profile.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let chrome = if matches!(owner, Owner::User(_)) {
        over_pipe(command, program, profile)?
    } else {
        over_port(command, program, profile, port, temporary)?
    };
    // Taken now, under the lock a tab waits for: a tab's page made at the same time is not taken for one of them.
    if let Ok(mut socket) = connect(&chrome) {
        if let Ok(open) = socket.call("Target.getTargets", json!({}), &mut Vec::new()) {
            for info in open["targetInfos"].as_array().into_iter().flatten().filter(|i| i["type"] == "page") {
                let id = info["targetId"].as_str().unwrap_or_default().to_string();
                chrome.claim(&id);
                chrome.first.lock().unwrap().push(id);
            }
        }
    }
    if !matches!(owner, Owner::Pictures) {
        watch(Arc::downgrade(&chrome), slot.clone(), matches!(owner, Owner::User(_)));
    }
    *held = Some(chrome.clone());
    Ok(chrome)
}

/// A Chrome at a DevTools port: the task's, given to its agent, or any.
fn over_port(mut command: std::process::Command, program: &Path, profile: PathBuf, port: u16, temporary: bool) -> Result<Arc<Chrome>> {
    let port_file = profile.join("DevToolsActivePort");
    let _ = std::fs::remove_file(&port_file);
    // The task's port when it is free; else any, and its terminals' is wrong.
    let port = if port != 0 && TcpListener::bind(("127.0.0.1", port)).is_ok() { port } else { 0 };
    command
        .arg(format!("--remote-debugging-port={port}"))
        // Its own DevTools, served from that port, connects back to it.
        .args((port != 0).then(|| format!("--remote-allow-origins=http://127.0.0.1:{port}")))
        .arg("about:blank");
    let mut child = crate::procs::spawn_tied(&mut command).with_context(|| format!("starting {}", program.display()))?;

    // Chrome writes the port it took and the browser socket path here.
    let started = Instant::now();
    let (port, ws_path) = loop {
        // A port given is not written to the file: Chrome tells it over HTTP.
        if port != 0 {
            if let Some(path) = devtools_version(port).and_then(|v| browser_path(&v)) {
                break (port, path);
            }
        } else if let Ok(text) = std::fs::read_to_string(&port_file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port.trim().parse() {
                    break (port, path.trim().to_string());
                }
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(anyhow!("Chrome ended as it started ({status}): is another one using {}?", profile.display()));
        }
        if started.elapsed() > Duration::from_secs(15) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!("Chrome did not open its DevTools port"));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let product = devtools_version(port).and_then(|v| v["Browser"].as_str().map(str::to_string)).unwrap_or_default();
    let major = major_of_product(&product);
    Ok(Arc::new(Chrome { port, pipe: None, ws_path, child: Mutex::new(child), profile, temporary, claimed: Default::default(), major, first: Default::default() }))
}

/// The user's Chrome: over a pipe only the app holds, with no port at all.
#[cfg(any(unix, windows))]
fn over_pipe(mut command: std::process::Command, program: &Path, profile: PathBuf) -> Result<Arc<Chrome>> {
    let (ours, chromes) = pipe::make(&mut command)?;
    command.arg("--remote-debugging-pipe").arg("about:blank");
    let mut child = crate::procs::spawn_tied(&mut command).with_context(|| format!("starting {}", program.display()))?;
    // Closed here, or Chrome's end would not tell when it is gone.
    drop(chromes);
    let pipe = pipe::Pipe::start(ours.0, ours.1);
    let version = (|| {
        let channel = pipe.channel()?;
        let id = channel.send(&mut json!({ "method": "Browser.getVersion", "params": {} }))?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(15) {
            if let Some(msg) = channel.recv(Some(Duration::from_millis(200)))? {
                if msg["id"].as_u64() == Some(id) {
                    return Ok(msg["result"]["product"].as_str().unwrap_or_default().to_string());
                }
            }
        }
        Err(anyhow!("Chrome did not answer over its pipe"))
    })();
    let product = match version {
        Ok(product) => product,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.context(format!("is another Chrome using {}?", profile.display())));
        }
    };
    let major = major_of_product(&product);
    Ok(Arc::new(Chrome { port: 0, pipe: Some(pipe), ws_path: String::new(), child: Mutex::new(child), profile, temporary: false, claimed: Default::default(), major, first: Default::default() }))
}

#[cfg(not(any(unix, windows)))]
fn over_pipe(_: std::process::Command, _: &Path, _: PathBuf) -> Result<Arc<Chrome>> {
    Err(anyhow!("ideae's own browser is not on this system"))
}

/// What a Chrome's cookies are encrypted with. A task's: a key known to
/// all, its logins are a dev stand's. The user's: the system's keyring
/// (Secret Service on Linux, falling back to that same key where there is
/// none; DPAPI on Windows), but never in tests or a run with a state of its
/// own, which must not touch the user's keyring. macOS keeps the mock
/// keychain until headless Chrome is seen to reach the real one without a
/// dialog that would hang it.
fn cookie_key(owner: &Owner) -> Vec<&'static str> {
    let own_state = cfg!(test) || std::env::var_os("IDEAE_STATE").is_some();
    match owner {
        Owner::User(_) if !own_state && cfg!(target_os = "linux") => vec!["--password-store=gnome-libsecret"],
        Owner::User(_) if !own_state && cfg!(windows) => vec![],
        _ => vec!["--password-store=basic", "--use-mock-keychain"],
    }
}

/// Whether the logins of the user's browser are kept unencrypted here, to say so.
fn user_logins_plain() -> bool {
    cookie_key(&Owner::User(UserBrowser::Shared)).contains(&"--password-store=basic")
        || (cfg!(target_os = "linux") && std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none())
}

/// `HeadlessChrome/158.0.8127.36` → `158`.
fn major_of_product(product: &str) -> String {
    product.split_once('/').map(|(_, v)| v.split('.').next().unwrap_or_default().to_string()).unwrap_or_else(|| "130".into())
}

/// What makes a headless Chrome the Chrome it is to a site: without it a
/// site sees `HeadlessChrome` and `navigator.webdriver`, and Google's
/// sign-in, among others, says the browser "may not be secure". The
/// graphics card too, where there is one, for pages that draw with WebGL.
fn as_ordinary(program: &Path) -> Vec<String> {
    let mut args = vec!["--disable-blink-features=AutomationControlled".to_string(), "--enable-gpu".to_string()];
    if let Some(major) = installed_major(program) {
        let edge = program.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("edge"));
        args.push(format!("--user-agent={}", ordinary_agent(&major, edge)));
    }
    args
}

/// The user agent the same Chrome has with a window: its major version
/// only and a fixed platform, as Chrome's own reduced one is.
fn ordinary_agent(major: &str, edge: bool) -> String {
    let platform = if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10_15_7"
    } else if cfg!(windows) {
        "Windows NT 10.0; Win64; x64"
    } else {
        "X11; Linux x86_64"
    };
    let edge = if edge { format!(" Edg/{major}.0.0.0") } else { String::new() };
    format!("Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36{edge}")
}

/// The major version of the Chrome at `program`, asked once for each build
/// of it: an update in place is a new one.
fn installed_major(program: &Path) -> Option<String> {
    static KNOWN: OnceLock<Mutex<HashMap<PathBuf, (Option<std::time::SystemTime>, Option<String>)>>> = OnceLock::new();
    let built = std::fs::metadata(program).and_then(|m| m.modified()).ok();
    let known = KNOWN.get_or_init(Default::default);
    if let Some((at, major)) = known.lock().unwrap().get(program) {
        if *at == built {
            return major.clone();
        }
    }
    let major = ask_major(program);
    known.lock().unwrap().insert(program.to_path_buf(), (built, major.clone()));
    major
}

/// `Google Chrome 158.0.8127.36`, `Chromium 158.0…`: its first number.
fn major_in(text: &str) -> Option<String> {
    text.split_whitespace().find(|w| w.contains('.') && w.starts_with(|c: char| c.is_ascii_digit()))?.split('.').next().map(str::to_string)
}

#[cfg(not(windows))]
fn ask_major(program: &Path) -> Option<String> {
    let out = crate::procs::command(program).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    major_in(&String::from_utf8_lossy(&out.stdout))
}

/// `chrome.exe --version` opens a window instead of answering: the folder
/// beside it is named after its version (`Application\158.0.8127.36`).
#[cfg(windows)]
fn ask_major(program: &Path) -> Option<String> {
    std::fs::read_dir(program.parent()?)
        .ok()?
        .flatten()
        .filter_map(|e| major_in(&e.file_name().to_string_lossy()))
        .filter_map(|m| m.parse::<u32>().ok())
        .max()
        .map(|m| m.to_string())
}

/// The browser's socket path, from what its DevTools endpoint says of it.
fn browser_path(version: &Value) -> Option<String> {
    let url = version["webSocketDebuggerUrl"].as_str()?;
    url.split_once("://")?.1.split_once('/').map(|(_, path)| format!("/{path}"))
}

/// What the DevTools HTTP endpoint at `port` says of the browser.
fn devtools_version(port: u16) -> Option<Value> {
    use std::io::{Read as _, Write as _};
    let mut stream = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream.write_all(b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").ok()?;
    // Read until the body is whole: Chrome keeps the connection open.
    let (mut answer, mut chunk) = (Vec::new(), [0u8; 4096]);
    let body: Value = loop {
        let n = stream.read(&mut chunk).ok().filter(|n| *n > 0)?;
        answer.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&answer);
        if let Some(body) = text.split_once("\r\n\r\n").and_then(|(_, body)| serde_json::from_str(body).ok()) {
            break body;
        }
    };
    Some(body)
}

/// While a task's Chrome is ended, its port waits for a call (an agent's
/// Playwright, given IDEAE_BROWSER_CDP): one starts it again, and the call
/// is carried to it.
fn wake_on_call(root: PathBuf, slot: Arc<Slot>) {
    let Ok(listener) = TcpListener::bind(("127.0.0.1", task_port(&root))) else { return };
    let stop = Arc::new(AtomicBool::new(false));
    *slot.waker.lock().unwrap() = Some(stop.clone());
    let _ = std::thread::Builder::new().name("cdp-waker".into()).spawn(move || {
        let Ok((client, _)) = listener.accept() else { return };
        drop(listener);
        if stop.load(Ordering::SeqCst) {
            return;
        }
        slot.waker.lock().unwrap().take();
        let owner = Owner::Task(root);
        // Counted and let go: Chrome ends a minute on unless a tab takes a page of it.
        let open = OpenPage::new(&owner);
        let Ok(chrome) = chrome(&owner) else { return };
        drop(open);
        let Ok(server) = TcpStream::connect(("127.0.0.1", chrome.port)) else { return };
        let (Ok(client_back), Ok(server_back)) = (client.try_clone(), server.try_clone()) else { return };
        let carry = |mut from: TcpStream, mut to: TcpStream| {
            let _ = std::io::copy(&mut from, &mut to);
            let _ = to.shutdown(std::net::Shutdown::Write);
        };
        let back = std::thread::spawn(move || carry(server_back, client_back));
        carry(client, server);
        let _ = back.join();
        drop(chrome);
    });
}

/// Ends every Chrome as the app quits: nothing else does where a child is
/// not told of its parent's end (macOS), and its idle time is a minute.
pub fn quit() {
    let Some(slots) = SLOTS.get() else { return };
    let slots: Vec<Arc<Slot>> = slots.lock().map(|s| s.values().cloned().collect()).unwrap_or_default();
    // Each closes on its own thread, whoever else holds it (a page's thread):
    // they write their profiles out at once.
    let closing: Vec<_> = slots
        .into_iter()
        .filter_map(|slot| slot.chrome.lock().ok().and_then(|mut c| c.take()))
        .map(|chrome| std::thread::spawn(move || chrome.close()))
        .collect();
    for closed in closing {
        let _ = closed.join();
    }
}

/// What a user's Chrome has for a task: the pages its tabs' pages opened, their downloads.
pub fn user_page_events(browser: &UserBrowser, root: &Path) -> async_channel::Receiver<PageEvent> {
    let (tx, rx) = async_channel::unbounded();
    slot(&Owner::User(browser.clone())).tasks.lock().unwrap().insert(root.to_path_buf(), tx);
    rx
}

/// A page of a user's Chrome shown in a task's tab: what it opens and
/// downloads goes to that task.
fn note_task_page(owner: &Owner, target: &str, root: &Path) {
    if matches!(owner, Owner::User(_)) {
        slot(owner).page_tasks.lock().unwrap().insert(target.to_string(), root.to_path_buf());
    }
}

/// A press that opens a link in a tab or a window of its own: the middle
/// button, or the left with Ctrl, Cmd or Shift. Chrome tells no opener of
/// the page it opens.
fn opens_a_tab(params: &Value) -> bool {
    let modifiers = params["modifiers"].as_u64().unwrap_or(0);
    params["type"] == "mousePressed" && (params["button"] == "middle" || (params["button"] == "left" && modifiers & (2 | 4 | 8) != 0))
}

/// What the task's Chrome has for the task: pages no tab opened, downloads.
pub fn page_events(root: &Path) -> async_channel::Receiver<PageEvent> {
    let (tx, rx) = async_channel::unbounded();
    *slot(&Owner::Task(root.to_path_buf())).adopt.lock().unwrap() = Some(tx);
    rx
}

fn ws_config() -> WebSocketConfig {
    // A picture of a large page at twice its size is tens of megabytes of text.
    WebSocketConfig::default().max_message_size(Some(512 << 20)).max_frame_size(Some(512 << 20))
}

/// A browser-level DevTools connection: a WebSocket of its own to a port,
/// or a channel of the user's Chrome's pipe.
fn connect(chrome: &Chrome) -> Result<Socket> {
    if let Some(pipe) = &chrome.pipe {
        return Ok(Socket { wire: Wire::Pipe { channel: pipe.channel()?, timeout: None }, session: None });
    }
    let stream = TcpStream::connect(("127.0.0.1", chrome.port))?;
    stream.set_nodelay(true)?;
    let url = format!("ws://127.0.0.1:{}{}", chrome.port, chrome.ws_path);
    let (ws, _) =
        tungstenite::client::client_with_config(url.as_str(), stream, Some(ws_config())).map_err(|e| anyhow!("Unable to connect to {url}: {e}"))?;
    Ok(Socket { wire: Wire::Ws { ws, next_id: 0 }, session: None })
}

/// Offers the task the pages nobody claims: one is claimed by its tab
/// within moments of being made, so a page is offered once that long
/// unclaimed. A user's Chrome serves many tasks: a page goes to the task
/// whose tab's page opened it, a download to the task whose page it came
/// from; one of no task's (none should be: no agent reaches that Chrome)
/// is closed.
fn watch(chrome: Weak<Chrome>, slot: Arc<Slot>, user: bool) {
    let _ = std::thread::Builder::new().name("cdp-targets".into()).spawn(move || {
        let Some(mut socket) = chrome.upgrade().and_then(|c| connect(&c).ok()) else {
            return;
        };
        let mut aside = Vec::new();
        // What is open already is Chrome's own first page: no tab's. Closed
        // once a tab has its page, so an agent's Playwright, taking the first
        // page it finds, drives one you see; shown as a tab when no tab came,
        // as when the agent's call started Chrome.
        let Some(mut first) = chrome.upgrade().map(|c| std::mem::take(&mut *c.first.lock().unwrap())) else { return };
        let started = Instant::now();
        if socket.call("Target.setDiscoverTargets", json!({ "discover": true }), &mut aside).is_err() {
            return;
        }
        // Downloads go to the Downloads folder under a name of their own, and
        // are named as the site asked once done: a second of a name overwrites nothing.
        let folder = downloads_dir();
        let _ = socket.call(
            "Browser.setDownloadBehavior",
            json!({ "behavior": "allowAndName", "downloadPath": folder.display().to_string(), "eventsEnabled": true }),
            &mut aside,
        );
        // Where what a page did goes: the task's, or the task the page is of.
        let task_of = |page: Option<&str>| page.and_then(|p| slot.page_tasks.lock().unwrap().get(p).cloned());
        let to = |task: Option<&PathBuf>| -> Option<async_channel::Sender<PageEvent>> {
            if !user {
                return slot.adopt.lock().unwrap().clone();
            }
            let tasks = slot.tasks.lock().unwrap();
            task.and_then(|t| tasks.get(t).cloned())
        };
        socket.set_timeout(Some(Duration::from_millis(200)));
        // A page, its opener, and whether it is to be shown in front (opened by a script, not a click).
        type Waiting = Vec<(String, Option<String>, bool, Instant)>;
        let mut waiting: Waiting = Vec::new();
        // A download's name and the page it came from, by its id.
        let mut downloads: HashMap<String, (String, Option<String>)> = HashMap::new();
        let mut handle = |msg: Value, waiting: &mut Waiting| {
            let params = &msg["params"];
            match msg["method"].as_str() {
                Some("Target.targetCreated") if params["targetInfo"]["type"] == "page" => {
                    let info = &params["targetInfo"];
                    let id = info["targetId"].as_str().unwrap_or_default().to_string();
                    let opener = info["openerId"].as_str().map(str::to_string);
                    let front = opener.is_some();
                    let clicked = || slot.clicked.lock().unwrap().take_if(|(_, at)| at.elapsed() < Duration::from_secs(3)).map(|(page, _)| page);
                    let opener = opener.or_else(|| (info["url"] != "about:blank").then(clicked).flatten());
                    waiting.push((id, opener, front, Instant::now()));
                }
                Some("Target.targetDestroyed") => {
                    let id = params["targetId"].as_str().unwrap_or_default();
                    waiting.retain(|(w, ..)| w != id);
                    slot.page_tasks.lock().unwrap().remove(id);
                }
                Some("Browser.downloadWillBegin") => {
                    let name = params["suggestedFilename"].as_str().unwrap_or("download").to_string();
                    // A page's main frame has the page's id.
                    downloads.insert(params["guid"].as_str().unwrap_or_default().to_string(), (name, params["frameId"].as_str().map(str::to_string)));
                }
                Some("Browser.downloadProgress") => {
                    let guid = params["guid"].as_str().unwrap_or_default();
                    let done = params["state"].as_str();
                    if !matches!(done, Some("completed" | "canceled")) {
                        return;
                    }
                    let (name, frame) = downloads.remove(guid).unwrap_or_else(|| ("download".into(), None));
                    let task = task_of(frame.as_deref()).or_else(|| slot.tasks.lock().unwrap().keys().next().cloned());
                    let event = if done == Some("completed") {
                        let to = free_name(&folder, &name);
                        let from = params["filePath"].as_str().map(PathBuf::from).unwrap_or_else(|| folder.join(guid));
                        match std::fs::rename(&from, &to) {
                            Ok(()) => PageEvent::Downloaded(to),
                            Err(_) => PageEvent::Downloaded(from),
                        }
                    } else {
                        PageEvent::DownloadFailed(name)
                    };
                    if let Some(tx) = to(task.as_ref()) {
                        let _ = tx.try_send(event);
                    }
                }
                _ => {}
            }
        };
        for msg in aside.drain(..) {
            handle(msg, &mut waiting);
        }
        loop {
            match socket.read() {
                Ok(Some(msg)) => handle(msg, &mut waiting),
                Ok(None) => {}
                Err(_) => return,
            }
            let Some(chrome) = chrome.upgrade() else {
                return;
            };
            if !first.is_empty() {
                let tabs = chrome.claimed.lock().unwrap().len() > first.len();
                if tabs || user {
                    if tabs {
                        for target in first.drain(..) {
                            let _ = socket.send("Target.closeTarget", json!({ "targetId": target }));
                        }
                    }
                } else if started.elapsed() > Duration::from_secs(3) {
                    for target in first.drain(..) {
                        chrome.claimed.lock().unwrap().remove(&target);
                        if let Some(tx) = slot.adopt.lock().unwrap().as_ref() {
                            let _ = tx.try_send(PageEvent::Opened { target, by_page: false });
                        }
                    }
                }
            }
            let due = Duration::from_millis(500);
            while let Some(ix) = waiting.iter().position(|(.., at)| at.elapsed() > due) {
                let (target, opener, front, _) = waiting.remove(ix);
                if chrome.claimed.lock().unwrap().contains(&target) {
                    continue;
                }
                let task = task_of(opener.as_deref());
                match to(task.as_ref()) {
                    Some(tx) => {
                        if let Some(task) = task {
                            slot.page_tasks.lock().unwrap().insert(target.clone(), task);
                        }
                        let _ = tx.try_send(PageEvent::Opened { target, by_page: front });
                    }
                    None if user => {
                        let _ = socket.send("Target.closeTarget", json!({ "targetId": target }));
                    }
                    None => {}
                }
            }
        }
    });
}

/// The user's Downloads folder.
fn downloads_dir() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    // XDG's own name for it, where a desktop set one.
    let named = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).ok().and_then(|text| {
        let line = text.lines().find(|l| l.starts_with("XDG_DOWNLOAD_DIR="))?;
        let value = line.split_once('=')?.1.trim().trim_matches('"');
        Some(PathBuf::from(value.replace("$HOME", &home.display().to_string())))
    });
    let dir = named.unwrap_or_else(|| home.join("Downloads"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// `name` in `folder`, or `name (2)`… where that is taken.
fn free_name(folder: &Path, name: &str) -> PathBuf {
    let name = Path::new(name).file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "download".into());
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.clone(), String::new()),
    };
    (1..).map(|n| folder.join(if n == 1 { name.clone() } else { format!("{stem} ({n}){ext}") })).find(|p| !p.exists()).expect("a free name")
}

// ---------------------------------------------------------------- page session

/// What a tab shows: a page to open, or one open already; or DevTools for
/// a page of a Chrome with no port to serve them (the user's).
pub enum Source {
    Url(String),
    Target(String),
    DevTools(String),
}

/// Chrome's own DevTools, loaded from inside Chrome. Its WebSocket is made
/// a stand-in: what it sends goes out through a binding, and ideae carries
/// it to the page inspected over the pipe and brings back what it answers.
const DEVTOOLS_BINDING: &str = "__ideaeDevtoolsSend";
const DEVTOOLS_SHIM: &str = r##"(() => {
  if (location.protocol !== "devtools:") return;
  const socket = { readyState: 0, onopen: null, onmessage: null, onclose: null, onerror: null,
    send: (m) => window.__ideaeDevtoolsSend(m), close() {},
    addEventListener(type, f) { this["on" + type] = f; }, removeEventListener() {} };
  window.__ideaeDevtools = { deliver(list) { for (const m of list) if (socket.onmessage) socket.onmessage({ data: JSON.stringify(m) }); } };
  const Shim = function () { setTimeout(() => { socket.readyState = 1; if (socket.onopen) socket.onopen({}); }, 0); return socket; };
  Shim.CONNECTING = 0; Shim.OPEN = 1; Shim.CLOSING = 2; Shim.CLOSED = 3;
  window.WebSocket = Shim;
})()"##;

/// What DevTools may not ask of the page's Chrome: its other pages.
const DEVTOOLS_DENIED: [&str; 8] = [
    "Target.attachToTarget",
    "Target.attachToBrowserTarget",
    "Target.createTarget",
    "Target.closeTarget",
    "Target.exposeDevToolsProtocol",
    "Target.createBrowserContext",
    "Target.disposeBrowserContext",
    "Target.setRemoteLocations",
];

/// The messages between DevTools (a page of the same Chrome) and the page it
/// inspects, through a session of its own: its replies by the id DevTools
/// gave, its events and those of the frames and workers under it.
struct DevToolsBridge {
    page: String,
    inspected: String,
    children: HashSet<String>,
    outbox: Vec<Value>,
}

impl DevToolsBridge {
    /// Takes the bridge's messages; gives back the page's own.
    fn take(&mut self, mut msg: Value, socket: &mut Socket, replies: &Replies, ask: &mpsc::Sender<Out>) -> Option<Value> {
        if let Some(id) = msg["id"].as_u64() {
            let mine = matches!(replies.lock().unwrap().get(&id), Some(Reply::DevTools(_)));
            if !mine {
                return Some(msg);
            }
            if let Some(Reply::DevTools(front)) = replies.lock().unwrap().remove(&id) {
                msg["id"] = json!(front);
                if msg["sessionId"] == self.inspected.as_str() {
                    msg.as_object_mut().map(|m| m.remove("sessionId"));
                }
                self.outbox.push(msg);
            }
            return None;
        }
        let session = msg["sessionId"].as_str().unwrap_or_default().to_string();
        if session == self.inspected || self.children.contains(&session) {
            if msg["method"] == "Target.attachedToTarget" {
                if let Some(child) = msg["params"]["sessionId"].as_str() {
                    self.children.insert(child.to_string());
                    socket.listen(child);
                }
            }
            if session == self.inspected {
                msg.as_object_mut().map(|m| m.remove("sessionId"));
            }
            self.outbox.push(msg);
            return None;
        }
        if session == self.page && msg["method"] == "Runtime.bindingCalled" && msg["params"]["name"] == DEVTOOLS_BINDING {
            let Ok(mut asked) = serde_json::from_str::<Value>(msg["params"]["payload"].as_str().unwrap_or_default()) else { return None };
            let front = asked["id"].as_u64().unwrap_or_default();
            let to = asked["sessionId"].as_str().map(str::to_string);
            let method = asked["method"].as_str().unwrap_or_default();
            let allowed = to.as_ref().is_none_or(|s| self.children.contains(s)) && !DEVTOOLS_DENIED.contains(&method);
            if !allowed {
                self.outbox.push(json!({ "id": front, "error": { "code": -32601, "message": format!("{method} is not given to DevTools in ideae") } }));
                return None;
            }
            if to.is_none() {
                asked["sessionId"] = json!(self.inspected);
            }
            let _ = ask.send(Out::Raw(asked, Reply::DevTools(front)));
            return None;
        }
        Some(msg)
    }

    /// What came for DevTools since, given to it at once.
    fn flush(&mut self, ask: &mpsc::Sender<Out>) {
        if self.outbox.is_empty() {
            return;
        }
        let list = Value::Array(std::mem::take(&mut self.outbox));
        let _ = ask.send(Out::Command("Runtime.evaluate", json!({ "expression": format!("window.__ideaeDevtools && window.__ideaeDevtools.deliver({list})") })));
    }
}

enum Out {
    Command(&'static str, Value),
    /// The page's title, asked once it loaded: what a new document tells
    /// as it starts can be lost while Chrome moves it to a process of its own.
    AskTitle,
    /// A command whose result is waited for.
    Call(&'static str, Value, mpsc::Sender<Value>),
    /// The pointer moved: the latest of several waiting is enough.
    Move(Value),
    /// The page's viewport in CSS pixels, device pixels to each, whether
    /// it is a phone's or a tablet's, and the user agent it says it is
    /// (Chrome's own when none), with the page's address to load again
    /// when that changes.
    Resize(u32, u32, f32, bool, Option<Value>, String),
    /// A frame is drawn: Chrome sends the next only then.
    Ack(i64),
    /// The tab came into view or left it.
    Visible(bool),
    /// A message as DevTools made it, for the session it names; its reply goes back to it.
    Raw(Value, Reply),
    Close,
}

#[derive(Clone)]
struct Dialog {
    kind: String,
    message: String,
}

/// A date, time or color field's picker asked for: headless Chrome draws
/// none, so the tab asks for the value.
#[derive(Clone)]
struct Picker {
    kind: String,
    value: String,
    bounds: Bounds<Pixels>,
    context: i64,
}

#[derive(Clone)]
struct Choice {
    label: String,
    disabled: bool,
    group: Option<String>,
}

/// A `<select>` clicked: headless Chrome draws its list nowhere, so the
/// tab does.
#[derive(Clone)]
struct SelectMenu {
    bounds: Bounds<Pixels>,
    choices: Vec<Choice>,
    selected: Option<usize>,
    /// The frame the select is in, to answer there.
    context: i64,
}

enum In {
    /// The DevTools port, the page's id there and Chrome's major version, once connected.
    Ready { port: u16, target: String, major: String },
    /// Where text is put in the page, for an input method's window.
    Caret(Bounds<Pixels>),
    Frame(Vec<u8>, i64),
    /// A new document; `failed` when it is Chrome's error page for it.
    Navigated {
        url: String,
        failed: bool,
    },
    Url(String),
    Title(String),
    Loading(bool),
    Dialog(Dialog),
    DialogClosed,
    FileChooser {
        node: i64,
        multiple: bool,
    },
    Cursor(String),
    Select(SelectMenu),
    Picker(Picker),
    Problem(String),
    /// A page of the user's browser asked for a passkey or a security key, and was refused.
    Passkey,
    Closed,
    Error(String),
}

/// Who waits for the reply to a command.
enum Reply {
    Caller(mpsc::Sender<Value>),
    Title,
    /// DevTools', by the id it gave.
    DevTools(u64),
}

type Replies = Mutex<HashMap<u64, Reply>>;

enum Wire {
    Ws { ws: WebSocket<TcpStream>, next_id: u64 },
    Pipe { channel: pipe::Channel, timeout: Option<Duration> },
}

struct Socket {
    wire: Wire,
    session: Option<String>,
}

impl Socket {
    fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        let mut msg = json!({ "method": method, "params": params });
        if let Some(session) = &self.session {
            msg["sessionId"] = json!(session);
        }
        match &mut self.wire {
            Wire::Ws { ws, next_id } => {
                *next_id += 1;
                msg["id"] = json!(*next_id);
                ws.send(Message::text(msg.to_string()))?;
                Ok(*next_id)
            }
            Wire::Pipe { channel, .. } => channel.send(&mut msg),
        }
    }

    /// Sends and waits for the reply, setting other messages aside.
    fn call(&mut self, method: &str, params: Value, aside: &mut Vec<Value>) -> Result<Value> {
        let id = self.send(method, params)?;
        loop {
            let Some(mut msg) = self.read()? else {
                continue;
            };
            if msg["id"].as_u64() == Some(id) {
                if let Some(error) = msg.get("error") {
                    return Err(anyhow!("{method}: {}", error["message"]));
                }
                // Taken, not copied: a screenshot is megabytes of text.
                return Ok(msg["result"].take());
            }
            aside.push(msg);
        }
    }

    fn read(&mut self) -> Result<Option<Value>> {
        match &mut self.wire {
            Wire::Ws { ws, .. } => match ws.read() {
                Ok(Message::Text(text)) => Ok(serde_json::from_str(text.as_str()).ok()),
                Ok(_) => Ok(None),
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
                Err(e) => Err(e.into()),
            },
            Wire::Pipe { channel, timeout } => channel.recv(*timeout),
        }
    }

    /// How long a read waits; none: until a message comes.
    fn set_timeout(&mut self, timeout: Option<Duration>) {
        match &mut self.wire {
            Wire::Ws { ws, .. } => {
                let _ = ws.get_mut().set_read_timeout(timeout);
            }
            Wire::Pipe { timeout: t, .. } => *t = timeout,
        }
    }

    /// Commands go to this page's session from now on, and over a pipe its
    /// events come here.
    fn set_session(&mut self, session: Option<String>) {
        if let (Wire::Pipe { channel, .. }, Some(session)) = (&mut self.wire, &session) {
            channel.listen(session);
        }
        self.session = session;
    }

    /// The events of another session come here too (over a WebSocket they come all the same).
    fn listen(&mut self, session: &str) {
        if let Wire::Pipe { channel, .. } = &mut self.wire {
            channel.listen(session);
        }
    }

    /// Writes for the page from another thread; what Chrome answers comes to
    /// this socket's reads.
    fn writer(&self) -> Result<Writer> {
        Ok(match &self.wire {
            Wire::Ws { ws, next_id } => {
                let raw = ws.get_ref().try_clone()?;
                Writer::Ws { ws: WebSocket::from_raw_socket(raw.try_clone()?, Role::Client, Some(ws_config())), raw, next_id: *next_id + 1 }
            }
            Wire::Pipe { channel, .. } => Writer::Pipe(channel.writer()),
        })
    }
}

enum Writer {
    Ws { ws: WebSocket<TcpStream>, raw: TcpStream, next_id: u64 },
    Pipe(pipe::Writer),
}

impl Writer {
    fn next_id(&mut self) -> u64 {
        match self {
            Writer::Ws { next_id, .. } => {
                *next_id += 1;
                *next_id
            }
            Writer::Pipe(writer) => writer.next_id(),
        }
    }

    fn write(&mut self, msg: &Value) -> bool {
        match self {
            Writer::Ws { ws, .. } => ws.send(Message::text(msg.to_string())).is_ok(),
            Writer::Pipe(writer) => writer.write(msg),
        }
    }

    /// Ends the reader's wait: the page is closed.
    fn end(&mut self) {
        match self {
            Writer::Ws { raw, .. } => {
                let _ = raw.shutdown(std::net::Shutdown::Both);
            }
            Writer::Pipe(writer) => writer.end(),
        }
    }
}

/// A tab's page counted among its owner's, for Chrome to end a while after
/// the last one closes: it is a browser's worth of processes kept for nothing.
struct OpenPage(Arc<Slot>, Owner);
const CHROME_IDLE: Duration = if cfg!(test) { Duration::from_millis(500) } else { Duration::from_secs(60) };
/// The user's: a link clicked a while after the last tab closed should not
/// wait for a Chrome to start.
const USER_CHROME_IDLE: Duration = if cfg!(test) { Duration::from_millis(500) } else { Duration::from_secs(300) };

impl OpenPage {
    fn new(owner: &Owner) -> Self {
        let slot = slot(owner);
        slot.pages.fetch_add(1, Ordering::SeqCst);
        Self(slot, owner.clone())
    }
}

impl Drop for OpenPage {
    fn drop(&mut self) {
        let slot = self.0.clone();
        if slot.pages.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        let this = slot.closed.fetch_add(1, Ordering::SeqCst) + 1;
        let owner = self.1.clone();
        std::thread::spawn(move || {
            std::thread::sleep(if matches!(owner, Owner::User(_)) { USER_CHROME_IDLE } else { CHROME_IDLE });
            let mut held = slot.chrome.lock().unwrap();
            // Under the lock a new page waits for: it finds Chrome there, or starts one.
            if slot.pages.load(Ordering::SeqCst) == 0 && slot.closed.load(Ordering::SeqCst) == this {
                if let Some(chrome) = held.take() {
                    // Ended now, so its port is free for the waker.
                    drop(chrome);
                    if let Owner::Task(root) = owner {
                        let started = Instant::now();
                        while TcpListener::bind(("127.0.0.1", task_port(&root))).is_err() && started.elapsed() < Duration::from_secs(2) {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        wake_on_call(root, slot.clone());
                    }
                }
            }
        });
    }
}

/// One page: this thread reads what Chrome sends, one of its own writes.
fn spawn_page(owner: Owner, source: Source, in_tx: async_channel::Sender<In>) -> mpsc::Sender<Out> {
    let (out_tx, out_rx) = mpsc::channel::<Out>();
    std::thread::Builder::new()
        .name("cdp-page".into())
        .spawn({
            // The reader's own questions, through the writer.
            let ask = out_tx.clone();
            move || {
                if let Err(e) = run_page(owner, source, &in_tx, out_rx, ask) {
                    let _ = in_tx.send_blocking(In::Error(format!("{e:#}")));
                }
            }
        })
        .expect("spawn cdp thread");
    out_tx
}

/// Run in every document of the page: tells the tab its title and the
/// pointer's cursor as they change, and takes what would open a list or a
/// picker headless Chrome draws nowhere: a `<select>`'s, a date's, a color's.
const PAGE_SCRIPT: &str = r##"(() => {
  if (window.__ideaeOn) return;
  window.__ideaeOn = true;
  // Chrome's DevTools in its panel: the page is in the tab beside it, not
  // in a picture of its own, which would set the page's size and scale.
  if (location.hostname === "127.0.0.1" && location.pathname === "/devtools/inspector.html") {
    try { for (const key of ["screencast-enabled", "screencastEnabled"]) localStorage.setItem(key, "false"); } catch (_) {}
  }
  const send = (m) => { try { window.__ideae(JSON.stringify(m)); } catch (_) {} };
  const typing = (e) => e.isContentEditable || e.localName === "textarea" ||
    (e.localName === "input" && !/^(button|submit|reset|checkbox|radio|range|color|file|image)$/.test(e.type));
  const cursorAt = (el, x, y) => {
    for (let n = el; n && n.nodeType === 1; n = n.parentElement || n.getRootNode().host || null) {
      const c = getComputedStyle(n).cursor;
      if (c && c !== "auto") return c.split(",").pop().trim();
      if (typing(n)) return "text";
      if ((n.localName === "a" && n.hasAttribute("href")) || n.localName === "summary") return "pointer";
    }
    const at = document.caretRangeFromPoint && document.caretRangeFromPoint(x, y);
    if (at && at.startContainer.nodeType === 3) {
      const text = document.createRange();
      text.selectNodeContents(at.startContainer);
      for (const b of text.getClientRects()) if (x >= b.left && x <= b.right && y >= b.top && y <= b.bottom) return "text";
    }
    return "default";
  };
  if (window === window.top) {
    let title = null;
    const tell = () => { if (document.title !== title) { title = document.title; send({ title }); } };
    const watch = () => {
      tell();
      if (document.head) new MutationObserver(tell).observe(document.head, { subtree: true, childList: true, characterData: true });
    };
    if (document.readyState === "loading") addEventListener("DOMContentLoaded", watch, { once: true }); else watch();
  }
  let last = "", moved = null;
  addEventListener("mousemove", (ev) => {
    const first = !moved;
    // Its path is empty once the event is over: taken now.
    moved = { target: ev.composedPath()[0], x: ev.clientX, y: ev.clientY };
    if (!first) return;
    setTimeout(() => {
      const m = moved;
      moved = null;
      let t = m.target;
      if (t && t.nodeType !== 1) t = t.parentElement;
      if (!t) return;
      const c = cursorAt(t, m.x, m.y);
      if (c !== last) { last = c; send({ cursor: c }); }
    }, 16);
  }, { capture: true, passive: true });
  // Where an element is in the top page, through same-origin frames.
  const place = (el) => {
    const r = el.getBoundingClientRect();
    let x = r.left, y = r.top;
    try {
      for (let w = window; w !== w.top && w.frameElement; w = w.parent) {
        const f = w.frameElement.getBoundingClientRect();
        x += f.left + w.frameElement.clientLeft;
        y += f.top + w.frameElement.clientTop;
      }
    } catch (_) {}
    return { x, y, width: r.width, height: r.height };
  };
  const openSelect = (s) => {
    if (s.multiple || s.size > 1 || s.disabled) return false;
    s.focus();
    window.__ideaeSelect = s;
    const options = [...s.options].map((o) => {
      const g = o.parentElement.localName === "optgroup" ? o.parentElement : null;
      return { label: o.label || o.text, disabled: o.disabled || !!(g && g.disabled), group: g ? g.label : null };
    });
    send({ select: Object.assign(place(s), { options, selected: s.selectedIndex }) });
    return true;
  };
  const PICKED = /^(date|time|datetime-local|month|week|color)$/;
  const openPicker = (i) => {
    if (i.disabled || i.readOnly) return false;
    i.focus();
    window.__ideaePicker = i;
    send({ picker: Object.assign(place(i), { type: i.type, value: i.value }) });
    return true;
  };
  const target = (ev) => { const t = ev.composedPath()[0]; return t && t.nodeType === 1 ? t : null; };
  // Where the caret is, for an input method's window, as it moves.
  const caretOf = () => {
    let e = document.activeElement;
    while (e && e.shadowRoot && e.shadowRoot.activeElement) e = e.shadowRoot.activeElement;
    if (!e || e === document.body) return null;
    let r = null;
    if ((e.localName === "input" || e.localName === "textarea") && typeof e.selectionStart === "number") {
      const b = e.getBoundingClientRect(), cs = getComputedStyle(e);
      let x = b.left + parseFloat(cs.paddingLeft) + parseFloat(cs.borderLeftWidth);
      if (e.localName === "input") {
        const c = document.createElement("canvas").getContext("2d");
        c.font = cs.font;
        x += c.measureText(e.value.slice(0, e.selectionStart)).width - e.scrollLeft;
      }
      r = { x: Math.min(x, b.right), y: b.top + parseFloat(cs.paddingTop) + parseFloat(cs.borderTopWidth), height: parseFloat(cs.lineHeight) || parseFloat(cs.fontSize) * 1.2 };
    } else if (e.isContentEditable) {
      const s = getSelection();
      const b = s.rangeCount ? s.getRangeAt(0).getBoundingClientRect() : null;
      if (b && b.height) r = { x: b.left, y: b.top, height: b.height };
    }
    if (!r) return null;
    try {
      for (let w = window; w !== w.top && w.frameElement; w = w.parent) {
        const f = w.frameElement.getBoundingClientRect();
        r.x += f.left;
        r.y += f.top;
      }
    } catch (_) {}
    return r;
  };
  let caretTold = "", caretTimer = 0;
  const tellCaret = () => {
    if (caretTimer) return;
    caretTimer = setTimeout(() => {
      caretTimer = 0;
      const r = caretOf();
      if (!r) return;
      const k = Math.round(r.x) + "," + Math.round(r.y) + "," + Math.round(r.height);
      if (k !== caretTold) { caretTold = k; send({ caret: r }); }
    }, 50);
  };
  document.addEventListener("selectionchange", tellCaret, true);
  addEventListener("focusin", tellCaret, true);
  addEventListener("mousedown", (ev) => {
    const t = target(ev);
    if (!t || ev.button !== 0) return;
    const s = t.closest("select");
    if (s && openSelect(s)) return ev.preventDefault();
    if (t.localName === "input" && PICKED.test(t.type)) {
      // A date's own fields take a click; its calendar mark, at the right end, opens the picker.
      const r = t.getBoundingClientRect();
      if ((t.type === "color" || ev.clientX > r.right - 28) && openPicker(t)) ev.preventDefault();
    }
  }, true);
  // The keys that open a list or a picker in Chrome.
  addEventListener("keydown", (ev) => {
    const t = target(ev);
    if (!t) return;
    const opens = ev.key === "F4" || (ev.altKey && (ev.key === "ArrowDown" || ev.key === "ArrowUp"));
    if (t.localName === "select" && (opens || ev.key === " ") && openSelect(t)) ev.preventDefault();
    else if (t.localName === "input" && PICKED.test(t.type) && (opens || (t.type === "color" && (ev.key === " " || ev.key === "Enter"))) && openPicker(t)) ev.preventDefault();
  }, true);
  // A page's own button for them.
  try {
    const own = HTMLInputElement.prototype.showPicker;
    HTMLInputElement.prototype.showPicker = function () { if (!(PICKED.test(this.type) && openPicker(this)) && own) own.call(this); };
    HTMLSelectElement.prototype.showPicker = function () { openSelect(this); };
  } catch (_) {}
})()"##;

const BINDING: &str = "__ideae";

/// In a page of the user's browser: a passkey or a security key asked for
/// is refused at once, not left waiting for a dialog headless Chrome never
/// shows, so the site offers its other ways in; the tab is told, to offer
/// the system's browser. Not in a task's: an agent's Playwright tests
/// passkeys there with virtual authenticators.
const PASSKEY_SCRIPT: &str = r##"(() => {
  const c = navigator.credentials;
  if (!c || !window.PublicKeyCredential || c.__ideae) return;
  c.__ideae = true;
  const refuse = (options) => {
    if (!options || !options.publicKey) return null;
    if (options.mediation !== "conditional") {
      try { window.__ideae(JSON.stringify({ passkey: true })); } catch (_) {}
    }
    return Promise.reject(new DOMException("No passkey or security key in this browser", "NotAllowedError"));
  };
  const get = c.get.bind(c), create = c.create.bind(c);
  c.get = (o) => refuse(o) || get(o);
  c.create = (o) => refuse(o) || create(o);
  PublicKeyCredential.isConditionalMediationAvailable = () => Promise.resolve(false);
  PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable = () => Promise.resolve(false);
})()"##;

/// The text selected, in a field too, where the page's selection has none;
/// in the frame or shadow root that has the focus.
const SELECTED: &str = r##"(() => {
  let d = document, e = d.activeElement;
  for (;;) {
    while (e && e.shadowRoot && e.shadowRoot.activeElement) e = e.shadowRoot.activeElement;
    if (e && e.localName === "iframe" && e.contentDocument) { d = e.contentDocument; e = d.activeElement; continue; }
    break;
  }
  try {
    if (e && typeof e.selectionStart === "number" && e.selectionEnd > e.selectionStart) return e.value.slice(e.selectionStart, e.selectionEnd);
  } catch (_) {}
  return String((d.defaultView || window).getSelection());
})()"##;

/// A message of the page about something wrong: an exception, an error
/// logged, a request that failed.
fn problem(method: &str, params: &Value) -> Option<String> {
    let place = |url: &Value, line: &Value| {
        let url = url.as_str().filter(|u| !u.is_empty())?;
        Some(match line.as_i64() {
            Some(line) => format!(" ({url}:{})", line + 1),
            None => format!(" ({url})"),
        })
    };
    let text = match method {
        "Runtime.exceptionThrown" => {
            let d = &params["exceptionDetails"];
            match d["exception"]["description"].as_str() {
                Some(description) => description.to_string(),
                None => format!("{}{}", d["text"].as_str().unwrap_or("Uncaught"), place(&d["url"], &d["lineNumber"]).unwrap_or_default()),
            }
        }
        "Runtime.consoleAPICalled" if matches!(params["type"].as_str(), Some("error" | "assert")) => {
            let args: Vec<String> = params["args"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| match &a["value"] {
                    Value::String(s) => s.clone(),
                    Value::Null => a["description"].as_str().unwrap_or("").to_string(),
                    other => other.to_string(),
                })
                .collect();
            let top = &params["stackTrace"]["callFrames"][0];
            format!("console.error: {}{}", args.join(" "), place(&top["url"], &top["lineNumber"]).unwrap_or_default())
        }
        // A missing favicon is Chrome's asking, not the page's.
        "Log.entryAdded" if params["entry"]["level"] == "error" && !params["entry"]["url"].as_str().is_some_and(|u| u.ends_with("/favicon.ico")) => {
            let entry = &params["entry"];
            format!("{}{}", entry["text"].as_str().unwrap_or_default(), place(&entry["url"], &Value::Null).unwrap_or_default())
        }
        _ => return None,
    };
    let mut text = text.trim().to_string();
    if text.len() > 2000 {
        let cut = (0..=2000).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
        text.truncate(cut);
        text.push('…');
    }
    Some(text)
}

fn run_page(owner: Owner, source: Source, in_tx: &async_channel::Sender<In>, out_rx: mpsc::Receiver<Out>, ask: mpsc::Sender<Out>) -> Result<()> {
    let open = OpenPage::new(&owner);
    let chrome = chrome(&owner)?;
    let mut socket = connect(&chrome)?;
    let mut aside = Vec::new();

    let target_id = match &source {
        Source::Url(_) | Source::DevTools(_) => {
            // A window of its own: Chrome draws only the tab in front of a window,
            // and a page behind another sends no frames.
            let target = socket.call("Target.createTarget", json!({ "url": "about:blank", "newWindow": true }), &mut aside)?;
            target["targetId"].as_str().unwrap_or_default().to_string()
        }
        Source::Target(id) => id.clone(),
    };
    chrome.claim(&target_id);
    let _ = in_tx.send_blocking(In::Ready { port: chrome.port, target: target_id.clone(), major: chrome.major.clone() });
    // The page's closing is told to the browser's connection.
    socket.call("Target.setDiscoverTargets", json!({ "discover": true }), &mut aside)?;
    let attached = socket.call("Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }), &mut aside)?;
    socket.set_session(attached["sessionId"].as_str().map(str::to_string));
    socket.call("Page.enable", json!({}), &mut aside)?;
    socket.call("Runtime.enable", json!({}), &mut aside)?;
    let _ = socket.call("Log.enable", json!({}), &mut aside);
    // A task's page: which hosts are its stands is told by where they are served from.
    let mut seen = matches!(owner, Owner::Task(_)).then(|| seed::Seen::load(&chrome.profile));
    if seen.is_some() {
        let _ = socket.call("Network.enable", json!({ "maxTotalBufferSize": 0, "maxResourceBufferSize": 0 }), &mut aside);
    }
    // Focused as the tab in view is, wherever its window is.
    let _ = socket.call("Emulation.setFocusEmulationEnabled", json!({ "enabled": true }), &mut aside);
    let _ = socket.call("Page.setInterceptFileChooserDialog", json!({ "enabled": true }), &mut aside);
    socket.call("Runtime.addBinding", json!({ "name": BINDING }), &mut aside)?;
    socket.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": PAGE_SCRIPT }), &mut aside)?;
    let _ = socket.call("Runtime.evaluate", json!({ "expression": PAGE_SCRIPT }), &mut aside);
    if matches!(owner, Owner::User(_)) {
        socket.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": PASSKEY_SCRIPT }), &mut aside)?;
        let _ = socket.call("Runtime.evaluate", json!({ "expression": PASSKEY_SCRIPT }), &mut aside);
    }
    let mut bridge = None;
    if let Source::DevTools(of) = &source {
        socket.call("Runtime.addBinding", json!({ "name": DEVTOOLS_BINDING }), &mut aside)?;
        socket.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": DEVTOOLS_SHIM }), &mut aside)?;
        // A session of its own on the page inspected: what DevTools turns on there is not the tab's.
        let page = socket.session.take().unwrap_or_default();
        let inspected = socket.call("Target.attachToTarget", json!({ "targetId": of, "flatten": true }), &mut aside);
        socket.session = Some(page.clone());
        let inspected = inspected?["sessionId"].as_str().unwrap_or_default().to_string();
        socket.listen(&inspected);
        bridge = Some(DevToolsBridge { page, inspected, children: HashSet::new(), outbox: Vec::new() });
        socket.send("Page.navigate", json!({ "url": format!("devtools://devtools/bundled/inspector.html?ws=ideae/page/{of}") }))?;
    } else if let Source::Url(url) = &source {
        socket.send("Page.navigate", json!({ "url": url }))?;
    } else {
        // A page made elsewhere: what it shows now.
        if let Ok(info) = socket.call("Target.getTargetInfo", json!({ "targetId": target_id }), &mut aside) {
            let info = &info["targetInfo"];
            let _ = in_tx.send_blocking(In::Navigated { url: info["url"].as_str().unwrap_or_default().to_string(), failed: false });
            let _ = in_tx.send_blocking(In::Title(info["title"].as_str().unwrap_or_default().to_string()));
        }
    }

    let replies: Arc<Replies> = Default::default();
    let closing = Arc::new(AtomicBool::new(false));
    {
        let writer = socket.writer()?;
        let (replies, closing, target_id, owner) = (replies.clone(), closing.clone(), target_id.clone(), owner.clone());
        let session = socket.session.clone().unwrap_or_default();
        std::thread::Builder::new().name("cdp-write".into()).spawn(move || {
            // Chrome stays while the page is written to.
            let _keep = (open, chrome);
            write_page(&owner, writer, session, &target_id, out_rx, &replies, &closing);
        })?;
    }

    let mut main_frame = target_id.clone();
    for msg in aside.drain(..) {
        if let Some(seen) = &mut seen {
            seen.note(&msg);
        }
        let Some(msg) = (match &mut bridge {
            Some(bridge) => bridge.take(msg, &mut socket, &replies, &ask),
            None => Some(msg),
        }) else {
            continue;
        };
        read_message(msg, &target_id, &mut main_frame, in_tx, &replies, &ask);
    }
    if let Some(bridge) = &mut bridge {
        bridge.flush(&ask);
    }
    // Blocking reads: the writer shuts the stream to end them.
    socket.set_timeout(None);
    loop {
        match socket.read() {
            Ok(Some(msg)) => {
                if let Some(seen) = &mut seen {
                    seen.note(&msg);
                }
                let msg = match &mut bridge {
                    Some(bridge) => match bridge.take(msg, &mut socket, &replies, &ask) {
                        Some(msg) => msg,
                        None => {
                            bridge.flush(&ask);
                            continue;
                        }
                    },
                    None => msg,
                };
                if !read_message(msg, &target_id, &mut main_frame, in_tx, &replies, &ask) {
                    return Ok(());
                }
            }
            Ok(None) => {}
            Err(e) if closing.load(Ordering::SeqCst) => {
                let _ = e;
                return Ok(());
            }
            Err(e) => return Err(anyhow!("the connection to Chrome ended: {e}")),
        }
    }
}

/// One message from Chrome to what the tab is told; false once the tab is gone.
fn read_message(mut msg: Value, target: &str, main_frame: &mut String, in_tx: &async_channel::Sender<In>, replies: &Replies, ask: &mpsc::Sender<Out>) -> bool {
    if let Some(id) = msg["id"].as_u64() {
        return match replies.lock().unwrap().remove(&id) {
            Some(Reply::Caller(reply)) => {
                let _ = reply.send(msg["result"].take());
                true
            }
            Some(Reply::Title) => {
                let title = msg["result"]["result"]["value"].as_str().unwrap_or_default().to_string();
                in_tx.send_blocking(In::Title(title)).is_ok()
            }
            Some(Reply::DevTools(_)) => true,
            None => true,
        };
    }
    let method = msg["method"].as_str().unwrap_or_default().to_string();
    let params = &msg["params"];
    let main = params["frameId"].as_str().is_some_and(|f| f == main_frame.as_str());
    let told = match method.as_str() {
        "Page.screencastFrame" => {
            let ack = params["sessionId"].as_i64().unwrap_or_default();
            let bytes = params["data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok()).unwrap_or_default();
            In::Frame(bytes, ack)
        }
        "Page.frameNavigated" if params["frame"]["parentId"].is_null() => {
            let frame = &params["frame"];
            *main_frame = frame["id"].as_str().unwrap_or(target).to_string();
            match frame["unreachableUrl"].as_str() {
                Some(url) => In::Navigated { url: url.to_string(), failed: true },
                None => In::Navigated { url: frame["url"].as_str().unwrap_or_default().to_string(), failed: false },
            }
        }
        "Page.navigatedWithinDocument" if main => In::Url(params["url"].as_str().unwrap_or_default().to_string()),
        "Page.loadEventFired" => {
            let _ = ask.send(Out::AskTitle);
            In::Loading(false)
        }
        "Page.frameStartedLoading" if main => In::Loading(true),
        "Page.frameStoppedLoading" if main => In::Loading(false),
        "Target.targetDestroyed" | "Target.detachedFromTarget" if params["targetId"].as_str() == Some(target) => In::Closed,
        "Inspector.targetCrashed" => In::Error("The page crashed: Reload starts it again".into()),
        "Page.javascriptDialogOpening" => In::Dialog(Dialog {
            kind: params["type"].as_str().unwrap_or("alert").to_string(),
            message: params["message"].as_str().unwrap_or_default().to_string(),
        }),
        "Page.javascriptDialogClosed" => In::DialogClosed,
        "Page.fileChooserOpened" => {
            In::FileChooser { node: params["backendNodeId"].as_i64().unwrap_or_default(), multiple: params["mode"] == "selectMultiple" }
        }
        "Runtime.bindingCalled" if params["name"] == BINDING => {
            let Ok(payload) = serde_json::from_str::<Value>(params["payload"].as_str().unwrap_or_default()) else {
                return true;
            };
            if let Some(title) = payload["title"].as_str() {
                In::Title(title.to_string())
            } else if payload["passkey"] == true {
                In::Passkey
            } else if payload["caret"].is_object() {
                let c = &payload["caret"];
                let n = |v: &Value| px(v.as_f64().unwrap_or(0.) as f32);
                In::Caret(Bounds { origin: point(n(&c["x"]), n(&c["y"])), size: size(px(2.), n(&c["height"])) })
            } else if let Some(cursor) = payload["cursor"].as_str() {
                In::Cursor(cursor.to_string())
            } else if payload["picker"].is_object() {
                let p = &payload["picker"];
                let n = |v: &Value| px(v.as_f64().unwrap_or(0.) as f32);
                In::Picker(Picker {
                    kind: p["type"].as_str().unwrap_or_default().to_string(),
                    value: p["value"].as_str().unwrap_or_default().to_string(),
                    bounds: Bounds { origin: point(n(&p["x"]), n(&p["y"])), size: size(n(&p["width"]), n(&p["height"])) },
                    context: params["executionContextId"].as_i64().unwrap_or_default(),
                })
            } else if payload["select"].is_object() {
                let s = &payload["select"];
                let n = |v: &Value| px(v.as_f64().unwrap_or(0.) as f32);
                In::Select(SelectMenu {
                    bounds: Bounds { origin: point(n(&s["x"]), n(&s["y"])), size: size(n(&s["width"]), n(&s["height"])) },
                    choices: s["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|o| Choice {
                            label: o["label"].as_str().unwrap_or_default().to_string(),
                            disabled: o["disabled"].as_bool().unwrap_or(false),
                            group: o["group"].as_str().map(str::to_string),
                        })
                        .collect(),
                    selected: s["selected"].as_i64().and_then(|i| usize::try_from(i).ok()),
                    context: params["executionContextId"].as_i64().unwrap_or_default(),
                })
            } else {
                return true;
            }
        }
        _ => match problem(&method, params) {
            Some(text) => In::Problem(text),
            None => return true,
        },
    };
    in_tx.send_blocking(told).is_ok()
}

/// Writes what the tab asks for. The pointer's moves waiting are sent as
/// the last of them; a resize while frames come waits for the size to settle.
#[allow(clippy::too_many_arguments)]
fn write_page(
    owner: &Owner,
    writer: Writer,
    session: String,
    target: &str,
    out_rx: mpsc::Receiver<Out>,
    replies: &Replies,
    closing: &AtomicBool,
) {
    // Shared by `send` and the close below.
    let writer = std::cell::RefCell::new(writer);
    let send = |method: &str, params: Value, in_page: bool, reply: Option<Reply>| -> bool {
        let id = writer.borrow_mut().next_id();
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if in_page {
            msg["sessionId"] = json!(session);
        }
        if let Some(reply) = reply {
            replies.lock().unwrap().insert(id, reply);
        }
        writer.borrow_mut().write(&msg)
    };
    let mut waiting: VecDeque<Out> = VecDeque::new();
    let mut resize: Option<((u32, u32, f32, bool, Option<Value>, String), Instant)> = None;
    let mut touch = false;
    let mut agent: Option<Value> = None;
    let mut casting = false;
    loop {
        let next = match waiting.pop_front() {
            Some(out) => Some(out),
            None => match resize {
                Some((_, at)) => match out_rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                    Ok(out) => Some(out),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => Some(Out::Close),
                },
                None => Some(out_rx.recv().unwrap_or(Out::Close)),
            },
        };
        let ok = match next {
            None => {
                let ((width, height, scale, mobile, new_agent, url), _) = resize.take().expect("a resize waits");
                // Another device: its user agent, and the page loaded again to see it;
                // by its address, as a first load may not be done (a reload would load what is).
                if new_agent != agent {
                    let told = new_agent.clone().unwrap_or_else(|| json!({ "userAgent": "" }));
                    send("Emulation.setUserAgentOverride", told, true, None);
                    if url.is_empty() || url == "about:blank" {
                        send("Page.reload", json!({}), true, None);
                    } else {
                        send("Page.navigate", json!({ "url": url }), true, None);
                    }
                    agent = new_agent;
                }
                if casting {
                    send("Page.stopScreencast", json!({}), true, None);
                }
                casting = true;
                // A phone's page is touched: the mouse's presses come to it as touches.
                if mobile != touch {
                    touch = mobile;
                    send("Emulation.setTouchEmulationEnabled", json!({ "enabled": mobile, "maxTouchPoints": 5 }), true, None);
                    send("Emulation.setEmitTouchEventsForMouse", json!({ "enabled": mobile, "configuration": "mobile" }), true, None);
                }
                send(
                    "Emulation.setDeviceMetricsOverride",
                    json!({ "width": width, "height": height, "deviceScaleFactor": scale, "mobile": mobile }),
                    true,
                    None,
                ) && send(
                    "Page.startScreencast",
                    json!({ "format": "jpeg", "quality": 95, "maxWidth": (width as f32 * scale).round() as u32, "maxHeight": (height as f32 * scale).round() as u32 }),
                    true,
                    None,
                )
            }
            Some(Out::Command(method, params)) => {
                if method == "Input.dispatchMouseEvent" && opens_a_tab(&params) {
                    *slot(owner).clicked.lock().unwrap() = Some((target.to_string(), Instant::now()));
                }
                send(method, params, true, None)
            }
            Some(Out::Raw(mut msg, reply)) => {
                let id = writer.borrow_mut().next_id();
                msg["id"] = json!(id);
                replies.lock().unwrap().insert(id, reply);
                writer.borrow_mut().write(&msg)
            }
            Some(Out::Call(method, params, reply)) => send(method, params, true, Some(Reply::Caller(reply))),
            Some(Out::AskTitle) => {
                send("Runtime.evaluate", json!({ "expression": "document.title", "returnByValue": true }), true, Some(Reply::Title))
            }
            Some(Out::Move(mut params)) => {
                while let Ok(out) = out_rx.try_recv() {
                    match out {
                        Out::Move(later) => params = later,
                        other => {
                            waiting.push_back(other);
                            break;
                        }
                    }
                }
                send("Input.dispatchMouseEvent", params, true, None)
            }
            Some(Out::Resize(width, height, scale, mobile, agent, url)) => {
                // The first size at once; later ones once a drag of the divider ends.
                let wait = if casting { Duration::from_millis(120) } else { Duration::ZERO };
                resize = Some(((width, height, scale, mobile, agent, url), Instant::now() + wait));
                true
            }
            Some(Out::Ack(id)) => send("Page.screencastFrameAck", json!({ "sessionId": id }), true, None),
            Some(Out::Visible(visible)) => {
                if !visible && casting {
                    casting = false;
                    resize = None;
                    send("Page.stopScreencast", json!({}), true, None)
                } else if visible {
                    // In front of its window, where a page opened from it may have been.
                    send("Target.activateTarget", json!({ "targetId": target }), false, None)
                } else {
                    true
                }
            }
            Some(Out::Close) => {
                closing.store(true, Ordering::SeqCst);
                send("Target.closeTarget", json!({ "targetId": target }), false, None);
                writer.borrow_mut().end();
                return;
            }
        };
        if !ok {
            closing.store(true, Ordering::SeqCst);
            writer.borrow_mut().end();
            return;
        }
    }
}

// ---------------------------------------------------------------- pictures

/// A picture of the page at `url` as a window of this size shows it, into
/// `out` (PNG). In a page of its own, closed after; waits for the page to
/// load and a moment more, for what it draws once loaded. Not for the UI
/// thread.
pub fn shot(url: &str, width: u32, height: u32, out: &std::path::Path) -> Result<()> {
    let _open = OpenPage::new(&Owner::Pictures);
    let chrome = chrome(&Owner::Pictures)?;
    let mut socket = connect(&chrome)?;
    let mut aside = Vec::new();
    let target = socket.call("Target.createTarget", json!({ "url": "about:blank" }), &mut aside)?;
    let target_id = target["targetId"].as_str().unwrap_or_default().to_string();
    let taken = (|| -> Result<Vec<u8>> {
        let attached = socket.call("Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }), &mut aside)?;
        socket.set_session(attached["sessionId"].as_str().map(str::to_string));
        socket.call("Page.enable", json!({}), &mut aside)?;
        socket.call(
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": width, "height": height, "deviceScaleFactor": 1, "mobile": false }),
            &mut aside,
        )?;
        socket.set_timeout(Some(Duration::from_millis(100)));
        aside.clear();
        let gone = socket.call("Page.navigate", json!({ "url": url }), &mut aside)?;
        if let Some(error) = gone["errorText"].as_str().filter(|e| !e.is_empty()) {
            return Err(anyhow!("{url}: {error}"));
        }
        // Loaded, or as far as it got in a while: a page that keeps a
        // request open never says so.
        let started = Instant::now();
        let loaded = |msg: &Value| msg["method"] == "Page.loadEventFired";
        let mut done = aside.iter().any(loaded);
        while !done && started.elapsed() < Duration::from_secs(15) {
            done = socket.read()?.is_some_and(|msg| loaded(&msg));
        }
        std::thread::sleep(Duration::from_millis(1200));
        let picture = socket.call("Page.captureScreenshot", json!({ "format": "png" }), &mut aside)?;
        Ok(base64::engine::general_purpose::STANDARD.decode(picture["data"].as_str().unwrap_or_default())?)
    })();
    socket.set_session(None);
    let _ = socket.send("Target.closeTarget", json!({ "targetId": target_id }));
    let png = taken?;
    if let Some(folder) = out.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(out, png)?;
    Ok(())
}

// ---------------------------------------------------------------- picking

/// Run in the page with a point of its viewport: the element there, through
/// shadow roots. Its box alone for the highlight; with `full`, what tells
/// an agent which element it is: a selector that finds it and nothing
/// else, its tag with attributes, its text, and for React the components
/// around it and where the nearest is written (development builds).
const ELEMENT_AT: &str = r##"(function (x, y, full) {
  let el = document.elementFromPoint(x, y);
  while (el && el.shadowRoot) {
    const inner = el.shadowRoot.elementFromPoint(x, y);
    if (!inner || inner === el) break;
    el = inner;
  }
  if (!el) return "";
  const r = el.getBoundingClientRect();
  const box = { x: r.left, y: r.top, width: r.width, height: r.height };
  if (!full) return JSON.stringify({ box });
  const step = (e) => {
    if (e.id && document.querySelectorAll("#" + CSS.escape(e.id)).length === 1) return "#" + CSS.escape(e.id);
    let s = e.localName + [...e.classList].slice(0, 2).map((c) => "." + CSS.escape(c)).join("");
    const same = e.parentElement ? [...e.parentElement.children].filter((c) => c.localName === e.localName) : [];
    if (same.length > 1) s += ":nth-of-type(" + (same.indexOf(e) + 1) + ")";
    return s;
  };
  const parts = [];
  let selector = "";
  for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
    parts.unshift(step(e));
    selector = parts.join(" > ");
    if (parts[0].startsWith("#")) break;
    try {
      if (document.querySelectorAll(selector).length === 1) break;
    } catch (_) {}
  }
  const components = [];
  let source = "";
  try {
    const key = Object.keys(el).find((k) => k.startsWith("__reactFiber$"));
    for (let f = key && el[key]; f && components.length < 4; f = f.return) {
      const name = f.type && typeof f.type !== "string" && (f.type.displayName || f.type.name);
      if (name) components.push(name);
      if (!source && f._debugSource) source = f._debugSource.fileName + ":" + f._debugSource.lineNumber;
    }
  } catch (_) {}
  const text = (el.innerText || el.value || el.alt || "").trim().replace(/\s+/g, " ");
  // What its look is made of, leaving out what every element has.
  const cs = getComputedStyle(el);
  const dull = new Set(["none", "normal", "auto", "0px", "rgba(0, 0, 0, 0)", "static", "visible", "0", "1", "start", "stretch", "content-box", "0px none rgb(0, 0, 0)"]);
  const styles = [];
  for (const k of ["display", "position", "width", "height", "margin", "padding", "font-family", "font-size", "font-weight", "line-height", "color",
    "background-color", "background-image", "border", "border-radius", "box-shadow", "opacity", "z-index", "overflow", "flex-direction",
    "justify-content", "align-items", "gap", "grid-template-columns", "text-align", "transform"]) {
    const v = cs.getPropertyValue(k);
    if (v && !dull.has(v)) styles.push(k + ": " + v);
  }
  return JSON.stringify({
    box,
    scroll: { x: window.scrollX, y: window.scrollY },
    url: location.href,
    selector,
    html: el.cloneNode(false).outerHTML.slice(0, 400),
    text: text.slice(0, 200),
    components,
    source,
    styles: styles.join("; "),
  });
})"##;

/// A command's result, waited for: not for the UI thread.
fn call(out: &mpsc::Sender<Out>, method: &'static str, params: Value) -> Option<Value> {
    let (tx, rx) = mpsc::channel();
    out.send(Out::Call(method, params, tx)).ok()?;
    rx.recv_timeout(Duration::from_secs(5)).ok()
}

fn element_at(out: &mpsc::Sender<Out>, x: f32, y: f32, full: bool) -> Option<Value> {
    let expression = format!("{ELEMENT_AT}({x}, {y}, {full})");
    let result = call(out, "Runtime.evaluate", json!({ "expression": expression, "returnByValue": true }))?;
    serde_json::from_str(result["result"]["value"].as_str()?).ok()
}

/// The box of the element at a point, in the viewport.
fn element_box(out: &mpsc::Sender<Out>, x: f32, y: f32) -> Option<Bounds<Pixels>> {
    let element = element_at(out, x, y, false)?;
    let side = |name: &str| element["box"][name].as_f64().map(|v| px(v as f32));
    Some(Bounds { origin: point(side("x")?, side("y")?), size: size(side("width")?, side("height")?) })
}

/// Old pictures go when a new one comes: the agent has long seen them.
fn picture_path(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    for old in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let age = old.metadata().and_then(|m| m.modified()).ok().and_then(|at| at.elapsed().ok());
        if age.is_some_and(|age| age > Duration::from_secs(24 * 3600)) {
            let _ = std::fs::remove_file(old.path());
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    Some(dir.join(format!("{name}-{stamp}.png")))
}

/// What was picked for the agent: an element, an area, the whole page.
#[derive(Clone)]
struct Picked {
    /// Short, for its chip.
    label: String,
    /// What the agent is told of it.
    text: String,
}

fn save_picture(shot: Option<Value>, dir: &Path, name: &str) -> Option<PathBuf> {
    let png = base64::engine::general_purpose::STANDARD.decode(shot?["data"].as_str()?).ok()?;
    let path = picture_path(dir, name)?;
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// The element at a point, written out for an agent, with a picture of it
/// saved under `dir`.
fn pick(out: &mpsc::Sender<Out>, x: f32, y: f32, dir: &Path) -> Option<Picked> {
    let element = element_at(out, x, y, true)?;
    let text = |name: &str| element[name].as_str().unwrap_or_default();
    let number = |value: &Value| value.as_f64().unwrap_or(0.);
    let (bounds, scroll) = (&element["box"], &element["scroll"]);
    // The picture is cut out of the whole page, not the part in view.
    let clip = json!({
        "x": number(&bounds["x"]) + number(&scroll["x"]),
        "y": number(&bounds["y"]) + number(&scroll["y"]),
        "width": number(&bounds["width"]).max(1.),
        "height": number(&bounds["height"]).max(1.),
        "scale": 1,
    });
    let picture = save_picture(call(out, "Page.captureScreenshot", json!({ "format": "png", "clip": clip })), dir, "element");

    let mut message = format!("the element `{}`:\n{}\n", text("selector"), text("html"));
    if !text("text").is_empty() {
        message.push_str(&format!("text: {:?}\n", text("text")));
    }
    let components: Vec<&str> = element["components"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !components.is_empty() {
        message.push_str(&format!("React: {}", components.join(" in ")));
        if !text("source").is_empty() {
            message.push_str(&format!(" ({})", text("source")));
        }
        message.push('\n');
    }
    if !text("styles").is_empty() {
        message.push_str(&format!("styles: {}\n", text("styles")));
    }
    if let Some(path) = picture {
        message.push_str(&format!("picture: {}\n", path.display()));
    }
    let short: String = text("selector").rsplit(" > ").next().unwrap_or_default().chars().take(40).collect();
    Some(Picked { label: short, text: message })
}

/// A part of the page in view, by its corners in CSS pixels, as a picture.
fn pick_area(out: &mpsc::Sender<Out>, from: (f32, f32), to: (f32, f32), dir: &Path) -> Option<Picked> {
    let scroll = call(out, "Runtime.evaluate", json!({ "expression": "[scrollX, scrollY]", "returnByValue": true }))?;
    let scroll = &scroll["result"]["value"];
    let (x, y) = (from.0.min(to.0), from.1.min(to.1));
    let (w, h) = ((from.0 - to.0).abs().max(1.), (from.1 - to.1).abs().max(1.));
    let clip = json!({ "x": f64::from(x) + scroll[0].as_f64().unwrap_or(0.), "y": f64::from(y) + scroll[1].as_f64().unwrap_or(0.), "width": w, "height": h, "scale": 1 });
    let path = save_picture(call(out, "Page.captureScreenshot", json!({ "format": "png", "clip": clip })), dir, "area")?;
    Some(Picked {
        label: format!("area {}×{}", w.round(), h.round()),
        text: format!("an area of the page in view, {}×{} CSS pixels at {},{}:\npicture: {}\n", w.round(), h.round(), x.round(), y.round(), path.display()),
    })
}

/// The whole page, scrolled out of view too, as a picture: up to a height
/// an agent can still read.
fn pick_page(out: &mpsc::Sender<Out>, dir: &Path) -> Option<Picked> {
    let metrics = call(out, "Page.getLayoutMetrics", json!({}))?;
    let content = &metrics["cssContentSize"];
    let (w, h) = (content["width"].as_f64().unwrap_or(1.).max(1.), content["height"].as_f64().unwrap_or(1.).clamp(1., 12000.));
    let clip = json!({ "x": 0, "y": 0, "width": w, "height": h, "scale": 1 });
    let path = save_picture(call(out, "Page.captureScreenshot", json!({ "format": "png", "clip": clip, "captureBeyondViewport": true })), dir, "page")?;
    Some(Picked { label: "whole page".into(), text: format!("the whole page, {}×{} CSS pixels:\npicture: {}\n", w.round(), h.round(), path.display()) })
}

/// An element of a page of the user's, as it is seen: its text and picture,
/// none of its markup.
fn pick_seen(out: &mpsc::Sender<Out>, x: f32, y: f32, dir: &Path) -> Option<Picked> {
    let element = element_at(out, x, y, true)?;
    let number = |value: &Value| value.as_f64().unwrap_or(0.);
    let (bounds, scroll) = (&element["box"], &element["scroll"]);
    let clip = json!({
        "x": number(&bounds["x"]) + number(&scroll["x"]),
        "y": number(&bounds["y"]) + number(&scroll["y"]),
        "width": number(&bounds["width"]).max(1.),
        "height": number(&bounds["height"]).max(1.),
        "scale": 1,
    });
    let picture = save_picture(call(out, "Page.captureScreenshot", json!({ "format": "png", "clip": clip })), dir, "element");
    let text = element["text"].as_str().unwrap_or_default();
    let mut message = "a part of the page".to_string();
    if !text.is_empty() {
        message.push_str(&format!(", reading {text:?}"));
    }
    message.push('\n');
    if let Some(path) = picture {
        message.push_str(&format!("picture: {}\n", path.display()));
    }
    let label: String = if text.is_empty() { "element".into() } else { text.chars().take(30).collect() };
    Some(Picked { label, text: message })
}

/// How much of a page's text goes to an agent.
const PAGE_TEXT: usize = 20_000;

/// The page's text as it reads, up to `PAGE_TEXT` characters.
fn page_text(out: &mpsc::Sender<Out>) -> Option<Picked> {
    let said = call(out, "Runtime.evaluate", json!({ "expression": "document.body ? document.body.innerText : ''", "returnByValue": true }))?;
    let text = said["result"]["value"].as_str().unwrap_or_default().trim();
    let count = text.chars().count();
    let kept: String = text.chars().take(PAGE_TEXT).collect();
    let cut = if count > PAGE_TEXT { format!(" (its first {PAGE_TEXT} characters of {count})") } else { String::new() };
    Some(Picked { label: "page text".into(), text: format!("the page's text{cut}:\n```\n{kept}\n```\n") })
}

/// An address without what may be a sign-in's in it: query parameters
/// named as tokens, codes and signatures are, and a fragment with values
/// (OAuth's `#access_token=`).
pub fn without_secrets(url: &str) -> String {
    let secret = |key: &str| {
        let key = key.to_ascii_lowercase();
        ["token", "access_token", "id_token", "refresh_token", "code", "state", "sig", "signature", "key", "apikey", "api_key", "password", "secret", "session", "auth"]
            .contains(&key.as_str())
            || key.starts_with("x-amz-")
            || key.starts_with("x-goog-")
            || key.ends_with("_token")
    };
    let (rest, fragment) = match url.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (url, None),
    };
    let mut out = match rest.split_once('?') {
        Some((base, query)) => {
            let kept: Vec<&str> = query.split('&').filter(|p| !p.is_empty() && !secret(p.split_once('=').map_or(*p, |(k, _)| k))).collect();
            if kept.is_empty() { base.to_string() } else { format!("{base}?{}", kept.join("&")) }
        }
        None => rest.to_string(),
    };
    if let Some(fragment) = fragment.filter(|f| !f.contains('=')) {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// Whether a task's link opens in a tab of the user's own browser: as the
/// settings of its project say, where that browser can run, and never one
/// of this machine (a stand's: the task's own browser has it) or of a site
/// set for the system's browser.
pub fn opens_in_tab(root: &Path, url: &str) -> bool {
    let web = url.starts_with("https://") || url.starts_with("http://");
    web && !is_local(url)
        && crate::layers::of_root(root).values.links_open_in == "tab"
        && user_browser_here()
        && !crate::state::with_settings(|s| s.system_hosts.iter().any(|h| host_named(h, url)))
}

/// Whether the user's own browser can run here: a Chrome to speak to over
/// a pipe. Looked for once, off the UI thread; until found out, as if it can.
fn user_browser_here() -> bool {
    static FOUND: OnceLock<Arc<std::sync::atomic::AtomicU8>> = OnceLock::new();
    if !cfg!(any(unix, windows)) {
        return false;
    }
    let found = FOUND.get_or_init(|| {
        let found = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let told = found.clone();
        std::thread::spawn(move || {
            let here = std::env::var_os("IDEAE_CHROME").is_some() || find_chrome().is_some();
            told.store(if here { 1 } else { 2 }, Ordering::Relaxed);
        });
        found
    });
    found.load(Ordering::Relaxed) != 2
}

/// The host of an address, in lower case.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => host,
        _ => authority,
    };
    Some(host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase())
}

/// Whether `pattern` (`example.com`, `*.example.com`) names the host of `url`.
fn host_named(pattern: &str, url: &str) -> bool {
    let Some(host) = host_of(url) else { return false };
    let pattern = pattern.trim().to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(domain) => host == domain || host.ends_with(&format!(".{domain}")),
        None => host == pattern,
    }
}

/// Whether `url` is of this machine: a dev server's, for a Web tab.
pub fn is_local(url: &str) -> bool {
    local_address(url).is_some()
}

/// The port of an address on this machine, and where it is written in
/// `url`: its digits, or, not written (the scheme's), the empty place at
/// the end of the host where `:port` would go (the flag false).
#[cfg_attr(not(unix), allow(dead_code))]
pub fn port_place(url: &str) -> Option<(u16, std::ops::Range<usize>, bool)> {
    let port = local_address(url)?.rsplit_once(':')?.1.parse().ok()?;
    let start = url.find("://")? + 3;
    let rest = url.get(start..)?;
    let authority_end = start + rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = url.get(start..authority_end)?;
    match authority.rfind(':').filter(|&at| authority.get(at..).is_some_and(|tail| !tail.contains(']'))) {
        Some(colon) => Some((port, start + colon + 1..authority_end, true)),
        None => Some((port, authority_end..authority_end, false)),
    }
}

/// `host:port` of an address on this machine, for waiting until something
/// listens there.
fn local_address(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => (host, port.parse::<u16>().ok()?),
        _ => (authority, if scheme == "https" { 443 } else { 80 }),
    };
    let host = match host {
        "localhost" | "127.0.0.1" | "[::1]" => host,
        "0.0.0.0" => "127.0.0.1",
        h if h.ends_with(".localhost") => "localhost",
        _ => return None,
    };
    Some(format!("{host}:{port}"))
}

/// What CSS calls a cursor, as the window draws it.
fn cursor_style(css: &str) -> CursorStyle {
    match css {
        "pointer" => CursorStyle::PointingHand,
        "text" => CursorStyle::IBeam,
        "vertical-text" => CursorStyle::IBeamCursorForVerticalLayout,
        "crosshair" | "cell" => CursorStyle::Crosshair,
        "grab" | "move" | "all-scroll" => CursorStyle::OpenHand,
        "grabbing" => CursorStyle::ClosedHand,
        "not-allowed" | "no-drop" => CursorStyle::OperationNotAllowed,
        "col-resize" => CursorStyle::ResizeColumn,
        "row-resize" => CursorStyle::ResizeRow,
        "ew-resize" => CursorStyle::ResizeLeftRight,
        "e-resize" => CursorStyle::ResizeRight,
        "w-resize" => CursorStyle::ResizeLeft,
        "ns-resize" => CursorStyle::ResizeUpDown,
        "n-resize" => CursorStyle::ResizeUp,
        "s-resize" => CursorStyle::ResizeDown,
        "nwse-resize" | "nw-resize" | "se-resize" => CursorStyle::ResizeUpLeftDownRight,
        "nesw-resize" | "ne-resize" | "sw-resize" => CursorStyle::ResizeUpRightDownLeft,
        "context-menu" => CursorStyle::ContextualMenu,
        "copy" => CursorStyle::DragCopy,
        "alias" => CursorStyle::DragLink,
        _ => CursorStyle::Arrow,
    }
}

/// The DOM's `code` and Windows key code of a key that types `c`, as on a
/// US keyboard: pages that look at either see the key they expect.
fn key_code(c: char) -> (String, u32) {
    let c = c.to_ascii_lowercase();
    match c {
        'a'..='z' => (format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_uppercase() as u32),
        '0'..='9' => (format!("Digit{c}"), c as u32),
        ' ' => ("Space".into(), 32),
        ';' | ':' => ("Semicolon".into(), 186),
        '=' | '+' => ("Equal".into(), 187),
        ',' | '<' => ("Comma".into(), 188),
        '-' | '_' => ("Minus".into(), 189),
        '.' | '>' => ("Period".into(), 190),
        '/' | '?' => ("Slash".into(), 191),
        '`' | '~' => ("Backquote".into(), 192),
        '[' | '{' => ("BracketLeft".into(), 219),
        '\\' | '|' => ("Backslash".into(), 220),
        ']' | '}' => ("BracketRight".into(), 221),
        '\'' | '"' => ("Quote".into(), 222),
        _ => (String::new(), 0),
    }
}

/// Screens a page can be shown at: name, CSS size, a phone's or tablet's.
const DEVICES: [(&str, u32, u32, bool); 5] =
    [("iPhone 15", 393, 852, true), ("Pixel 8", 412, 915, true), ("iPad", 820, 1180, true), ("Laptop", 1366, 768, false), ("Desktop", 1920, 1080, false)];

/// What the device's browser says it is, for sites that tell phones by it:
/// `Emulation.setUserAgentOverride`'s parameters. Chrome's own on a desktop.
fn user_agent(device: &str, major: &str) -> Option<Value> {
    let safari = |what: &str| format!("Mozilla/5.0 ({what} like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1");
    match device {
        "iPhone 15" => Some(json!({ "userAgent": safari("iPhone; CPU iPhone OS 17_5"), "platform": "iPhone" })),
        "iPad" => Some(json!({ "userAgent": safari("iPad; CPU OS 17_5"), "platform": "iPad" })),
        "Pixel 8" => Some(json!({
            "userAgent": format!("Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Mobile Safari/537.36"),
            "platform": "Linux armv8l",
            "userAgentMetadata": {
                "brands": [{ "brand": "Chromium", "version": major }, { "brand": "Google Chrome", "version": major }, { "brand": "Not.A/Brand", "version": "99" }],
                "platform": "Android", "platformVersion": "14.0.0", "architecture": "", "model": "Pixel 8", "mobile": true,
            },
        })),
        _ => None,
    }
}

/// The colors a page is asked for (`prefers-color-scheme`).
#[derive(Clone, Copy, PartialEq)]
enum Scheme {
    AsTheIde,
    Light,
    Dark,
}

/// A date's or color's value as a field of that type writes it.
fn picker_format(kind: &str) -> &'static str {
    match kind {
        "date" => "YYYY-MM-DD",
        "time" => "HH:MM",
        "datetime-local" => "YYYY-MM-DDTHH:MM",
        "month" => "YYYY-MM",
        "week" => "YYYY-W01",
        "color" => "#rrggbb",
        _ => "",
    }
}

const ZOOMS: [f32; 13] = [0.5, 0.67, 0.75, 0.8, 0.9, 1., 1.1, 1.25, 1.5, 1.75, 2., 2.5, 3.];

// ---------------------------------------------------------------- view

actions!(
    ideae_browser,
    [
        BrowserBack,
        BrowserForward,
        BrowserReload,
        BrowserHardReload,
        BrowserCopy,
        BrowserPaste,
        BrowserCopyUrl,
        BrowserOpenExternal,
        BrowserFocusAddress,
        BrowserFind,
        BrowserZoomIn,
        BrowserZoomOut,
        BrowserZoomReset,
        BrowserDevTools
    ]
);

pub const KEY_CONTEXT: &str = "Browser";

pub enum BrowserEvent {
    /// The page's address or title changed, so the tab's label should too.
    Navigated,
    /// Text for the agent's prompt: an element picked, the page's errors.
    ToAgent(String),
    /// The page closed itself, or was closed from elsewhere.
    Closed,
}

impl EventEmitter<BrowserEvent> for BrowserView {}

pub struct BrowserView {
    owner: Owner,
    /// A tab of the user's own browser: whose, as its toolbar shows it, and
    /// the task it is a tab of, which gets the pages it opens.
    user: Option<UserMark>,
    task_root: Option<PathBuf>,
    /// What its mark tells of it.
    whose: SharedString,
    /// The page asked for a passkey or a security key: the system's browser is offered.
    passkey: bool,
    /// Shown while presenting all the same, as asked.
    unveiled: bool,
    /// The page alone, no toolbar: DevTools in its panel.
    bare: bool,
    /// The page's id in Chrome and Chrome's major version, once connected.
    target: Option<String>,
    major: String,
    devtools: Option<Entity<BrowserView>>,
    /// DevTools is to be shown: kept while the page is asleep or not yet connected.
    devtools_wanted: bool,
    dock_right: bool,
    /// The panel's share of the page area.
    dock_size: f32,
    dock_area: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    resizing: bool,
    /// Where text goes in the page (CSS pixels), for an input method's window.
    caret: Option<Bounds<Pixels>>,
    out: mpsc::Sender<Out>,
    /// Its page closed while the task was long out of view: opened again,
    /// at its address, when the tab is next drawn.
    asleep: bool,
    url_input: Entity<InputState>,
    /// The press now on the address came while the keyboard was elsewhere.
    url_cold: bool,
    /// How the task's own addresses are found, and those found when the address was last gone to: offered under it.
    places: Option<Places>,
    offered: Vec<Place>,
    offering: bool,
    _offer: Task<()>,
    url: SharedString,
    page_title: SharedString,
    loading: bool,
    port: Option<u16>,
    frame: Option<Arc<RenderImage>>,
    error: Option<String>,
    focus: FocusHandle,
    /// Where the page is drawn, for mapping the pointer, and the size the
    /// page was last told, to resize it only on change.
    bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    sent_size: std::rc::Rc<std::cell::Cell<(u32, u32, u32, u32)>>,
    /// Window pixels to each CSS pixel of the page, and how far right of the
    /// area's edge the page is drawn (a screen narrower than the area is centered).
    layout: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
    /// The screen the page is shown at, from `DEVICES`, turned or not; none
    /// for the tab's own size.
    device: Option<usize>,
    turned: bool,
    scheme: Scheme,
    /// Dark or not, and calm or not, as the page was last told.
    sent_media: Option<(bool, bool)>,
    /// Text an input method is composing.
    marked: Option<String>,
    /// In a tab that is shown, of a task in view.
    visible: bool,
    zoom: f32,
    cursor: CursorStyle,
    /// The mouse buttons held since a press on the page, as CDP counts them:
    /// their moves and release go to the page wherever the pointer is.
    pressed: u32,
    /// A click picks an element for the agent instead of going to the page.
    picking: bool,
    /// The element under the pointer while picking, in the page area, and
    /// whether the page is being asked for it.
    hover: Option<Bounds<Pixels>>,
    asking: bool,
    /// What is picked for the agent so far, what the user says of it, and
    /// an area being dragged over (window points), with picks under way.
    picked: Vec<Picked>,
    pick_note: Entity<InputState>,
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    picks_waiting: usize,
    picker: Option<Picker>,
    picker_input: Entity<InputState>,
    picker_wrong: bool,
    dialog: Option<Dialog>,
    prompt_input: Entity<InputState>,
    select: Option<SelectMenu>,
    select_ix: Option<usize>,
    problems: Vec<String>,
    problems_open: bool,
    find_input: Entity<InputState>,
    finding: bool,
    /// The address of a page that could not be had: tried again once
    /// something listens there.
    failed: Option<String>,
    _wait: Task<()>,
    /// Frames of the latest run of them, close together.
    burst: u32,
    last_frame: Option<Instant>,
    _frames: Task<()>,
    /// Frames taken so far, to tell whether one came since a moment.
    frames_taken: u64,
    /// The last frame as Chrome sent it, compressed: shown again at once
    /// when the tab comes back in view, before the stream sends a new one.
    last_jpeg: Option<Arc<[u8]>>,
    /// A picture asked of the page when no frames come after it loads or
    /// comes back in view: the stream sends one only when the page is drawn
    /// again, and may stay quiet on a page that already was.
    _probe: Task<()>,
    /// A sharp PNG of the page once screencast frames stop coming: the JPEG
    /// frames are for motion.
    _sharpen: Task<()>,
}

impl Drop for BrowserView {
    fn drop(&mut self) {
        let _ = self.out.send(Out::Close);
    }
}

impl BrowserView {
    /// Given by the task: how its own addresses are found.
    pub fn set_places(&mut self, places: Places) {
        self.places = Some(places);
    }

    /// The port the page is at, when it is a page of this machine.
    pub fn own_port(&self) -> Option<u16> {
        local_port(&self.url)
    }

    /// Loaded again, as Reload does.
    pub fn load_again(&mut self) {
        self.reload(false);
    }

    fn offer_places(&mut self, cx: &mut Context<Self>) {
        let Some(places) = self.places.clone() else { return };
        self.offering = true;
        self._offer = cx.spawn(async move |this, cx| {
            let found = cx.background_spawn(async move { places() }).await;
            let _ = this.update(cx, |this, cx| {
                this.offered = found;
                cx.notify();
            });
        });
        cx.notify();
    }

    /// The task's own addresses under the address being written: those that
    /// have what is typed in them, the ones something listens at first.
    fn render_places(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.offering {
            return None;
        }
        let typed = self.url_input.read(cx).value().trim().to_lowercase();
        // The address as it stands is not something typed to look for.
        let typed = if typed == self.url.to_lowercase() { String::new() } else { typed };
        let mut places: Vec<&Place> = self.offered.iter().filter(|p| typed.is_empty() || p.address().contains(&typed) || p.label.to_lowercase().contains(&typed)).collect();
        places.sort_by_key(|p| !p.up);
        if places.is_empty() {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let mut list = v_flex().id("web-places").absolute().top_0().left(px(84.)).w(px(420.)).py_1().rounded_md().border_1().border_color(cx.theme().border).bg(cx.theme().popover).shadow_md().occlude().text_sm();
        for (ix, place) in places.into_iter().take(12).enumerate() {
            let address = place.address();
            list = list.child(
                h_flex()
                    .id(("web-place", ix))
                    .px_3()
                    .py_0p5()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|row| row.bg(cx.theme().accent))
                    .child(div().w(px(10.)).text_xs().text_color(if place.up { gpui_kit::green() } else { muted }).child(if place.up { "●" } else { "○" }))
                    .child(div().flex_none().child(address.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(place.label.clone()))
                    // On the press: the address loses the keyboard with it, and this list goes before a click is one.
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.offering = false;
                        this.go_there(&address, cx);
                        this.focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        Some(list.into_any_element())
    }

    pub fn url(&self) -> String {
        self.url.to_string()
    }

    /// A tab of the user's browser, in the task at `root`.
    pub fn for_user(&mut self, mark: UserMark, root: &Path) {
        let plain = if user_logins_plain() { " Its logins are kept unencrypted here, as no keyring can be asked for a key." } else { "" };
        self.whose = format!(
            "{}'s browser: your own, its logins kept apart from other spaces' and out of agents' reach. No agent drives it; Pick sends only what you choose.{plain}",
            mark.name
        )
        .into();
        self.user = Some(mark);
        self.task_root = Some(root.to_path_buf());
        if let Some(target) = &self.target {
            note_task_page(&self.owner, target, root);
        }
    }

    pub fn is_user(&self) -> bool {
        self.user.is_some()
    }

    /// The address as it goes to the agent: of the user's browser, without
    /// what in it may be a sign-in's (a token, a code, a signature).
    fn told_url(&self) -> String {
        if self.user.is_some() { without_secrets(&self.url) } else { self.url.to_string() }
    }

    /// To another address.
    pub fn go(&self, url: &str) {
        self.command("Page.navigate", json!({ "url": url }));
    }

    /// Goes to an address typed or picked: a remote task's own (`localhost:3000`
    /// there) once its port is forwarded here ([`crate::hostfs::reach`]).
    fn go_there(&self, url: &str, cx: &mut Context<Self>) {
        let Owner::Task(root) = &self.owner else { return self.go(url) };
        if !crate::hosts::is_remote(root) || !is_local(url) {
            return self.go(url);
        }
        let (root, asked) = (root.clone(), url.to_string());
        let host = crate::hosts::split(&root).map(|(host, _)| host).unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let reached = cx.background_spawn(async move { crate::hostfs::reach(&root, &asked) }).await;
            let _ = this.update(cx, |page, cx| match reached {
                Ok(url) => page.go(&url),
                // Said on the page, where the address was typed.
                Err(e) => {
                    page.error = Some(format!("Not reached through ssh on {}: {}", crate::hosts::label(&host), crate::hosts::explain_ssh(&host, &e)));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A page's connection, and the task that takes what it tells.
    fn attach(source: Source, owner: Owner, window: &mut Window, cx: &mut Context<Self>) -> (mpsc::Sender<Out>, Task<()>) {
        let (in_tx, in_rx) = async_channel::bounded::<In>(64);
        note_screen(window.scale_factor());
        let out = spawn_page(owner, source, in_tx);
        let frames = cx.spawn_in(window, async move |this, cx| {
            while let Ok(message) = in_rx.recv().await {
                let message = match message {
                    In::Frame(jpeg, ack) => {
                        let (image, jpeg) = cx
                            .background_spawn(async move {
                                let jpeg: Arc<[u8]> = jpeg.into();
                                (decode(&jpeg, image::ImageFormat::Jpeg), jpeg)
                            })
                            .await;
                        Ok((image, jpeg, ack))
                    }
                    other => Err(other),
                };
                let ok = this.update_in(cx, |this, window, cx| {
                    match message {
                        Ok((image, jpeg, ack)) => this.take_frame(image, jpeg, ack, cx),
                        Err(other) => this.take(other, window, cx),
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        });
        (out, frames)
    }

    pub fn new(source: Source, owner: Owner, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (url, loading) = match &source {
            Source::Url(url) => (url.clone(), true),
            Source::Target(_) | Source::DevTools(_) => (String::new(), false),
        };
        let (out, frames) = Self::attach(source, owner.clone(), window, cx);
        // The last frame's pixels go with the tab.
        cx.on_release(|this, cx| {
            if let Some(old) = this.frame.take() {
                // After the update that closed the tab: see `drop_later`.
                cx.defer(move |cx| cx.drop_image(old, None));
            }
        })
        .detach();
        let url_input = cx.new(|cx| InputState::new(window, cx).placeholder("http://localhost:3000"));
        url_input.update(cx, |input, cx| input.set_value(url.clone(), window, cx));
        cx.subscribe_in(&url_input, window, |this, input, event: &InputEvent, window, cx| {
            match event {
                // The task's own addresses are offered while the address is being written.
                InputEvent::Focus => this.offer_places(cx),
                InputEvent::Blur => {
                    this.offering = false;
                    cx.notify();
                }
                InputEvent::Change if this.offering => cx.notify(),
                _ => {}
            }
            if matches!(event, InputEvent::PressEnter { .. }) {
                let mut url = input.read(cx).value().trim().to_string();
                if !url.contains("://") && !url.is_empty() && !url.starts_with("about:") {
                    url = format!("http://{url}");
                }
                this.go_there(&url, cx);
                this.focus.focus(window, cx);
            }
        })
        .detach();
        let prompt_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&prompt_input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.answer(true, window, cx);
            }
        })
        .detach();
        let find_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in page"));
        cx.subscribe_in(&find_input, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { shift, .. } = event {
                this.find_next(*shift, cx);
            }
        })
        .detach();
        let pick_note = cx.new(|cx| InputState::new(window, cx).placeholder("What to do with them (Enter sends to the agent)"));
        cx.subscribe_in(&pick_note, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.send_picked(window, cx);
            }
        })
        .detach();
        let picker_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&picker_input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.apply_picker(window, cx);
            }
        })
        .detach();

        Self {
            owner,
            user: None,
            task_root: None,
            whose: SharedString::default(),
            passkey: false,
            unveiled: false,
            bare: false,
            target: None,
            major: String::new(),
            devtools: None,
            devtools_wanted: false,
            dock_right: false,
            dock_size: 0.4,
            dock_area: Default::default(),
            resizing: false,
            caret: None,
            out,
            asleep: false,
            url_input,
            url_cold: false,
            places: None,
            offered: Vec::new(),
            offering: false,
            _offer: Task::ready(()),
            url: url.into(),
            page_title: SharedString::default(),
            loading,
            port: None,
            frame: None,
            error: None,
            focus: cx.focus_handle(),
            bounds: Default::default(),
            sent_size: Default::default(),
            layout: std::rc::Rc::new(std::cell::Cell::new((1., 0.))),
            device: None,
            turned: false,
            scheme: Scheme::AsTheIde,
            sent_media: None,
            marked: None,
            visible: true,
            zoom: 1.,
            cursor: CursorStyle::Arrow,
            pressed: 0,
            picking: false,
            hover: None,
            asking: false,
            picked: Vec::new(),
            pick_note,
            drag: None,
            picks_waiting: 0,
            picker: None,
            picker_input,
            picker_wrong: false,
            dialog: None,
            prompt_input,
            select: None,
            select_ix: None,
            problems: Vec::new(),
            problems_open: false,
            find_input,
            finding: false,
            failed: None,
            _wait: Task::ready(()),
            burst: 0,
            last_frame: None,
            _frames: frames,
            frames_taken: 0,
            last_jpeg: None,
            _probe: Task::ready(()),
            _sharpen: Task::ready(()),
        }
    }

    /// Closes the page, keeping its address: a task long out of view keeps
    /// no browser running for it. Drawn again, the tab opens it anew.
    pub fn sleep(&mut self, cx: &mut Context<Self>) {
        if self.asleep {
            return;
        }
        let _ = self.out.send(Out::Close);
        // What is sent until it wakes goes nowhere.
        self.out = mpsc::channel().0;
        self._frames = Task::ready(());
        self._sharpen = Task::ready(());
        self._probe = Task::ready(());
        self._wait = Task::ready(());
        self.last_jpeg = None;
        self.asleep = true;
        self.loading = false;
        // Its DevTools was of a page that is gone; it opens again with the page.
        self.devtools = None;
        (self.dialog, self.select, self.picker, self.hover, self.drag) = (None, None, None, None, None);
        self.pressed = 0;
        if let Some(old) = self.frame.take() {
            cx.defer(move |cx| cx.drop_image(old, None));
        }
        cx.notify();
    }

    fn wake(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.asleep = false;
        let url = if self.url.is_empty() { "about:blank".to_string() } else { self.url.to_string() };
        let (out, frames) = Self::attach(Source::Url(url), self.owner.clone(), window, cx);
        (self.out, self._frames) = (out, frames);
        self.sent_size.set((0, 0, 0, 0));
        self.sent_media = None;
        self.loading = true;
    }

    fn take_frame(&mut self, image: Option<Arc<RenderImage>>, jpeg: Arc<[u8]>, ack: i64, cx: &mut Context<Self>) {
        let _ = self.out.send(Out::Ack(ack));
        let Some(image) = image else { return };
        // One sent before the page was told to stop.
        if !self.visible {
            return drop_later(image, cx);
        }
        self.frames_taken += 1;
        self.last_jpeg = Some(jpeg);
        let close = self.last_frame.is_some_and(|at| at.elapsed() < Duration::from_millis(300));
        self.burst = if close { self.burst + 1 } else { 1 };
        self.last_frame = Some(Instant::now());
        self.error = None;
        self.show_frame(image, cx);
        self.sharpen_later(cx);
    }

    fn take(&mut self, message: In, window: &mut Window, cx: &mut Context<Self>) {
        match message {
            In::Frame(..) => {}
            In::Ready { port, target, major } => {
                // The user's Chrome has none: no DevTools served from one.
                if let Some(root) = &self.task_root {
                    note_task_page(&self.owner, &target, root);
                }
                (self.port, self.target, self.major) = ((port != 0).then_some(port), Some(target), major);
                // A device's user agent is told with the size.
                self.sent_size.set((0, 0, 0, 0));
                if self.devtools_wanted {
                    self.open_devtools(window, cx);
                }
            }
            In::Caret(at) => self.caret = Some(at),
            In::Navigated { url, failed } => {
                self.problems.clear();
                self.passkey = false;
                (self.select, self.picker) = (None, None);
                self.error = None;
                self.failed = failed.then(|| url.clone());
                // A picture of the page before is not shown for this one.
                self.last_jpeg = None;
                self.wait_for_server(cx);
                self.set_url(url, window, cx);
            }
            In::Url(url) => self.set_url(url, window, cx),
            In::Title(title) => {
                self.page_title = title.into();
                cx.emit(BrowserEvent::Navigated);
            }
            In::Loading(loading) => {
                // Told at the load event and at the main frame's stop: probed once.
                let was = std::mem::replace(&mut self.loading, loading);
                if was && !loading {
                    self.probe(cx);
                }
            }
            In::Dialog(dialog) => {
                if dialog.kind == "prompt" {
                    self.prompt_input.update(cx, |input, cx| {
                        input.set_value(String::new(), window, cx);
                        input.focus(window, cx);
                    });
                } else if self.visible {
                    self.focus.focus(window, cx);
                }
                self.dialog = Some(dialog);
                self._probe = Task::ready(());
            }
            In::DialogClosed => self.dialog = None,
            In::FileChooser { node, multiple } => {
                let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple, prompt: None });
                let out = self.out.clone();
                cx.spawn(async move |_, _| {
                    if let Ok(Ok(Some(paths))) = picked.await {
                        let files: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                        let _ = out.send(Out::Command("DOM.setFileInputFiles", json!({ "files": files, "backendNodeId": node })));
                    }
                })
                .detach();
            }
            In::Cursor(css) => self.cursor = cursor_style(&css),
            In::Select(menu) => {
                self.select_ix = menu.selected;
                self.select = Some(menu);
                self.focus.focus(window, cx);
            }
            In::Picker(picker) => {
                self.picker_wrong = false;
                self.picker_input.update(cx, |input, cx| {
                    input.set_placeholder(picker_format(&picker.kind), window, cx);
                    input.set_value(picker.value.clone(), window, cx);
                    input.focus(window, cx);
                    input.select_all(window, cx);
                });
                self.picker = Some(picker);
            }
            In::Passkey => self.passkey = true,
            In::Problem(text) => {
                // The latest hundred: a page that throws on every frame says nothing new after.
                if self.problems.len() >= 100 {
                    self.problems.remove(0);
                }
                self.problems.push(text);
            }
            In::Closed => cx.emit(BrowserEvent::Closed),
            In::Error(e) => self.error = Some(e),
        }
    }

    fn set_url(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if url == self.url.as_ref() {
            return;
        }
        self.url = url.clone().into();
        // Not while you type there.
        if !self.url_input.focus_handle(cx).is_focused(window) {
            self.url_input.update(cx, |input, cx| input.set_value(url, window, cx));
        }
        cx.emit(BrowserEvent::Navigated);
    }

    /// The page's title, else its address without the scheme.
    pub fn title(&self) -> SharedString {
        let url = self.url.as_ref();
        let bare = url.split_once("://").map_or(url, |(_, rest)| rest).trim_end_matches('/');
        let title = self.page_title.trim();
        if !title.is_empty() && title != url && title != bare && self.failed.is_none() {
            return title.to_string().into();
        }
        bare.to_string().into()
    }

    /// Out of view the page stops sending frames and the last one is let
    /// go: megabytes of pixels per tab. Back in view, drawing the page area
    /// starts the frames again.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.visible, visible) == visible {
            return;
        }
        let _ = self.out.send(Out::Visible(visible));
        if let Some(devtools) = &self.devtools {
            devtools.update(cx, |d, cx| d.set_visible(visible, cx));
        }
        self.wait_for_server(cx);
        if visible {
            self.show_last(cx);
            self.probe(cx);
        } else {
            self.sent_size.set((0, 0, 0, 0));
            self._sharpen = Task::ready(());
            self._probe = Task::ready(());
            self.last_frame = None;
            if let Some(old) = self.frame.take() {
                // Once this update is over: a window in the middle of one is
                // passed over, and its texture of the frame would stay.
                cx.defer(move |cx| cx.drop_image(old, None));
            }
        }
        cx.notify();
    }

    /// A page of this machine that could not be had is loaded again once
    /// something listens at its port: a dev server that was still starting.
    fn wait_for_server(&mut self, cx: &mut Context<Self>) {
        self._wait = Task::ready(());
        let Some(url) = self.failed.clone().filter(|_| self.visible) else {
            return;
        };
        let Some(address) = local_address(&url) else {
            return;
        };
        self._wait = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let address = address.clone();
                let up = cx
                    .background_spawn(async move {
                        address.to_socket_addrs().into_iter().flatten().any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok())
                    })
                    .await;
                if up {
                    let _ = this.update(cx, |this, _| {
                        if this.failed.as_deref() == Some(url.as_str()) {
                            this.go(&url);
                        }
                    });
                    return;
                }
            }
        });
    }

    fn show_frame(&mut self, image: Arc<RenderImage>, cx: &mut Context<Self>) {
        if let Some(old) = self.frame.replace(image) {
            drop_later(old, cx);
        }
        cx.notify();
    }

    /// The last frame, kept compressed while out of view, drawn again until
    /// the stream sends a new one.
    fn show_last(&mut self, cx: &mut Context<Self>) {
        let Some(jpeg) = self.last_jpeg.clone().filter(|_| self.frame.is_none()) else {
            return;
        };
        let taken = self.frames_taken;
        cx.spawn(async move |this, cx| {
            let Some(image) = cx.background_spawn(async move { decode(&jpeg, image::ImageFormat::Jpeg) }).await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.visible && this.frame.is_none() && this.frames_taken == taken {
                    this.show_frame(image, cx)
                } else {
                    drop_later(image, cx)
                }
            });
        })
        .detach();
    }

    /// When no frame comes for a while after the page loads or comes back
    /// in view, a picture of it is asked for and shown: it is what the page
    /// is now, and drawing it gets the stream going again.
    fn probe(&mut self, cx: &mut Context<Self>) {
        if !self.visible || self.asleep || self.dialog.is_some() {
            self._probe = Task::ready(());
            return;
        }
        let taken = self.frames_taken;
        let out = self.out.clone();
        self._probe = cx.spawn(async move |this, cx| {
            for wait in [400, 1200] {
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                // A page with a dialog open answers no picture.
                let quiet = |this: &mut Self| this.visible && !this.asleep && this.dialog.is_none() && this.frames_taken == taken;
                if !this.update(cx, |this, _| quiet(this)).unwrap_or(false) {
                    return;
                }
                let out = out.clone();
                let image = cx
                    .background_spawn(async move {
                        let result = call(&out, "Page.captureScreenshot", json!({ "format": "png" }))?;
                        let png = base64::engine::general_purpose::STANDARD.decode(result["data"].as_str()?).ok()?;
                        decode(&png, image::ImageFormat::Png)
                    })
                    .await;
                if let Some(image) = image {
                    let _ = this.update(cx, |this, cx| if quiet(this) { this.show_frame(image, cx) } else { drop_later(image, cx) });
                    return;
                }
            }
        });
    }

    /// After 300 ms without new frames, replace the last JPEG with a PNG.
    /// Not after a frame alone: a caret blinking sends one twice a second.
    fn sharpen_later(&mut self, cx: &mut Context<Self>) {
        if self.burst < 2 {
            self._sharpen = Task::ready(());
            return;
        }
        let out = self.out.clone();
        let taken = self.frames_taken;
        self._sharpen = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let image = cx
                .background_spawn(async move {
                    let result = call(&out, "Page.captureScreenshot", json!({ "format": "png" }))?;
                    let png = base64::engine::general_purpose::STANDARD.decode(result["data"].as_str()?).ok()?;
                    decode(&png, image::ImageFormat::Png)
                })
                .await;
            if let Some(image) = image {
                let _ = this.update(cx, |this, cx| if this.visible && this.frames_taken == taken { this.show_frame(image, cx) } else { drop_later(image, cx) });
            }
        });
    }

    /// What `expression` gives in the page, from the CDP thread: waited
    /// for off the UI thread.
    fn eval_string(&self, expression: &str, cx: &mut Context<Self>) -> Task<Option<String>> {
        let (tx, rx) = mpsc::channel();
        let sent = self.out.send(Out::Call("Runtime.evaluate", json!({ "expression": expression, "returnByValue": true }), tx));
        cx.background_spawn(async move {
            sent.ok()?;
            let result = rx.recv_timeout(Duration::from_secs(2)).ok()?;
            result["result"]["value"].as_str().map(str::to_string)
        })
    }

    fn set_picking(&mut self, on: bool, cx: &mut Context<Self>) {
        self.picking = on;
        self.hover = None;
        cx.notify();
    }

    /// Frames the element under the pointer; one question to the page at a time.
    fn hover_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.asking {
            return;
        }
        self.asking = true;
        let (x, y) = self.pointer(position);
        let out = self.out.clone();
        cx.spawn(async move |this, cx| {
            let found = cx.background_spawn(async move { element_box(&out, x, y) }).await;
            let _ = this.update(cx, |this, cx| {
                this.asking = false;
                if this.picking {
                    this.hover = found;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn pick_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (x, y) = self.pointer(position);
        // A page of the user's: what is seen of it, not its markup, where a token may be.
        if self.user.is_some() {
            return self.run_pick(cx, move |out, dir| pick_seen(&out, x, y, &dir));
        }
        self.run_pick(cx, move |out, dir| pick(&out, x, y, &dir));
    }

    /// One pick, made off the UI thread and added to the others.
    fn run_pick(&mut self, cx: &mut Context<Self>, job: impl FnOnce(mpsc::Sender<Out>, PathBuf) -> Option<Picked> + Send + 'static) {
        self.picks_waiting += 1;
        let out = self.out.clone();
        let dir = crate::state::runtime_dir().join("picked");
        cx.spawn(async move |this, cx| {
            let picked = cx.background_spawn(async move { job(out, dir) }).await;
            let _ = this.update(cx, |this, cx| {
                this.picks_waiting -= 1;
                this.picked.extend(picked);
                // A picture of the whole page lays it out at its full height for a moment, the tab's size forgotten.
                this.sent_size.set((0, 0, 0, 0));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// What is picked, with what the user says of it, into the agent's prompt.
    fn send_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picked.is_empty() {
            return;
        }
        let note = self.pick_note.read(cx).value().trim().to_string();
        let mut message = String::new();
        if !note.is_empty() {
            message.push_str(&note);
            message.push_str("\n\n");
        }
        message.push_str(&format!("On {}:\n", self.told_url()));
        let many = self.picked.len() > 1;
        for (i, picked) in self.picked.drain(..).enumerate() {
            if many {
                message.push_str(&format!("{}. ", i + 1));
            }
            message.push_str(&picked.text);
        }
        self.drop_picks(window, cx);
        cx.emit(BrowserEvent::ToAgent(message));
    }

    fn drop_picks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picked.clear();
        self.drag = None;
        self.pick_note.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        self.set_picking(false, cx);
        self.focus.focus(window, cx);
    }

    /// The value typed for a date or color field, into it as the page would
    /// have it from its picker.
    fn apply_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.clone() else { return };
        let value = self.picker_input.read(cx).value().trim().to_string();
        let expression = format!(
            "((v) => {{ const e = window.__ideaePicker; if (!e) return 'gone'; \
             Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(e, v); \
             if (v !== '' && e.value.toLowerCase() !== v.toLowerCase()) return 'wrong'; \
             e.dispatchEvent(new Event('input', {{ bubbles: true }})); e.dispatchEvent(new Event('change', {{ bubbles: true }})); return 'ok'; }})({})",
            json!(value)
        );
        let (tx, rx) = mpsc::channel();
        let _ = self.out.send(Out::Call("Runtime.evaluate", json!({ "expression": expression, "contextId": picker.context, "returnByValue": true }), tx));
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx.background_spawn(async move { rx.recv_timeout(Duration::from_secs(2)).ok() }).await;
            let answer = answer.and_then(|a| a["result"]["value"].as_str().map(str::to_string)).unwrap_or_default();
            let _ = this.update_in(cx, |this, window, cx| {
                if answer == "wrong" {
                    this.picker_wrong = true;
                } else {
                    this.picker = None;
                    this.focus.focus(window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Today, or now, as the field's type writes it, in the page's time.
    fn picker_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &self.picker else { return };
        let expression = format!(
            r#"((k) => {{ const d = new Date(), p = (n) => String(n).padStart(2, "0");
              const date = d.getFullYear() + "-" + p(d.getMonth() + 1) + "-" + p(d.getDate()), time = p(d.getHours()) + ":" + p(d.getMinutes());
              if (k === "date") return date; if (k === "time") return time; if (k === "datetime-local") return date + "T" + time;
              if (k === "month") return date.slice(0, 7);
              const t = new Date(Date.UTC(d.getFullYear(), d.getMonth(), d.getDate())); t.setUTCDate(t.getUTCDate() + 4 - (t.getUTCDay() || 7));
              return t.getUTCFullYear() + "-W" + p(Math.ceil(((t - Date.UTC(t.getUTCFullYear(), 0, 1)) / 86400000 + 1) / 7)); }})({})"#,
            json!(picker.kind)
        );
        let now = self.eval_string(&expression, cx);
        cx.spawn_in(window, async move |this, cx| {
            if let Some(now) = now.await {
                let _ = this.update_in(cx, |this, window, cx| this.picker_input.update(cx, |input, cx| input.set_value(now, window, cx)));
            }
        })
        .detach();
    }

    fn toggle_devtools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.devtools.take().is_some() {
            self.devtools_wanted = false;
            self.focus.focus(window, cx);
        } else {
            self.devtools_wanted = true;
            self.open_devtools(window, cx);
        }
        cx.notify();
    }

    /// Chrome's own DevTools for the page, served by the task's Chrome and
    /// shown in a panel of the tab; the page gives it room.
    fn open_devtools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        if self.devtools.is_some() {
            return;
        }
        // The task's Chrome serves DevTools at its port; the user's has none, and they come from inside it.
        let source = match self.port {
            Some(port) => Source::Url(format!("http://127.0.0.1:{port}/devtools/inspector.html?ws=127.0.0.1:{port}/devtools/page/{target}")),
            None if self.user.is_some() => Source::DevTools(target),
            None => return,
        };
        let owner = self.owner.clone();
        let scheme = self.scheme;
        let view = cx.new(|cx| {
            let mut view = BrowserView::new(source, owner, window, cx);
            view.bare = true;
            view.scheme = scheme;
            view
        });
        cx.subscribe(&view, |this, _, event: &BrowserEvent, cx| {
            if matches!(event, BrowserEvent::Closed) {
                this.devtools = None;
                cx.notify();
            }
        })
        .detach();
        self.devtools = Some(view);
    }

    /// How the tab shows its page, for the session: none when as it starts.
    pub fn look(&self) -> Option<crate::state::WebLook> {
        let look = crate::state::WebLook {
            device: self.device.map(|i| DEVICES[i].0.to_string()),
            turned: self.turned,
            colors: match self.scheme {
                Scheme::AsTheIde => String::new(),
                Scheme::Light => "light".into(),
                Scheme::Dark => "dark".into(),
            },
            devtools: match (self.devtools_wanted, self.dock_right) {
                (false, _) => String::new(),
                (true, false) => "bottom".into(),
                (true, true) => "right".into(),
            },
        };
        (look != crate::state::WebLook::default()).then_some(look)
    }

    pub fn set_look(&mut self, look: &crate::state::WebLook, window: &mut Window, cx: &mut Context<Self>) {
        self.device = look.device.as_deref().and_then(|name| DEVICES.iter().position(|d| d.0 == name));
        self.turned = look.turned && self.device.is_some();
        self.scheme = match look.colors.as_str() {
            "light" => Scheme::Light,
            "dark" => Scheme::Dark,
            _ => Scheme::AsTheIde,
        };
        self.dock_right = look.devtools == "right";
        if !look.devtools.is_empty() {
            self.devtools_wanted = true;
            self.open_devtools(window, cx);
        }
        self.relayout(cx);
    }

    /// The divider dragged to `at`: the panel takes what is right of or below it.
    fn resize_dock(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let area = self.dock_area.get();
        let share = if self.dock_right {
            f32::from(area.right() - at.x) / f32::from(area.size.width).max(1.)
        } else {
            f32::from(area.bottom() - at.y) / f32::from(area.size.height).max(1.)
        };
        self.dock_size = share.clamp(0.15, 0.85);
        cx.notify();
    }

    fn device_size(&self) -> Option<(u32, u32, bool)> {
        self.device.map(|i| {
            let (_, w, h, mobile) = DEVICES[i];
            if self.turned { (h, w, mobile) } else { (w, h, mobile) }
        })
    }

    fn set_device(&mut self, device: Option<usize>, cx: &mut Context<Self>) {
        self.device = device;
        self.turned = false;
        self.relayout(cx);
    }

    /// The page is told its new size at the next frame; what was placed
    /// over it goes.
    fn relayout(&mut self, cx: &mut Context<Self>) {
        self.sent_size.set((0, 0, 0, 0));
        (self.hover, self.select, self.picker, self.drag) = (None, None, None, None);
        cx.notify();
    }

    /// The page's errors into the agent's prompt.
    fn problems_to_agent(&mut self, cx: &mut Context<Self>) {
        let mut message = format!("Errors on {}:\n", self.url);
        for problem in &self.problems {
            let lines: Vec<&str> = problem.lines().take(12).collect();
            let entry = format!("- {}\n", lines.join("\n  "));
            if message.len() + entry.len() > 8000 {
                message.push_str("- …\n");
                break;
            }
            message.push_str(&entry);
        }
        self.problems_open = false;
        cx.emit(BrowserEvent::ToAgent(message));
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let selected = self.eval_string(SELECTED, cx);
        cx.spawn(async move |_, cx| {
            if let Some(text) = selected.await.filter(|t| !t.is_empty()) {
                let _ = cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
            }
        })
        .detach();
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.command("Input.insertText", json!({ "text": text }));
        }
    }

    /// One of Chrome's editing commands with the Cmd key that does it on a
    /// Mac: the page sees the key and may do its own, else Chrome does this.
    fn edit(&self, command: &str, letter: char, shift: bool) {
        let c = letter.to_ascii_uppercase();
        let key = if shift { c.to_string() } else { letter.to_string() };
        let modifiers = 4 | if shift { 8 } else { 0 };
        let event =
            |kind: &str| json!({ "type": kind, "key": key, "code": format!("Key{c}"), "windowsVirtualKeyCode": c as u32, "modifiers": modifiers });
        let mut down = event("rawKeyDown");
        down["commands"] = json!([command]);
        self.command("Input.dispatchKeyEvent", down);
        self.command("Input.dispatchKeyEvent", event("keyUp"));
    }

    /// Copies the selection, and takes it out of the page only once it is
    /// on the clipboard.
    fn cut(&mut self, cx: &mut Context<Self>) {
        let selected = self.eval_string(SELECTED, cx);
        cx.spawn(async move |this, cx| {
            if let Some(text) = selected.await.filter(|t| !t.is_empty()) {
                let _ = this.update(cx, |this, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    this.edit("delete", 'x', false);
                });
            }
        })
        .detach();
    }

    fn command(&self, method: &'static str, params: Value) {
        let _ = self.out.send(Out::Command(method, params));
    }

    fn back(&self) {
        self.command("Runtime.evaluate", json!({ "expression": "history.back()" }));
    }

    fn forward(&self) {
        self.command("Runtime.evaluate", json!({ "expression": "history.forward()" }));
    }

    fn reload(&mut self, hard: bool) {
        // An error page reloads what it could not show.
        match self.failed.clone() {
            Some(url) => self.go(&url),
            None => self.command("Page.reload", json!({ "ignoreCache": hard })),
        }
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.zoom = zoom.clamp(ZOOMS[0], ZOOMS[ZOOMS.len() - 1]);
        self.relayout(cx);
    }

    fn zoom_step(&mut self, up: bool, cx: &mut Context<Self>) {
        let next =
            if up { ZOOMS.iter().copied().find(|z| *z > self.zoom + 0.001) } else { ZOOMS.iter().rev().copied().find(|z| *z < self.zoom - 0.001) };
        if let Some(zoom) = next {
            self.set_zoom(zoom, cx);
        }
    }

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finding = true;
        self.find_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finding = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn find_next(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let text = self.find_input.read(cx).value().to_string();
        if text.is_empty() {
            return;
        }
        let expression = format!("window.find({}, false, {backwards}, true, false, false, false)", json!(text));
        self.command("Runtime.evaluate", json!({ "expression": expression }));
    }

    /// The page's dialog answered: OK (with the prompt's text) or Cancel.
    fn answer(&mut self, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        let mut params = json!({ "accept": accept });
        if dialog.kind == "prompt" {
            params["promptText"] = json!(self.prompt_input.read(cx).value().to_string());
        }
        self.command("Page.handleJavaScriptDialog", params);
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn choose(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(menu) = self.select.take() else {
            return;
        };
        if menu.choices.get(ix).is_some_and(|c| !c.disabled) {
            let expression = format!(
                "((i) => {{ const s = window.__ideaeSelect; if (!s || s.selectedIndex === i) return; s.selectedIndex = i; \
                 s.dispatchEvent(new Event('input', {{ bubbles: true }})); s.dispatchEvent(new Event('change', {{ bubbles: true }})); }})({ix})"
            );
            self.command("Runtime.evaluate", json!({ "expression": expression, "contextId": menu.context }));
        }
        cx.notify();
    }

    /// The page's coordinates of a point of the window.
    fn pointer(&self, position: Point<Pixels>) -> (f32, f32) {
        let b = self.bounds.get();
        let (factor, offset) = self.layout.get();
        ((f32::from(position.x - b.origin.x) - offset) / factor, f32::from(position.y - b.origin.y) / factor)
    }

    /// A box of the page's, in CSS pixels, where it is drawn in the page area.
    fn on_screen(&self, b: Bounds<Pixels>) -> Bounds<Pixels> {
        let (factor, offset) = fit(self.bounds.get().size, self.device_size(), self.zoom);
        Bounds { origin: point(b.origin.x * factor + px(offset), b.origin.y * factor), size: size(b.size.width * factor, b.size.height * factor) }
    }

    fn mouse(&self, kind: &str, position: Point<Pixels>, button: Option<MouseButton>, clicks: usize, modifiers: &Modifiers) {
        let (x, y) = self.pointer(position);
        let button = match button {
            Some(MouseButton::Left) => "left",
            Some(MouseButton::Right) => "right",
            Some(MouseButton::Middle) => "middle",
            _ => "none",
        };
        let params = json!({
            "type": kind, "x": x, "y": y, "button": button, "buttons": self.pressed,
            "clickCount": clicks, "modifiers": cdp_modifiers(modifiers),
        });
        let _ = self.out.send(if kind == "mouseMoved" { Out::Move(params) } else { Out::Command("Input.dispatchMouseEvent", params) });
    }

    fn press(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        // A click beside the list closes it, as a browser's would.
        if self.select.take().is_some() || self.picker.take().is_some() {
            return cx.notify();
        }
        if self.picking && e.button == MouseButton::Left {
            // An element where it is let go near here, else the area dragged over.
            self.drag = Some((e.position, e.position));
            return;
        }
        self.pressed |= button_bit(e.button);
        self.mouse("mousePressed", e.position, Some(e.button), e.click_count, &e.modifiers);
    }

    fn release(&mut self, e: &MouseUpEvent) {
        let bit = button_bit(e.button);
        if self.pressed & bit == 0 {
            return;
        }
        self.pressed &= !bit;
        self.mouse("mouseReleased", e.position, Some(e.button), e.click_count, &e.modifiers);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = &keystroke.modifiers;
        let key = keystroke.key.as_str();
        if self.picking && key == "escape" {
            return self.set_picking(false, cx);
        }
        if self.dialog.is_some() {
            match key {
                "enter" => self.answer(true, window, cx),
                "escape" => self.answer(false, window, cx),
                _ => {}
            }
            return;
        }
        if let Some(menu) = &self.select {
            let enabled: Vec<usize> = (0..menu.choices.len()).filter(|i| !menu.choices[*i].disabled).collect();
            let at = self.select_ix.and_then(|ix| enabled.iter().position(|i| *i == ix));
            match key {
                "escape" => self.select = None,
                "enter" | "space" => {
                    if let Some(ix) = self.select_ix {
                        self.choose(ix, cx);
                    }
                }
                "down" => self.select_ix = enabled.get(at.map_or(0, |a| (a + 1).min(enabled.len().saturating_sub(1)))).copied(),
                "up" => self.select_ix = enabled.get(at.map_or(0, |a| a.saturating_sub(1))).copied(),
                _ => {}
            }
            return cx.notify();
        }
        let cmd = cfg!(target_os = "macos") && m.platform;
        if (m.control || cmd) && !m.alt && !m.shift {
            match key {
                "c" => return self.copy(cx),
                "v" => return self.paste(cx),
                _ => {}
            }
        }
        // Cmd on a Mac: Chrome is told what to do, it does not know the key.
        if cmd && !m.alt && !m.control {
            match (key, m.shift) {
                ("x", false) => return self.cut(cx),
                ("a", false) => return self.edit("selectAll", 'a', false),
                ("z", false) => return self.edit("undo", 'z', false),
                ("z", true) => return self.edit("redo", 'z', true),
                _ => {}
            }
        }
        if key == "f5" && !m.control && !m.alt && !m.platform {
            return self.reload(m.shift);
        }
        let modifiers = cdp_modifiers(m);
        let shortcut = m.control || m.platform || (m.alt && !event.prefer_character_input);
        let send = |kind: &str, key: &str, code: &str, vk: u32, text: Option<&str>| {
            let mut e =
                json!({ "type": kind, "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers, "autoRepeat": event.is_held });
            if let Some(text) = text {
                e["text"] = json!(text);
                e["unmodifiedText"] = json!(text);
            }
            self.command("Input.dispatchKeyEvent", e);
        };
        // A key that types: the page sees it pressed, typed and let go.
        if let (Some(text), false) = (keystroke.key_char.as_deref(), shortcut) {
            let (code, vk) = key_code(key.chars().next().filter(|_| key.chars().count() == 1).unwrap_or_else(|| text.chars().next().unwrap_or(' ')));
            send("keyDown", text, &code, vk, Some(text));
            send("keyUp", text, &code, vk, None);
            return;
        }
        let named = match key {
            "enter" => Some(("Enter", 13)),
            "backspace" => Some(("Backspace", 8)),
            "tab" => Some(("Tab", 9)),
            "escape" => Some(("Escape", 27)),
            "delete" => Some(("Delete", 46)),
            "insert" => Some(("Insert", 45)),
            "left" => Some(("ArrowLeft", 37)),
            "up" => Some(("ArrowUp", 38)),
            "right" => Some(("ArrowRight", 39)),
            "down" => Some(("ArrowDown", 40)),
            "home" => Some(("Home", 36)),
            "end" => Some(("End", 35)),
            "pageup" => Some(("PageUp", 33)),
            "pagedown" => Some(("PageDown", 34)),
            "space" => Some(("Space", 32)),
            _ => None,
        };
        if let Some((name, vk)) = named {
            // Enter types a line break and sends a form, as the key does.
            let text = (name == "Enter" && !shortcut).then_some("\r");
            let (dom_key, code) = if name == "Space" { (" ", "Space") } else { (name, name) };
            send(if text.is_some() { "keyDown" } else { "rawKeyDown" }, dom_key, code, vk, text);
            send("keyUp", dom_key, code, vk, None);
            return;
        }
        if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=12).contains(n)) {
            let name = format!("F{n}");
            send("rawKeyDown", &name, &name, 111 + n, None);
            send("keyUp", &name, &name, 111 + n, None);
            return;
        }
        if let Some(c) = key.chars().next().filter(|_| key.chars().count() == 1) {
            // A shortcut such as Ctrl+A: the key with its modifiers.
            let (code, vk) = key_code(c);
            send("rawKeyDown", key, &code, vk, None);
            send("keyUp", key, &code, vk, None);
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let problems = self.problems.len();
        let lit = cx.theme().foreground;
        h_flex()
            .gap_1()
            .items_center()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .when_some(self.user.as_ref(), |bar, mark| {
                let told = self.whose.clone();
                bar.child(
                    div()
                        .id("whose-browser")
                        .flex_none()
                        .child(crate::workspace::spaces::chip(&mark.mark, mark.color))
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(told.clone()).build(window, cx)),
                )
            })
            .child(Button::new("back").xsmall().ghost().child(drawn("arrow-left", muted)).tooltip(crate::keymap::tip("Back", "web_back")).on_click(crate::learn::clicked("web_back", cx.listener(|this, _, _, _| this.back()))))
            .child(Button::new("forward").xsmall().ghost().child(drawn("arrow-right", muted)).tooltip(crate::keymap::tip("Forward", "web_forward")).on_click(crate::learn::clicked("web_forward", cx.listener(|this, _, _, _| this.forward()))))
            .child(if self.loading {
                Button::new("stop")
                    .xsmall()
                    .ghost()
                    .child(drawn("x", muted))
                    .tooltip("Stop")
                    .on_click(cx.listener(|this, _, _, _| this.command("Page.stopLoading", json!({}))))
            } else {
                Button::new("reload").xsmall().ghost().child(drawn("rotate-cw", muted)).tooltip(crate::keymap::tip("Reload", "web_reload")).on_click(crate::learn::clicked("web_reload", cx.listener(|this, _, _, _| this.reload(false))))
            })
            // As a browser's: a click on it while the keyboard is elsewhere takes the whole
            // address, to type another over; the next click puts the caret where it lands,
            // and a drag from the first is the selection it made.
            .child(
                div()
                    .flex_1()
                    .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.url_cold = !this.url_input.focus_handle(cx).is_focused(window);
                    }))
                    .capture_any_mouse_up(cx.listener(|this, _: &MouseUpEvent, window, cx| {
                        if std::mem::take(&mut this.url_cold) {
                            this.url_input.update(cx, |input, cx| {
                                if input.selected_range().is_empty() {
                                    input.select_all(window, cx);
                                }
                            });
                        }
                    }))
                    .child(Input::new(&self.url_input).small()),
            )
            .when_some(self.failed.as_ref().and_then(|url| local_address(url)), |this, address| {
                this.child(div().text_xs().text_color(muted).child(format!("waiting for {address}…")))
            })
            .child(self.render_device_button(cx))
            .when((self.zoom - 1.).abs() > 0.001, |this| {
                this.child(
                    Button::new("zoom")
                        .xsmall()
                        .ghost()
                        .label(format!("{:.0}%", self.zoom * 100.))
                        .tooltip(crate::keymap::tip("Back to 100%", "web_zoom_reset"))
                        .on_click(crate::learn::clicked("web_zoom_reset", cx.listener(|this, _, _, cx| this.set_zoom(1., cx)))),
                )
            })
            .when(problems > 0, |this| {
                this.child(
                    Button::new("problems")
                        .xsmall()
                        .ghost()
                        .text_color(gpui_kit::red())
                        .child(h_flex().gap_1().child(drawn("triangle-alert", gpui_kit::red())).child(problems.to_string()))
                        .selected(self.problems_open)
                        .tooltip("The page's errors: exceptions, console.error, failed requests")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.problems_open = !this.problems_open;
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("pick")
                    .xsmall()
                    .ghost()
                    .child(drawn("square-dashed-mouse-pointer", if self.picking { lit } else { muted }))
                    .selected(self.picking)
                    .tooltip(if self.user.is_some() {
                        "Pick for the agent what you choose of the page: an element's text and picture, an area, the page's text"
                    } else {
                        "Pick an element for the agent: its selector, markup and picture go into the prompt"
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        // Focused, so Esc gets here to leave picking.
                        this.focus.focus(window, cx);
                        this.set_picking(!this.picking, cx);
                    })),
            )
            .child(
                Button::new("devtools")
                    .xsmall()
                    .ghost()
                    .child(drawn("code-xml", if self.devtools.is_some() { lit } else { muted }))
                    .selected(self.devtools.is_some())
                    .tooltip(format!("{}: in a panel of the tab, the page beside it", crate::keymap::tip("DevTools", "web_devtools")))
                    .on_click(crate::learn::clicked("web_devtools", cx.listener(|this, _, window, cx| this.toggle_devtools(window, cx)))),
            )
            .when(self.user.is_some(), |bar| {
                let url = self.url.to_string();
                let host = host_of(&url).filter(|h| !h.is_empty() && url.starts_with("http"));
                bar.child(
                    Button::new("system-browser")
                        .xsmall()
                        .ghost()
                        .child(drawn("external-link", muted))
                        .tooltip("Open in the system's browser: for a site this one cannot sign you in to (a passkey, a security key, a certificate)")
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                            let once = url.clone();
                            let menu = menu.item(PopupMenuItem::new("Open in the system's browser").on_click(move |_, _, cx| cx.open_url(&once)));
                            let Some(host) = host.clone() else { return menu };
                            let always = url.clone();
                            menu.item(PopupMenuItem::new(format!("Always open {host} there")).on_click(move |_, _, cx| {
                                let mut settings = crate::state::settings();
                                if !settings.system_hosts.contains(&host) {
                                    settings.system_hosts.push(host.clone());
                                }
                                crate::state::set_settings(settings);
                                cx.open_url(&always);
                            }))
                        }),
                )
            })
            .when_some(self.port, |this, port| {
                let endpoint = format!("http://127.0.0.1:{port}");
                this.child(
                    Button::new("cdp")
                        .xsmall()
                        .ghost()
                        .text_color(muted)
                        .label("CDP")
                        .tooltip(format!(
                            "{endpoint}: the task's terminals have it as IDEAE_BROWSER_CDP, for Playwright MCP's --cdp-endpoint. Click to copy"
                        ))
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(endpoint.clone()))),
                )
            })
    }

    /// The page, and DevTools beside or below it when open: the page gives
    /// it room, as in a browser, rather than lying under it.
    fn with_devtools(&self, page: impl IntoElement, cx: &mut Context<Self>) -> AnyElement {
        let Some(devtools) = self.devtools.clone() else { return page.into_any_element() };
        let right = self.dock_right;
        let area = self.dock_area.clone();
        let handle = div()
            .id("devtools-divider")
            .flex_none()
            .bg(cx.theme().border)
            .when(right, |d| d.w(px(4.)).h_full().cursor(CursorStyle::ResizeLeftRight))
            .when(!right, |d| d.h(px(4.)).w_full().cursor(CursorStyle::ResizeUpDown))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.resizing = true;
                cx.stop_propagation();
            }));
        let header = h_flex()
            .flex_none()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().text_xs().text_color(cx.theme().muted_foreground).child("DevTools"))
            .child(Button::new("devtools-dock").xsmall().ghost().label(if right { "Dock below" } else { "Dock right" }).on_click(cx.listener(|this, _, _, cx| {
                this.dock_right = !this.dock_right;
                cx.notify();
            })))
            .child(Button::new("devtools-close").xsmall().ghost().label("×").tooltip("Close DevTools (F12)").on_click(cx.listener(|this, _, window, cx| this.toggle_devtools(window, cx))));
        let panel = v_flex()
            .flex_none()
            .min_w_0()
            .min_h_0()
            .when(right, |d| d.w(relative(self.dock_size)).h_full())
            .when(!right, |d| d.h(relative(self.dock_size)).w_full())
            .child(header)
            .child(div().flex_1().min_h_0().child(devtools));
        // Stretched across, not centered as h_flex would have the page.
        if right { div().flex().flex_row() } else { div().flex().flex_col() }
            .flex_1()
            .min_h_0()
            .min_w_0()
            .relative()
            .child(canvas(move |b, _, _| area.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(page)
            .child(handle)
            .child(panel)
            .into_any_element()
    }

    fn render_device_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let (device, turned, scheme) = (self.device, self.turned, self.scheme);
        // The tab's own size has only the drawing; a device chosen is named after it.
        let label = match self.device_size() {
            Some((w, h, _)) => format!("{} {w}×{h}", DEVICES[device.unwrap_or_default()].0),
            None => String::new(),
        };
        let muted = cx.theme().muted_foreground;
        let dark = match scheme {
            Scheme::AsTheIde => "",
            Scheme::Light => " ☀",
            Scheme::Dark => " ☾",
        };
        Button::new("device").xsmall().ghost().child(h_flex().gap_1().child(drawn("monitor-smartphone", muted)).child(format!("{label}{dark}"))).tooltip("The screen and colors the page is shown with").dropdown_menu_with_anchor(
            Anchor::TopRight,
            move |mut menu, _, _| {
                let to = |d: Option<usize>| {
                    let this = this.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        let _ = this.update(cx, |v, cx| v.set_device(d, cx));
                    }
                };
                menu = menu.item(PopupMenuItem::new("The tab's size").checked(device.is_none()).on_click(to(None)));
                for (i, (name, w, h, _)) in DEVICES.iter().enumerate() {
                    menu = menu.item(PopupMenuItem::new(format!("{name}  {w}×{h}")).checked(device == Some(i)).on_click(to(Some(i))));
                }
                if device.is_some() {
                    let this = this.clone();
                    menu = menu.separator().item(PopupMenuItem::new("Turned").checked(turned).on_click(move |_, _, cx| {
                        let _ = this.update(cx, |v, cx| {
                            v.turned = !v.turned;
                            v.relayout(cx);
                        });
                    }));
                }
                menu = menu.separator();
                for (label, s) in [("Colors as the IDE's", Scheme::AsTheIde), ("Light colors", Scheme::Light), ("Dark colors", Scheme::Dark)] {
                    let this = this.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(scheme == s).on_click(move |_, _, cx| {
                        let _ = this.update(cx, |v, cx| {
                            v.scheme = s;
                            cx.notify();
                        });
                    }));
                }
                menu
            },
        )
    }

    /// What is picked for the agent, a note to it, and sending.
    fn render_picked(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .flex_wrap()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(self.picking && self.picked.is_empty(), |d| d.child(div().text_xs().text_color(muted).child("Click elements, or drag over an area")))
            .children(self.picked.iter().enumerate().map(|(i, picked)| {
                h_flex()
                    .pl_1()
                    .rounded_sm()
                    .bg(cx.theme().secondary)
                    .text_xs()
                    .child(format!("{} {}", i + 1, picked.label))
                    .child(Button::new(("unpick", i)).xsmall().ghost().label("×").on_click(cx.listener(move |this, _, _, cx| {
                        if i < this.picked.len() {
                            this.picked.remove(i);
                        }
                        cx.notify();
                    })))
            }))
            .when(self.picks_waiting > 0, |d| d.child(div().text_xs().text_color(muted).child("…")))
            .child(
                Button::new("pick-page")
                    .xsmall()
                    .ghost()
                    .label("Whole page")
                    .tooltip("A picture of the whole page, what is scrolled out of view too")
                    .on_click(cx.listener(|this, _, _, cx| this.run_pick(cx, |out, dir| pick_page(&out, &dir)))),
            )
            .when(self.user.is_some(), |bar| {
                bar.child(
                    Button::new("pick-text")
                        .xsmall()
                        .ghost()
                        .label("Page text")
                        .tooltip("The page's text as it reads: a ticket, an error, a page of documentation")
                        .on_click(cx.listener(|this, _, _, cx| this.run_pick(cx, |out, _| page_text(&out)))),
                )
            })
            .child(div().flex_1().min_w(px(180.)).child(Input::new(&self.pick_note).small()))
            .child(
                Button::new("pick-send")
                    .xsmall()
                    .primary()
                    .label("Send to agent")
                    .disabled(self.picked.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.send_picked(window, cx))),
            )
            .child(
                Button::new("pick-cancel")
                    .xsmall()
                    .ghost()
                    .label("×")
                    .tooltip("Stop picking and drop what is picked")
                    .on_click(cx.listener(|this, _, window, cx| this.drop_picks(window, cx))),
            )
    }

    /// A date's, time's or color's value asked for where its picker would open.
    fn render_picker(&self, picker: &Picker, cx: &mut Context<Self>) -> impl IntoElement {
        let at = self.on_screen(picker.bounds);
        let area = self.bounds.get().size;
        let height = px(110.);
        let top = if at.bottom() + height > area.height && at.top() > height { at.top() - height } else { at.bottom() };
        let left = at.left().min((area.width - px(280.)).max(px(0.)));
        let color = picker.kind == "color";
        let typed = self.picker_input.read(cx).value();
        let swatch = u32::from_str_radix(typed.trim_start_matches('#'), 16).ok().filter(|_| color && typed.len() == 7);
        v_flex()
            .id("page-picker")
            .absolute()
            .left(left)
            .top(top)
            .w(px(280.))
            .p_2()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" {
                    this.picker = None;
                    this.focus.focus(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{}: {}", picker.kind, picker_format(&picker.kind))))
            .child(
                h_flex()
                    .gap_1()
                    .when_some(swatch, |d, rgb| d.child(div().size(px(20.)).rounded_sm().border_1().border_color(cx.theme().border).bg(gpui_kit::rgb(rgb))))
                    .child(div().flex_1().child(Input::new(&self.picker_input).small())),
            )
            .when(self.picker_wrong, |d| {
                d.child(div().text_xs().text_color(gpui_kit::red()).child(format!("The field takes {}", picker_format(&picker.kind))))
            })
            .child(
                h_flex()
                    .gap_1()
                    .when(!color, |d| {
                        d.child(
                            Button::new("picker-now")
                                .xsmall()
                                .ghost()
                                .label(if picker.kind == "time" { "Now" } else { "Today" })
                                .on_click(cx.listener(|this, _, window, cx| this.picker_now(window, cx))),
                        )
                    })
                    .child(Button::new("picker-clear").xsmall().ghost().label("Clear").on_click(cx.listener(|this, _, window, cx| {
                        this.picker_input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
                        this.apply_picker(window, cx);
                    })))
                    .child(div().flex_1())
                    .child(Button::new("picker-ok").xsmall().primary().label("OK").on_click(cx.listener(|this, _, window, cx| this.apply_picker(window, cx)))),
            )
    }

    fn render_problems(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .child(div().flex_1().text_xs().text_color(cx.theme().muted_foreground).child("The page's errors since it loaded"))
                    .child(
                        Button::new("problems-agent")
                            .xsmall()
                            .outline()
                            .label("Send to agent")
                            .on_click(cx.listener(|this, _, _, cx| this.problems_to_agent(cx))),
                    )
                    .child(Button::new("problems-clear").xsmall().ghost().label("Clear").on_click(cx.listener(|this, _, _, cx| {
                        this.problems.clear();
                        this.problems_open = false;
                        cx.notify();
                    }))),
            )
            .child(
                v_flex()
                    .id("problems")
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .px_2()
                    .pb_1()
                    .gap_1()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .children(self.problems.iter().map(|p| {
                        let shown: String = p.lines().take(6).collect::<Vec<_>>().join("\n");
                        div().text_color(gpui_kit::red()).child(shown)
                    })),
            )
    }

    fn render_find(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" {
                    this.close_find(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(div().w(px(260.)).child(Input::new(&self.find_input).small()))
            .child(
                Button::new("find-prev")
                    .xsmall()
                    .ghost()
                    .label("↑")
                    .tooltip("Previous (Shift+Enter)")
                    .on_click(cx.listener(|this, _, _, cx| this.find_next(true, cx))),
            )
            .child(
                Button::new("find-next")
                    .xsmall()
                    .ghost()
                    .label("↓")
                    .tooltip("Next (Enter)")
                    .on_click(cx.listener(|this, _, _, cx| this.find_next(false, cx))),
            )
            .child(Button::new("find-close").xsmall().ghost().label("×").on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))))
    }

    fn render_dialog(&self, dialog: &Dialog, cx: &mut Context<Self>) -> impl IntoElement {
        let leave = dialog.kind == "beforeunload";
        let (ok, cancel) = if leave {
            ("Leave", Some("Stay"))
        } else if dialog.kind == "alert" {
            ("OK", None)
        } else {
            ("OK", Some("Cancel"))
        };
        let message = if leave && dialog.message.is_empty() {
            "Leave this page? What you changed may not be saved.".to_string()
        } else {
            dialog.message.clone()
        };
        div().absolute().inset_0().bg(gpui_kit::black().opacity(0.25)).flex().items_start().justify_center().pt_8().child(
            v_flex()
                .id("page-dialog")
                .w(px(420.))
                .max_w_full()
                .p_3()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .text_color(cx.theme().popover_foreground)
                .shadow_lg()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} says", self.title())))
                .child(div().id("page-dialog-text").max_h(px(240.)).overflow_y_scroll().text_sm().child(message))
                .when(dialog.kind == "prompt", |this| this.child(Input::new(&self.prompt_input).small()))
                .child(
                    h_flex()
                        .justify_end()
                        .gap_1()
                        .when_some(cancel, |this, cancel| {
                            this.child(
                                Button::new("dialog-cancel")
                                    .small()
                                    .ghost()
                                    .label(cancel)
                                    .on_click(cx.listener(|this, _, window, cx| this.answer(false, window, cx))),
                            )
                        })
                        .child(
                            Button::new("dialog-ok")
                                .small()
                                .primary()
                                .label(ok)
                                .on_click(cx.listener(|this, _, window, cx| this.answer(true, window, cx))),
                        ),
                ),
        )
    }

    fn render_select(&self, menu: &SelectMenu, cx: &mut Context<Self>) -> impl IntoElement {
        let at = self.on_screen(menu.bounds);
        let area = self.bounds.get().size;
        let (left, top, width) = (at.left(), at.bottom(), at.size.width);
        let rows = menu.choices.len() + menu.choices.iter().filter_map(|c| c.group.as_ref()).collect::<HashSet<_>>().len();
        let height = px(26. * rows.min(12) as f32 + 8.);
        // Above the select when there is no room under it.
        let top = if top + height > area.height && at.top() > height { at.top() - height } else { top };
        let mut list = v_flex()
            .id("page-select")
            .absolute()
            .left(left)
            .top(top)
            .min_w(width.max(px(120.)))
            .max_h(height)
            .overflow_y_scroll()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .shadow_lg()
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation());
        let mut group: Option<&String> = None;
        for (ix, choice) in menu.choices.iter().enumerate() {
            if choice.group.as_ref() != group {
                group = choice.group.as_ref();
                if let Some(label) = group {
                    list = list.child(
                        div().px_2().pt_1().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(cx.theme().muted_foreground).child(label.clone()),
                    );
                }
            }
            let current = self.select_ix == Some(ix);
            list = list.child(
                div()
                    .id(("page-select-choice", ix))
                    .px_2()
                    .when(group.is_some(), |d| d.pl_4())
                    .py(px(3.))
                    .whitespace_nowrap()
                    .when(current, |d| d.bg(cx.theme().accent).text_color(cx.theme().accent_foreground))
                    .when(choice.disabled, |d| d.text_color(cx.theme().muted_foreground))
                    .when(!choice.disabled, |d| d.hover(|d| d.bg(cx.theme().accent)).on_click(cx.listener(move |this, _, _, cx| this.choose(ix, cx))))
                    .child(if choice.label.is_empty() { "\u{a0}".to_string() } else { choice.label.clone() }),
            );
        }
        list
    }
}

/// How the page sits in an area: its viewport in CSS pixels, window pixels
/// to each, how far right it is drawn, and whether it is a phone's. The
/// tab's own size scaled by the zoom; or a screen's, made smaller to fit
/// and centered.
fn fit(area: Size<Pixels>, device: Option<(u32, u32, bool)>, zoom: f32) -> (f32, f32) {
    let (_, _, factor, offset, _) = fit_all(area, device, zoom);
    (factor, offset)
}

fn fit_all(area: Size<Pixels>, device: Option<(u32, u32, bool)>, zoom: f32) -> (u32, u32, f32, f32, bool) {
    let (aw, ah) = (f32::from(area.width).max(1.), f32::from(area.height).max(1.));
    match device {
        None => ((aw / zoom).round() as u32, (ah / zoom).round() as u32, zoom, 0., false),
        Some((w, h, mobile)) => {
            let factor = zoom.min(aw / w as f32).min(ah / h as f32);
            (w, h, factor, ((aw - w as f32 * factor) / 2.).max(0.), mobile)
        }
    }
}

/// Lets go of a frame's texture once the update under way is over. Not at
/// once: while a window is updated it is out of the app's list, and
/// `drop_image` passes over it; every frame of a page in motion (they come
/// in such an update) then kept its texture in the window's atlas for good,
/// megabytes each.
fn drop_later<T: 'static>(image: Arc<RenderImage>, cx: &mut Context<T>) {
    cx.defer(move |cx| cx.drop_image(image, None));
}

/// One of the toolbar's drawings (Lucide's, assets/side-icons), in a color.
fn drawn(name: &'static str, color: Hsla) -> Svg {
    svg().path(SharedString::from(format!("side-icons/{name}.svg"))).size(px(15.)).flex_none().text_color(color)
}

/// How large a frame is drawn, in window pixels: as wide as the page's
/// place, and as high as that makes it. A frame always shows the whole
/// viewport, so its own pixels tell its shape and nothing of its size;
/// while a resize waits for its first frame the old one is a little larger
/// or smaller for a moment, never out of shape.
fn frame_drawn(place_w: f32, pixels: (f32, f32)) -> (f32, f32) {
    (place_w, pixels.1 * place_w / pixels.0.max(1.))
}

fn button_bit(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
        _ => 0,
    }
}

fn cdp_modifiers(m: &Modifiers) -> u32 {
    (m.alt as u32) | (m.control as u32) << 1 | (m.platform as u32) << 2 | (m.shift as u32) << 3
}

/// A JPEG or PNG frame to the BGRA image gpui draws.
fn decode(bytes: &[u8], format: image::ImageFormat) -> Option<Arc<RenderImage>> {
    let mut rgba = image::load_from_memory_with_format(bytes, format).ok()?.into_rgba8();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The user's own pages (mail, tickets) are not shown while presenting, as files git ignores are not.
        if self.user.is_some() && !self.unveiled && crate::workspace::present::on() && crate::state::with_settings(|s| s.present_veil) {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(cx.theme().muted_foreground)
                .child("A tab of your own browser: it is not shown while presenting.")
                .child(Button::new("unveil").small().outline().label("Show it anyway").on_click(cx.listener(|this, _, _, cx| {
                    this.unveiled = true;
                    cx.notify();
                })))
                .into_any_element();
        }
        if self.asleep && self.visible {
            self.wake(window, cx);
        }
        let dark = match self.scheme {
            Scheme::AsTheIde => cx.theme().mode.is_dark(),
            Scheme::Light => false,
            Scheme::Dark => true,
        };
        let media = (dark, calm());
        if self.sent_media != Some(media) {
            self.sent_media = Some(media);
            // One call says all: a feature left out is no longer emulated.
            let (scheme, motion) = (if dark { "dark" } else { "light" }, if media.1 { "reduce" } else { "no-preference" });
            self.command(
                "Emulation.setEmulatedMedia",
                json!({ "features": [{ "name": "prefers-color-scheme", "value": scheme }, { "name": "prefers-reduced-motion", "value": motion }] }),
            );
        }
        let bounds = self.bounds.clone();
        let sent = self.sent_size.clone();
        let layout = self.layout.clone();
        let out = self.out.clone();
        let scale = window.scale_factor();
        let zoom = self.zoom;
        let device = self.device_size();
        let (view_w, _, factor, offset, _) = fit_all(self.bounds.get().size, device, zoom);
        let agent = self.device.and_then(|i| user_agent(DEVICES[i].0, &self.major));
        let which = self.device.map_or(0, |i| i as u32 + 1);
        let address = self.url.to_string();
        let weak = cx.entity().downgrade();
        let (entity, focus) = (cx.entity(), self.focus.clone());
        let page = div()
                    .id("page")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .relative()
                    .overflow_hidden()
                    // A screen narrower than the tab sits on the panel's color; a page not drawn yet on the theme's, not a white flash.
                    .bg(if device.is_some() { cx.theme().muted } else { cx.theme().background })
                    .track_focus(&self.focus)
                    .cursor(if self.picking { CursorStyle::Crosshair } else { self.cursor })
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, window, cx| this.press(e, window, cx)))
                    .on_mouse_down(MouseButton::Middle, cx.listener(|this, e: &MouseDownEvent, window, cx| this.press(e, window, cx)))
                    .on_mouse_down(MouseButton::Right, cx.listener(|this, e: &MouseDownEvent, window, cx| this.press(e, window, cx)))
                    .on_mouse_down(MouseButton::Navigate(NavigationDirection::Back), cx.listener(|this, _, _, _| this.back()))
                    .on_mouse_down(MouseButton::Navigate(NavigationDirection::Forward), cx.listener(|this, _, _, _| this.forward()))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, e: &MouseUpEvent, _, cx| {
                        if !this.picking {
                            return;
                        }
                        match this.drag.take() {
                            Some((from, _)) if (from.x - e.position.x).abs() > px(6.) || (from.y - e.position.y).abs() > px(6.) => {
                                let (from, to) = (this.pointer(from), this.pointer(e.position));
                                this.run_pick(cx, move |out, dir| pick_area(&out, from, to, &dir));
                            }
                            _ => this.pick_at(e.position, cx),
                        }
                    }))
                    .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                        // Held, the window's listener sends it.
                        if this.pressed == 0 && this.dialog.is_none() {
                            // The page still sees the pointer, so what shows on hover can be picked.
                            this.mouse("mouseMoved", e.position, None, 0, &e.modifiers);
                        }
                        if let Some((_, to)) = this.drag.as_mut() {
                            *to = e.position;
                            this.hover = None;
                            cx.notify();
                        } else if this.picking {
                            this.hover_at(e.position, cx);
                        }
                    }))
                    .on_scroll(cx.listener(|this, e: &ScrollEvent, window, cx| {
                        if this.select.take().is_some() {
                            cx.notify();
                        }
                        let delta = e.delta.pixel_delta(window.line_height());
                        let (x, y) = this.pointer(e.position);
                        let factor = this.layout.get().0;
                        this.command(
                            "Input.dispatchMouseEvent",
                            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": -f32::from(delta.x) / factor, "deltaY": -f32::from(delta.y) / factor }),
                        );
                    }))
                    .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                        this.key(e, window, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        // Tracks the page area: Chrome renders at exactly this
                        // size and the window's scale, so frames are 1:1. A
                        // button held on the page has its moves and release go
                        // there wherever the pointer is, as in a browser.
                        canvas(
                            move |b, _, _| {
                                bounds.set(b);
                                let (width, height, factor, offset, mobile) = fit_all(b.size, device, zoom);
                                layout.set((factor, offset));
                                let told = (width, height, (scale * factor * 1000.) as u32, u32::from(mobile) | which << 1);
                                if told != sent.get() && width > 0 && height > 0 && f32::from(b.size.width) > 0. {
                                    sent.set(told);
                                    let _ = out.send(Out::Resize(width, height, scale * factor, mobile, agent.clone(), address.clone()));
                                }
                            },
                            move |b, _, window, cx| {
                                // Text an input method composes goes to the page.
                                window.handle_input(&focus, ElementInputHandler::new(b, entity.clone()), cx);
                                let moved = weak.clone();
                                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                                    if phase == DispatchPhase::Bubble {
                                        let _ = moved.update(cx, |this, cx| {
                                            if this.resizing {
                                                this.resize_dock(e.position, cx);
                                            } else if this.pressed != 0 {
                                                this.mouse("mouseMoved", e.position, e.pressed_button, 0, &e.modifiers);
                                            }
                                        });
                                    }
                                });
                                let released = weak.clone();
                                window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                                    if phase == DispatchPhase::Bubble {
                                        let _ = released.update(cx, |this, _| {
                                            this.resizing = false;
                                            this.release(e)
                                        });
                                    }
                                });
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .when_some(self.frame.clone(), |this, frame| {
                        // 1:1, never stretched: while a resize waits for frames
                        // of the new size, the old one stays sharp at its size.
                        // As wide as the page's place, whatever pixels it has: on a screen of
                        // two pixels to one Chrome sends the frames of a page in motion with
                        // one to each and the sharp one after them with two, and drawn by
                        // their pixels the page jumped between half its size and all of it.
                        let size = frame.size(0);
                        let (w, h) = frame_drawn(view_w as f32 * factor, (size.width.0 as f32, size.height.0 as f32));
                        let _ = scale;
                        this.child(img(frame).absolute().top_0().left(px(offset)).w(px(w)).h(px(h)))
                    })
                    .children(self.render_places(cx))
                    .when_some(self.hover.filter(|_| self.picking).map(|b| self.on_screen(b)), |this, hover| {
                        let color = cx.theme().primary;
                        this.child(div().absolute().left(hover.left()).top(hover.top()).w(hover.size.width).h(hover.size.height).border_1().border_color(color).bg(color.opacity(0.2)))
                    })
                    .when_some(self.drag.filter(|_| self.picking), |this, (from, to)| {
                        let origin = self.bounds.get().origin;
                        let color = cx.theme().primary;
                        this.child(
                            div()
                                .absolute()
                                .left(from.x.min(to.x) - origin.x)
                                .top(from.y.min(to.y) - origin.y)
                                .w((from.x - to.x).abs())
                                .h((from.y - to.y).abs())
                                .border_1()
                                .border_color(color)
                                .bg(color.opacity(0.15)),
                        )
                    })
                    .when_some(self.picker.clone(), |this, picker| this.child(self.render_picker(&picker, cx)))
                    .when_some(self.select.clone(), |this, menu| this.child(self.render_select(&menu, cx)))
                    .when_some(self.dialog.clone(), |this, dialog| this.child(self.render_dialog(&dialog, cx)))
                    // Headless Chrome has no menu of its own: that is browser UI.
                    .context_menu(|menu, _, _| {
                        menu.menu("Back", Box::new(BrowserBack))
                            .menu("Forward", Box::new(BrowserForward))
                            .menu("Reload", Box::new(BrowserReload))
                            .separator()
                            .menu("Copy", Box::new(BrowserCopy))
                            .menu("Paste", Box::new(BrowserPaste))
                            .menu("Find…", Box::new(BrowserFind))
                            .separator()
                            .menu("Copy page address", Box::new(BrowserCopyUrl))
                            .menu("Open in system browser", Box::new(BrowserOpenExternal))
                    });
        if self.bare {
            return v_flex()
                .size_full()
                .key_context(KEY_CONTEXT)
                .on_action(cx.listener(|this, _: &BrowserCopy, _, cx| this.copy(cx)))
                .on_action(cx.listener(|this, _: &BrowserPaste, _, cx| this.paste(cx)))
                .on_action(cx.listener(|this, _: &gpui_kit::component::input::Copy, _, cx| this.copy(cx)))
                .on_action(cx.listener(|this, _: &gpui_kit::component::input::Paste, _, cx| this.paste(cx)))
                .on_action(cx.listener(|this, _: &gpui_kit::component::input::Cut, _, cx| this.cut(cx)))
                .on_action(cx.listener(|this, _: &gpui_kit::component::input::SelectAll, _, _| this.edit("selectAll", 'a', false)))
                .child(page)
                .into_any_element();
        }
        let page = self.with_devtools(page, cx);
        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &BrowserBack, _, _| this.back()))
            .on_action(cx.listener(|this, _: &BrowserForward, _, _| this.forward()))
            .on_action(cx.listener(|this, _: &BrowserReload, _, _| this.reload(false)))
            .on_action(cx.listener(|this, _: &BrowserHardReload, _, _| this.reload(true)))
            .on_action(cx.listener(|this, _: &BrowserCopy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &BrowserPaste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &BrowserFocusAddress, window, cx| {
                this.url_input.update(cx, |input, cx| {
                    input.focus(window, cx);
                    input.select_all(window, cx);
                })
            }))
            .on_action(cx.listener(|this, _: &BrowserFind, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &BrowserZoomIn, _, cx| this.zoom_step(true, cx)))
            .on_action(cx.listener(|this, _: &BrowserZoomOut, _, cx| this.zoom_step(false, cx)))
            .on_action(cx.listener(|this, _: &BrowserZoomReset, _, cx| this.set_zoom(1., cx)))
            .on_action(cx.listener(|this, _: &BrowserCopyUrl, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(this.url.to_string()))
            }))
            .on_action(cx.listener(|this, _: &BrowserOpenExternal, _, cx| cx.open_url(&this.url)))
            .child(self.render_toolbar(cx))
            .when(self.finding, |this| this.child(self.render_find(cx)))
            .when(self.problems_open && !self.problems.is_empty(), |this| this.child(self.render_problems(cx)))
            .when(self.picking || !self.picked.is_empty(), |this| this.child(self.render_picked(cx)))
            .when(self.passkey, |this| {
                let url = self.url.to_string();
                let always = url.clone();
                let host = host_of(&url).filter(|h| !h.is_empty());
                this.child(
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .bg(cx.theme().secondary)
                        .child(div().flex_1().text_xs().child(
                            "The site asked for a passkey or a security key: this browser has no window for them, so it said no, and the site may offer another way in. Or sign in in the system's browser.",
                        ))
                        .child(Button::new("passkey-system").xsmall().outline().label("Open in the system's browser").on_click(move |_, _, cx| cx.open_url(&url)))
                        .when_some(host, |bar, host| {
                            bar.child(Button::new("passkey-always").xsmall().ghost().label(format!("Always for {host}")).on_click(move |_, _, cx| {
                                let mut settings = crate::state::settings();
                                if !settings.system_hosts.contains(&host) {
                                    settings.system_hosts.push(host.clone());
                                }
                                crate::state::set_settings(settings);
                                cx.open_url(&always);
                            }))
                        })
                        .child(Button::new("passkey-close").xsmall().ghost().label("×").on_click(cx.listener(|this, _, _, cx| {
                            this.passkey = false;
                            cx.notify();
                        }))),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .child(div().flex_1().text_xs().text_color(gpui_kit::red()).child(error))
                        .child(Button::new("error-reload").xsmall().ghost().label("Reload").on_click(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            this.reload(false);
                            cx.notify();
                        })))
                        .child(Button::new("error-close").xsmall().ghost().label("×").on_click(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            cx.notify();
                        }))),
                )
            })
            .on_action(cx.listener(|this, _: &BrowserDevTools, window, cx| this.toggle_devtools(window, cx)))
            .child(page)
            .into_any_element()
    }
}

/// An input method's text: composed in the page as it is typed, put in when done.
impl EntityInputHandler for BrowserView {
    fn text_for_range(&mut self, range: std::ops::Range<usize>, adjusted: &mut Option<std::ops::Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let utf16: Vec<u16> = self.marked.as_deref().unwrap_or_default().encode_utf16().collect();
        let (start, end) = (range.start.min(utf16.len()), range.end.min(utf16.len()));
        let start = start.min(end);
        if start != range.start || end != range.end {
            *adjusted = Some(start..end);
        }
        Some(String::from_utf16_lossy(&utf16[start..end]))
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let length = self.marked.as_deref().map_or(0, |m| m.encode_utf16().count());
        Some(UTF16Selection { range: length..length, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<std::ops::Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        if let Some(text) = self.marked.take().filter(|t| !t.is_empty()) {
            self.command("Input.insertText", json!({ "text": text }));
        }
    }

    fn replace_text_in_range(&mut self, _: Option<std::ops::Range<usize>>, text: &str, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
        self.command("Input.insertText", json!({ "text": text }));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let length = text.encode_utf16().count();
        self.marked = (!text.is_empty()).then(|| text.to_string());
        self.command("Input.imeSetComposition", json!({ "text": text, "selectionStart": length, "selectionEnd": length }));
    }

    /// The candidate window opens at the caret, as the page last told it.
    fn bounds_for_range(&mut self, _: std::ops::Range<usize>, element_bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let at = self.caret.map(|c| self.on_screen(c)).unwrap_or(Bounds { origin: point(px(24.), px(24.)), size: size(px(2.), px(20.)) });
        Some(Bounds { origin: element_bounds.origin + at.origin, size: at.size })
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    /// The key a click opens a link in a tab of its own with, as CDP's modifiers count it:
    /// Ctrl, Meta (Cmd) on macOS.
    const NEW_TAB: u32 = if cfg!(target_os = "macos") { 4 } else { 2 };
    use super::*;

    /// Ends the pictures' Chrome however a test ends: on macOS nothing else does.
    /// Ends the owner's Chrome as a test ends, failed or not, and only then
    /// removes its profile: a Chrome ending writes its profile out, and one
    /// removed before it would be made again.
    struct Quit(Owner);
    impl Drop for Quit {
        fn drop(&mut self) {
            if let Some(chrome) = slot(&self.0).chrome.lock().ok().and_then(|mut c| c.take()) {
                end(chrome);
            }
            let (profile, all) = match &self.0 {
                // Its waker too, which a call at its port would start it again by.
                Owner::Task(root) => {
                    forget(root);
                    (profile_of(root), profiles_dir())
                }
                Owner::User(browser) => (user_profile_path(browser), user_profiles_dir()),
                Owner::Pictures => return,
            };
            // What Chrome still writes as it ends makes it again: removed until it stays gone half a second.
            let (started, mut gone) = (Instant::now(), None::<Instant>);
            while started.elapsed() < Duration::from_secs(4) && gone.is_none_or(|at| at.elapsed() < Duration::from_millis(500)) {
                if profile.exists() {
                    let _ = std::fs::remove_dir_all(&profile);
                    gone = None;
                } else if gone.is_none() {
                    gone = Some(Instant::now());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = std::fs::remove_dir(all);
        }
    }

    fn end(mut chrome: Arc<Chrome>) {
        let started = Instant::now();
        // A page's writer lets go of it once the page is closed.
        while started.elapsed() < Duration::from_secs(3) {
            match Arc::try_unwrap(chrome) {
                Ok(owned) => return drop(owned),
                Err(shared) => chrome = shared,
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Still held by a page's thread: ended all the same, or it is left
        // running when the test is over, a gigabyte each time.
        chrome.close();
    }

    #[core::prelude::v1::test]
    fn the_port_of_an_address_and_where_it_is_written() {
        let place = |url: &str| super::port_place(url).map(|(port, at, written)| (port, url.get(at).unwrap_or_default().to_string(), written));
        assert_eq!(place("http://localhost:3000/a?b"), Some((3000, "3000".into(), true)));
        assert_eq!(place("http://[::1]:8443"), Some((8443, "8443".into(), true)));
        assert_eq!(place("http://localhost/x"), Some((80, String::new(), false)), "the scheme's own, where it would be written");
        assert_eq!(super::port_place("http://localhost/x").unwrap().1, 16..16);
        assert_eq!(place("https://example.com:3000/"), None, "not of this machine");
    }

    #[core::prelude::v1::test]
    fn addresses_of_this_machine() {
        assert_eq!(local_address("http://localhost:3000/a?b").as_deref(), Some("localhost:3000"));
        assert_eq!(local_address("http://0.0.0.0:8080").as_deref(), Some("127.0.0.1:8080"));
        assert_eq!(local_address("http://app.localhost/").as_deref(), Some("localhost:80"));
        assert_eq!(local_address("https://[::1]:8443/").as_deref(), Some("[::1]:8443"));
        assert_eq!(local_address("https://example.com/"), None);
    }

    #[core::prelude::v1::test]
    fn a_task_s_link_opens_where_its_settings_say() {
        let (tab, system) = (Path::new("/tasks/links-in-tabs"), Path::new("/tasks/links-in-system"));
        // A tab of the user's browser unless set otherwise.
        let mut own = crate::layers::Overrides::default();
        own.links_open_in = Some("system".into());
        crate::layers::set_project(system, Arc::new(crate::layers::resolve(&Default::default(), None, None, &own)));
        assert!(opens_in_tab(tab, "https://jira.example.com/browse/DEV-1") || !cfg!(unix));
        assert!(!opens_in_tab(tab, "http://localhost:3000/"), "a stand's: the task's own browser");
        assert!(!opens_in_tab(tab, "http://127.0.0.1:8080/x"));
        assert!(!opens_in_tab(tab, "mailto:someone@example.com"));
        assert!(!opens_in_tab(system, "https://jira.example.com/browse/DEV-1"), "the system's browser where set so");
    }

    #[core::prelude::v1::test]
    fn a_site_named_for_the_system_s_browser() {
        assert!(host_named("example.com", "https://example.com/x"));
        assert!(host_named("*.example.com", "https://sso.EXAMPLE.com:8443/login"));
        assert!(host_named("*.example.com", "https://example.com/"));
        assert!(!host_named("example.com", "https://sso.example.com/"));
        assert!(!host_named("*.example.com", "https://notexample.com/"));
        assert_eq!(host_of("https://u:p@[::1]:8080/x"), Some("::1".into()));
    }

    #[core::prelude::v1::test]
    fn an_address_without_its_secrets() {
        assert_eq!(without_secrets("https://jira.example.com/browse/DEV-1?focusedId=5#comment-1"), "https://jira.example.com/browse/DEV-1?focusedId=5#comment-1");
        assert_eq!(without_secrets("https://app.example/cb?code=abc&state=x&tab=2"), "https://app.example/cb?tab=2");
        assert_eq!(without_secrets("https://b.s3.example/f?X-Amz-Signature=1&X-Amz-Credential=2"), "https://b.s3.example/f");
        assert_eq!(without_secrets("https://app.example/#access_token=xyz&type=bearer"), "https://app.example/");
        assert_eq!(without_secrets("https://пример.example/путь?токен=1&csrf_token=2"), "https://пример.example/путь?токен=1");
    }

    #[core::prelude::v1::test]
    fn an_ordinary_chrome_s_version_and_agent() {
        assert_eq!(major_in("Google Chrome 158.0.8127.36 "), Some("158".into()));
        assert_eq!(major_in("Chromium 158.0.8127.36 snap"), Some("158".into()));
        assert_eq!(major_in("158.0.8127.36"), Some("158".into()));
        assert_eq!(major_in("Chromium"), None);
        assert_eq!(major_of_product("HeadlessChrome/158.0.8127.36"), "158");
        let agent = ordinary_agent("158", false);
        assert!(agent.starts_with("Mozilla/5.0 (") && agent.ends_with(") AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Safari/537.36"), "{agent}");
        assert!(ordinary_agent("158", true).ends_with(" Safari/537.36 Edg/158.0.0.0"));
    }

    #[core::prelude::v1::test]
    fn a_task_keeps_its_port() {
        // By its key, not its path: the key has the state's folder in it, which
        // tests running beside this one change (IDEAE_STATE).
        let port = super::port_of("/state\0/w/a");
        assert_eq!(port, super::port_of("/state\0/w/a"));
        assert!((21000..30000).contains(&port));
        assert_ne!(port, super::port_of("/state\0/w/b"));
        assert_ne!(port, super::port_of("/other\0/w/a"), "another state, another port");
        assert!(super::task_key(Path::new("/w/a")).ends_with("\0/w/a"));
    }

    #[core::prelude::v1::test]
    fn problems_of_a_page() {
        let thrown =
            json!({ "exceptionDetails": { "text": "Uncaught", "exception": { "description": "TypeError: x is undefined\n    at f (app.js:3:5)" } } });
        assert_eq!(problem("Runtime.exceptionThrown", &thrown).unwrap(), "TypeError: x is undefined\n    at f (app.js:3:5)");
        let logged = json!({ "type": "error", "args": [{ "type": "string", "value": "bad" }, { "type": "number", "value": 3 }],
            "stackTrace": { "callFrames": [{ "url": "http://x/app.js", "lineNumber": 9 }] } });
        assert_eq!(problem("Runtime.consoleAPICalled", &logged).unwrap(), "console.error: bad 3 (http://x/app.js:10)");
        assert!(problem("Runtime.consoleAPICalled", &json!({ "type": "log", "args": [] })).is_none());
        let failed = json!({ "entry": { "level": "error", "text": "Failed to load resource: 500", "url": "http://x/api" } });
        assert_eq!(problem("Log.entryAdded", &failed).unwrap(), "Failed to load resource: 500 (http://x/api)");
    }

    /// Needs Chrome (or `IDEAE_CHROME`): `cargo test -- --ignored pictures`.
    #[core::prelude::v1::test]
    #[ignore]
    fn pictures_a_page_at_the_size_asked() {
        let _quit = Quit(Owner::Pictures);
        let out = std::env::temp_dir().join(format!("ideae-shot-test-{}.png", std::process::id()));
        shot("data:text/html,<body style='margin:0;background:rgb(0,128,0)'>", 640, 400, &out).unwrap();
        let image = image::open(&out).unwrap().into_rgba8();
        assert_eq!(image.dimensions(), (640, 400));
        assert!(image.pixels().all(|p| p.0 == [0, 128, 0, 255]), "the page, all of the picture");
        assert!(shot("http://127.0.0.1:9/", 640, 400, &out).is_err(), "nothing serves there");
        let _ = std::fs::remove_file(&out);
    }

    /// Needs Chrome (or `IDEAE_CHROME`): `cargo test -- --ignored picks`.
    #[core::prelude::v1::test]
    #[ignore]
    fn picks_an_element_out_of_a_scrolled_page() {
        let page = "data:text/html,<body style='margin:0'><div style='height:1500px'></div>\
            <ul id='users'><li class='row'>Ada</li><li class='row'>\
            <a class='name' href='/b' style='display:inline-block;width:120px;height:30px;background:rgb(255,0,0);color:rgb(255,0,0)'>Grace Hopper</a>\
            </li></ul><div style='height:1500px'></div>";
        let _quit = Quit(Owner::Pictures);
        let (frames, _kept) = async_channel::bounded(64);
        let out = spawn_page(Owner::Pictures, Source::Url(page.into()), frames);
        let eval = |expression: &str| {
            call(&out, "Runtime.evaluate", json!({ "expression": expression, "returnByValue": true })).map(|r| r["result"]["value"].clone())
        };
        call(&out, "Emulation.setDeviceMetricsOverride", json!({ "width": 800, "height": 600, "deviceScaleFactor": 2, "mobile": false }));
        let started = Instant::now();
        while eval("!!document.querySelector('a.name')") != Some(json!(true)) {
            assert!(started.elapsed() < Duration::from_secs(20), "the page loads");
            std::thread::sleep(Duration::from_millis(50));
        }
        eval("window.scrollTo(0, 1400)");
        let at = eval("(() => { const r = document.querySelector('a.name').getBoundingClientRect(); return [r.left + 5, r.top + 5] })()").unwrap();
        let (x, y) = (at[0].as_f64().unwrap() as f32, at[1].as_f64().unwrap() as f32);
        assert!(y > 0. && y < 600., "the link is in view at {y}");

        let hover = element_box(&out, x, y).expect("an element there");
        assert_eq!((hover.size.width, hover.size.height), (px(120.), px(30.)));
        assert_eq!((hover.origin.x + px(5.), hover.origin.y + px(5.)), (px(x), px(y)));

        let dir = std::env::temp_dir().join(format!("ideae-pick-test-{}", std::process::id()));
        let message = pick(&out, x, y, &dir).expect("the element, described").text;
        assert!(message.contains("the element `a.name`:"), "the shortest selector that finds it alone: {message}");
        assert!(message.contains("<a class=\"name\" href=\"/b\"") && message.contains("text: \"Grace Hopper\""), "{message}");
        let picture = message.lines().find_map(|l| l.strip_prefix("picture: ")).expect("a picture");
        let image = image::open(picture).unwrap().into_rgba8();
        assert_eq!(image.dimensions(), (240, 60));
        assert!(image.pixels().all(|p| p.0 == [255, 0, 0, 255]), "the picture is the link and nothing around it");

        let at = eval("(() => { const r = document.querySelector('li').getBoundingClientRect(); return [r.right - 5, r.top + 5] })()").unwrap();
        let message = pick(&out, at[0].as_f64().unwrap() as f32, at[1].as_f64().unwrap() as f32, &dir).expect("the first row").text;
        assert!(message.contains("the element `li.row:nth-of-type(1)`:"), "one of two alike: {message}");
        let _ = std::fs::remove_dir_all(&dir);
        drop(out);
    }

    /// What headless Chrome would show nowhere or keep to itself comes to
    /// the tab: a dialog, a select's list, the cursor, the page's errors, a
    /// page it opens, its title and the address of a page of an app.
    /// On a screen of two pixels to one Chrome sends the frames of a page in
    /// motion with one pixel to each CSS pixel and the sharp one after them
    /// with two: both are drawn as large, and as large as the page's place.
    #[core::prelude::v1::test]
    fn a_frame_is_as_wide_as_the_page_s_place_whatever_its_pixels() {
        let moving = frame_drawn(623., (623., 518.));
        let sharp = frame_drawn(623., (1246., 1036.));
        assert_eq!((moving, sharp), ((623., 518.), (623., 518.)));
        assert_eq!(frame_drawn(934.5, (623., 518.)), (934.5, 777.), "zoomed, by the zoom");
        // The place made narrower, and the frame of the new size not come yet: smaller, in shape.
        assert_eq!(frame_drawn(500., (1246., 1036.)).1.round(), 416.);
    }

    /// On a screen of two pixels to one the frames of a page's stream have
    /// two to one as well, like the picture taken when it rests: with one
    /// they were drawn stretched, and the page was soft whenever it moved.
    /// Needs Chrome: `cargo test -- --ignored a_stream_has_the_screen_s_pixels`.
    #[core::prelude::v1::test]
    #[ignore]
    fn a_stream_has_the_screen_s_pixels() {
        if no_chrome() {
            return;
        }
        let _one = ONE_CHROME.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("ideae-stream-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let owner = Owner::Task(root.clone());
        let _quit = Quit(owner.clone());
        SCREEN_SCALE.store(200, std::sync::atomic::Ordering::Relaxed);
        let (frames, kept) = async_channel::bounded(64);
        let out = spawn_page(owner, Source::Url("data:text/html,<body style='margin:0'><h1>Sharp</h1>".into()), frames);
        let started = Instant::now();
        while !matches!(kept.try_recv(), Ok(In::Ready { .. })) {
            assert!(started.elapsed() < Duration::from_secs(20), "the page connects");
            std::thread::sleep(Duration::from_millis(20));
        }
        call(&out, "Emulation.setDeviceMetricsOverride", json!({ "width": 800, "height": 600, "deviceScaleFactor": 2, "mobile": false }));
        call(&out, "Page.startScreencast", json!({ "format": "jpeg", "quality": 95, "maxWidth": 1600, "maxHeight": 1200 }));
        let size = |bytes: &[u8], format| image::load_from_memory_with_format(bytes, format).map(|picture| (picture.width(), picture.height())).ok();
        let began = Instant::now();
        let stream = loop {
            call(&out, "Runtime.evaluate", json!({ "expression": "document.body.style.background = '#' + Math.floor(Math.random() * 900 + 100)" }));
            match kept.try_recv() {
                Ok(In::Frame(bytes, _)) if size(&bytes, image::ImageFormat::Jpeg) != Some((800, 600)) || began.elapsed() > Duration::from_secs(3) => break size(&bytes, image::ImageFormat::Jpeg),
                _ => assert!(began.elapsed() < Duration::from_secs(10), "a frame comes"),
            }
            std::thread::sleep(Duration::from_millis(30));
        };
        let shot = call(&out, "Page.captureScreenshot", json!({ "format": "png" })).and_then(|r| base64::engine::general_purpose::STANDARD.decode(r["data"].as_str()?).ok()).unwrap_or_default();
        assert_eq!((stream, size(&shot, image::ImageFormat::Png)), (Some((1600, 1200)), Some((1600, 1200))));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One at a time: three Chromes starting at once, beside all the check
    /// runs then, take longer to answer than a test waits for one.
    static ONE_CHROME: Mutex<()> = Mutex::new(());

    /// A machine with no Chrome cannot run these, and says so: the check
    /// before a push goes on with the rest instead of ending there.
    fn no_chrome() -> bool {
        let none = std::env::var_os("IDEAE_CHROME").is_none() && find_chrome().is_none();
        if none {
            eprintln!("SKIPPED: no Chrome or Chromium here (name one in IDEAE_CHROME)");
        }
        none
    }

    /// Needs Chrome: `cargo test -- --ignored a_page_tells_the_tab`.
    #[core::prelude::v1::test]
    #[ignore]
    fn a_page_tells_the_tab() {
        if no_chrome() {
            return;
        }
        let _one = ONE_CHROME.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("ideae-web-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let owner = Owner::Task(root.clone());
        let _quit = Quit(owner.clone());
        let adopted = page_events(&root);
        let html = "<title>Shop</title><body style='margin:0'>\
            <select id='s' style='position:absolute;left:10px;top:10px;width:100px;height:20px'>\
            <optgroup label='Fruit'><option>Apple</option><option disabled>Pear</option></optgroup><option>Other</option></select>\
            <a href='#x' id='l' style='position:absolute;left:10px;top:100px;width:100px;height:20px;display:block'>link</a>\
            <input id='d' type='date' value='2026-01-02' style='position:absolute;left:10px;top:200px;width:160px;height:24px'>\
            <script>document.getElementById('s').onchange = (e) => console.error('chose', e.target.value); \
            document.getElementById('d').onchange = (e) => console.error('dated', e.target.value)</script>";
        // A server of its own: a data: page may not go anywhere, not even to its #fragment.
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let page = format!("http://127.0.0.1:{}/", server.local_addr().unwrap().port());
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            for mut stream in server.incoming().flatten() {
                let mut asked = [0u8; 2048];
                let _ = stream.read(&mut asked);
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len());
            }
        });
        let (tx, rx) = async_channel::bounded(64);
        let out = spawn_page(owner, Source::Url(page.clone()), tx);
        let started = Instant::now();
        let wait = |what: &str, found: &mut dyn FnMut(&In) -> bool| loop {
            assert!(started.elapsed() < Duration::from_secs(30), "waited for {what}");
            match rx.try_recv() {
                Ok(message) if found(&message) => return message,
                Ok(In::Error(e)) => panic!("waiting for {what}: {e}"),
                Ok(_) => {}
                Err(async_channel::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => panic!("the page ended waiting for {what}"),
            }
        };
        wait("the title", &mut |m| matches!(m, In::Title(t) if t == "Shop"));
        // To its sites an ordinary Chrome: Google's sign-in, on a stand's "Sign in with Google", refuses a headless one.
        let said = call(&out, "Runtime.evaluate", json!({ "expression": "navigator.userAgent + ' ' + navigator.webdriver", "returnByValue": true })).unwrap();
        let said = said["result"]["value"].as_str().unwrap().to_string();
        assert!(said.contains(" Chrome/") && !said.contains("Headless") && said.ends_with(" false"), "{said}");
        // A size of its own, as a tab gives its page: the window Chrome starts with is as many
        // pixels whatever its screen, and on one of two to one too low for the date field below.
        let _ = out.send(Out::Resize(800, 600, 1., false, None, String::new()));
        std::thread::sleep(Duration::from_millis(300));
        let mouse = |kind: &str, x: f32, y: f32| {
            let _ = out.send(Out::Command(
                "Input.dispatchMouseEvent",
                json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": u32::from(kind == "mousePressed"), "clickCount": 1 }),
            ));
        };
        let _ = out.send(Out::Move(json!({ "type": "mouseMoved", "x": 20., "y": 110. })));
        wait("the link's cursor", &mut |m| matches!(m, In::Cursor(c) if c == "pointer"));
        // A link Ctrl+clicked (Cmd on macOS, where Ctrl+click is the right button's), or with
        // the middle button: a tab of its own, behind.
        for (button, bit, modifiers) in [("left", 1, NEW_TAB), ("middle", 4, 0)] {
            for kind in ["mousePressed", "mouseReleased"] {
                let _ = out.send(Out::Command(
                    "Input.dispatchMouseEvent",
                    json!({ "type": kind, "x": 20., "y": 110., "button": button, "buttons": if kind == "mousePressed" { bit } else { 0 }, "clickCount": 1, "modifiers": modifiers }),
                ));
            }
            let asked = Instant::now();
            loop {
                assert!(asked.elapsed() < Duration::from_secs(15), "the link opened by the {button} button, offered to the task");
                match adopted.try_recv() {
                    Ok(PageEvent::Opened { target, .. }) => {
                        let _ = call(&out, "Target.closeTarget", json!({ "targetId": target }));
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }

        mouse("mousePressed", 20., 15.);
        mouse("mouseReleased", 20., 15.);
        let In::Select(menu) = wait("the select's list", &mut |m| matches!(m, In::Select(_))) else { unreachable!() };
        let labels: Vec<(&str, bool, Option<&str>)> = menu.choices.iter().map(|c| (c.label.as_str(), c.disabled, c.group.as_deref())).collect();
        assert_eq!(labels, [("Apple", false, Some("Fruit")), ("Pear", true, Some("Fruit")), ("Other", false, None)]);
        assert_eq!((menu.bounds.origin.x, menu.bounds.size.width), (px(10.), px(100.)));
        let choose = "((i) => { const s = window.__ideaeSelect; s.selectedIndex = i; s.dispatchEvent(new Event('change', { bubbles: true })); })(2)";
        let _ = out.send(Out::Command("Runtime.evaluate", json!({ "expression": choose, "contextId": menu.context })));
        wait("the error the page logs", &mut |m| matches!(m, In::Problem(p) if p.starts_with("console.error: chose Other")));

        // Keys open the list too: Alt+Down on the select, which has the focus.
        let key = |kind: &str, key: &str, code: &str, vk: u32, modifiers: u32| {
            let _ = out.send(Out::Command("Input.dispatchKeyEvent", json!({ "type": kind, "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers })));
        };
        key("rawKeyDown", "ArrowDown", "ArrowDown", 40, 1);
        key("keyUp", "ArrowDown", "ArrowDown", 40, 1);
        let In::Select(menu) = wait("the list from the keyboard", &mut |m| matches!(m, In::Select(_))) else { unreachable!() };
        assert_eq!(menu.selected, Some(2), "the choice made is the list's");

        // A date's calendar mark opens a picker the tab draws, and the value comes back into the field.
        mouse("mousePressed", 160., 212.);
        mouse("mouseReleased", 160., 212.);
        let In::Picker(picker) = wait("the date's picker", &mut |m| matches!(m, In::Picker(_))) else { unreachable!() };
        assert_eq!((picker.kind.as_str(), picker.value.as_str()), ("date", "2026-01-02"));
        let set = "((v) => { const e = window.__ideaePicker; Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(e, v); \
            e.dispatchEvent(new Event('change', { bubbles: true })); })('2026-03-04')";
        let _ = out.send(Out::Command("Runtime.evaluate", json!({ "expression": set, "contextId": picker.context })));
        wait("the date set", &mut |m| matches!(m, In::Problem(p) if p.starts_with("console.error: dated 2026-03-04")));
        // Its own fields still take a click.
        mouse("mousePressed", 20., 212.);
        mouse("mouseReleased", 20., 212.);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!std::iter::from_fn(|| rx.try_recv().ok()).any(|m| matches!(m, In::Picker(_))), "a click on the day is the field's");

        // An area of the page, as a picture.
        let dir = std::env::temp_dir().join(format!("ideae-area-test-{}", std::process::id()));
        let area = pick_area(&out, (10., 10.), (110., 30.), &dir).expect("the area");
        assert_eq!(area.label, "area 100×20");
        let picture = area.text.lines().find_map(|l| l.strip_prefix("picture: ")).expect("a picture");
        assert_eq!(image::open(picture).unwrap().into_rgba8().dimensions(), (100, 20));
        let _ = std::fs::remove_dir_all(&dir);

        mouse("mousePressed", 20., 110.);
        mouse("mouseReleased", 20., 110.);
        wait("the address within the page", &mut |m| matches!(m, In::Url(u) if *u == format!("{page}#x")));

        let _ = out.send(Out::Command("Runtime.evaluate", json!({ "expression": "setTimeout(() => window.__answer = confirm('Sure?'))" })));
        let In::Dialog(dialog) = wait("the dialog", &mut |m| matches!(m, In::Dialog(_))) else { unreachable!() };
        assert_eq!((dialog.kind.as_str(), dialog.message.as_str()), ("confirm", "Sure?"));
        let _ = out.send(Out::Command("Page.handleJavaScriptDialog", json!({ "accept": true })));
        let answered = call(&out, "Runtime.evaluate", json!({ "expression": "window.__answer", "returnByValue": true })).unwrap();
        assert_eq!(answered["result"]["value"], json!(true), "the page has the answer and goes on");

        // A phone's size says it is a phone, to the page's scripts too.
        let pixel = user_agent("Pixel 8", "130");
        let _ = out.send(Out::Resize(412, 915, 1., true, pixel, String::new()));
        let started_ua = Instant::now();
        loop {
            let said = call(&out, "Runtime.evaluate", json!({ "expression": "navigator.userAgent + ' ' + screen.width + ' ' + (navigator.userAgentData || {}).mobile", "returnByValue": true }));
            let said = said.and_then(|r| r["result"]["value"].as_str().map(str::to_string)).unwrap_or_default();
            if said.contains("Android 14; Pixel 8") && said.ends_with(" 412 true") {
                break;
            }
            assert!(started_ua.elapsed() < Duration::from_secs(10), "the page as a phone's: {said}");
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = out.send(Out::Resize(800, 600, 1., false, None, String::new()));
        let ready_target = loop {
            // The DevTools of the page connects to it.
            let port = slot(&Owner::Task(root.clone())).chrome.lock().unwrap().as_ref().map(|c| c.port).unwrap();
            let id = call(&out, "Target.getTargetInfo", json!({})).and_then(|r| r["targetInfo"]["targetId"].as_str().map(str::to_string));
            if let Some(id) = id {
                break (port, id);
            }
        };
        let (devtools_tx, devtools_rx) = async_channel::bounded(64);
        let devtools_url = format!("http://127.0.0.1:{0}/devtools/inspector.html?ws=127.0.0.1:{0}/devtools/page/{1}", ready_target.0, ready_target.1);
        let devtools = spawn_page(Owner::Task(root.clone()), Source::Url(devtools_url), devtools_tx);
        let started_dt = Instant::now();
        loop {
            // Its Elements panel shows the page's select once connected.
            let shown = call(&devtools, "Runtime.evaluate", json!({ "expression": "document.deepText ? 0 : (() => { const walk = (n) => n.shadowRoot ? walk(n.shadowRoot) + [...n.shadowRoot.querySelectorAll('*')].map(walk).join('') : ''; return [...document.querySelectorAll('*')].map((e) => (e.shadowRoot ? walk(e) : '') + (e.textContent || '')).join('').includes('optgroup') })()", "returnByValue": true }));
            if shown.is_some_and(|r| r["result"]["value"] == json!(true)) {
                break;
            }
            assert!(started_dt.elapsed() < Duration::from_secs(20), "DevTools shows the page's elements");
            std::thread::sleep(Duration::from_millis(200));
        }
        // The page still draws with DevTools open: its window is its own.
        let frames = || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(5) {
                match rx.try_recv() {
                    Ok(In::Frame(..)) => return true,
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
            false
        };
        let _ = out.send(Out::Resize(700, 500, 1., false, None, String::new()));
        std::thread::sleep(Duration::from_millis(300));
        let _ = out.send(Out::Command("Runtime.evaluate", json!({ "expression": "document.body.style.background = 'red'" })));
        assert!(frames(), "frames with DevTools open");
        drop((devtools, devtools_rx));

        let _ = out.send(Out::Command("Runtime.evaluate", json!({ "expression": "window.open('about:blank#opened')", "userGesture": true })));
        let asked = Instant::now();
        let opened = loop {
            assert!(asked.elapsed() < Duration::from_secs(15), "the page it opened, offered to the task");
            match adopted.try_recv() {
                Ok(PageEvent::Opened { target, by_page }) => {
                    assert!(by_page, "opened by the page, so shown in front");
                    break target;
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let (tx2, rx2) = async_channel::bounded(64);
        let other = spawn_page(Owner::Task(root.clone()), Source::Target(opened), tx2);
        let started = Instant::now();
        let wait2 = |what: &str, found: &dyn Fn(&In) -> bool| loop {
            assert!(started.elapsed() < Duration::from_secs(10), "waited for {what}");
            match rx2.try_recv() {
                Ok(message) if found(&message) => return,
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        wait2("the opened page's address", &|m| matches!(m, In::Navigated { url, .. } if url.ends_with("#opened")));
        let _ = other.send(Out::Command("Runtime.evaluate", json!({ "expression": "window.close()" })));
        wait2("the opened page to close itself", &|m| matches!(m, In::Closed));
        drop((out, other));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A task's Chrome ended a while after its last page; a call at the
    /// task's port, as an agent's Playwright makes one, starts it again.
    /// Needs Chrome: `cargo test -- --ignored a_task_s_browser_wakes`.
    #[core::prelude::v1::test]
    #[ignore]
    fn a_task_s_browser_wakes_when_called() {
        if no_chrome() {
            return;
        }
        let _one = ONE_CHROME.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("ideae-wake-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let owner = Owner::Task(root.clone());
        let _quit = Quit(owner.clone());
        let (tx, rx) = async_channel::bounded(64);
        let out = spawn_page(owner.clone(), Source::Url("about:blank".into()), tx);
        let started = Instant::now();
        while !matches!(rx.try_recv(), Ok(In::Ready { .. })) {
            assert!(started.elapsed() < Duration::from_secs(20), "the page connects");
            std::thread::sleep(Duration::from_millis(20));
        }
        let first = slot(&owner).chrome.lock().unwrap().as_ref().map(|c| c.child.lock().unwrap().id()).unwrap();
        drop((out, rx));
        while slot(&owner).waker.lock().unwrap().is_none() {
            assert!(started.elapsed() < Duration::from_secs(20), "Chrome ends and the port waits");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(slot(&owner).chrome.lock().unwrap().is_none(), "no Chrome while the port waits");
        let version = devtools_version(task_port(&root)).expect("the call answered, by a Chrome started for it");
        assert!(version["webSocketDebuggerUrl"].is_string(), "{version}");
        let second = slot(&owner).chrome.lock().unwrap().as_ref().map(|c| c.child.lock().unwrap().id()).expect("a Chrome again");
        assert_ne!(first, second);
        let _ = std::fs::remove_dir_all(&root);
    }


    /// The sockets of `pid` and its children's that listen for a TCP connection.
    #[cfg(target_os = "linux")]
    fn listening(pid: u32) -> Vec<String> {
        let mut pids = vec![pid];
        let mut i = 0;
        while i < pids.len() {
            let parent = pids[i];
            for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
                let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
                // `pid (name) state ppid`: the name may hold spaces and parentheses.
                let ppid = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok());
                if let (Some(ppid), Ok(child)) = (ppid, entry.file_name().to_string_lossy().parse::<u32>()) {
                    if ppid == parent && !pids.contains(&child) {
                        pids.push(child);
                    }
                }
            }
            i += 1;
        }
        let mut inodes = HashSet::new();
        for pid in &pids {
            for fd in std::fs::read_dir(format!("/proc/{pid}/fd")).into_iter().flatten().flatten() {
                let link = std::fs::read_link(fd.path()).unwrap_or_default().display().to_string();
                if let Some(inode) = link.strip_prefix("socket:[").and_then(|l| l.strip_suffix(']')) {
                    inodes.insert(inode.to_string());
                }
            }
        }
        assert!(pids.len() > 1, "Chrome's processes found: {pids:?}");
        let mut found = Vec::new();
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            for line in std::fs::read_to_string(table).unwrap_or_default().lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                // State 0A is LISTEN; the inode is the tenth column.
                if cols.get(3) == Some(&"0A") && cols.get(9).is_some_and(|inode| inodes.contains(*inode)) {
                    found.push(line.to_string());
                }
            }
        }
        found
    }

    #[cfg(target_os = "macos")]
    fn listening(pid: u32) -> Vec<String> {
        let tree = std::process::Command::new("pgrep").args(["-P", &pid.to_string()]).output().unwrap();
        let mut pids = vec![pid.to_string()];
        pids.extend(String::from_utf8_lossy(&tree.stdout).lines().map(str::to_string));
        let out = std::process::Command::new("lsof").args(["-a", "-p", &pids.join(","), "-iTCP", "-sTCP:LISTEN", "-Fn"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.starts_with('n')).map(str::to_string).collect()
    }

    /// The user's Chrome, over its pipe: pages shown and driven as a task's
    /// are, two at once on the one connection; no port of it listens, no
    /// DevToolsActivePort says where one is, and a site sees an ordinary
    /// Chrome. Needs Chrome: `cargo test -- --ignored a_user_browser_has_no_port`.
    #[core::prelude::v1::test]
    #[ignore]
    fn a_user_browser_has_no_port() {
        if no_chrome() {
            return;
        }
        let _one = ONE_CHROME.lock().unwrap_or_else(|e| e.into_inner());
        let owner = Owner::User(UserBrowser::Space(format!("test-{}", std::process::id())));
        let _quit = Quit(owner.clone());
        // A page of this machine is a secure context, as a site's is: navigator.credentials is there.
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let site = format!("http://127.0.0.1:{}/", server.local_addr().unwrap().port());
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let html = "<title>One</title><h1 style='font-size:60px'>One</h1>";
            for mut stream in server.incoming().flatten() {
                let mut asked = [0u8; 2048];
                let _ = stream.read(&mut asked);
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len());
            }
        });
        let open = |title: &str| {
            let (tx, rx) = async_channel::bounded(256);
            let url = if title == "One" { site.clone() } else { format!("data:text/html,<title>{title}</title><h1 style='font-size:60px'>{title}</h1>") };
            let out = spawn_page(owner.clone(), Source::Url(url), tx);
            (out, rx)
        };
        let wait = |rx: &async_channel::Receiver<In>, what: &str, found: &mut dyn FnMut(&In) -> bool| {
            let started = Instant::now();
            loop {
                assert!(started.elapsed() < Duration::from_secs(30), "waited for {what}");
                match rx.try_recv() {
                    Ok(message) if found(&message) => return message,
                    Ok(In::Error(e)) => panic!("waiting for {what}: {e}"),
                    Ok(_) => {}
                    Err(async_channel::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
                    Err(_) => panic!("the page ended waiting for {what}"),
                }
            }
        };
        let (one, one_rx) = open("One");
        let (two, two_rx) = open("Two");
        let In::Ready { port, .. } = wait(&one_rx, "the page", &mut |m| matches!(m, In::Ready { .. })) else { unreachable!() };
        assert_eq!(port, 0);
        wait(&one_rx, "the first title", &mut |m| matches!(m, In::Title(t) if t == "One"));
        wait(&two_rx, "the second title", &mut |m| matches!(m, In::Title(t) if t == "Two"));
        let _ = two.send(Out::Resize(640, 480, 1., false, None, String::new()));
        let In::Frame(frame, _) = wait(&two_rx, "a frame", &mut |m| matches!(m, In::Frame(bytes, _) if !bytes.is_empty())) else { unreachable!() };
        assert!(image::load_from_memory(&frame).is_ok());

        let said = call(&one, "Runtime.evaluate", json!({ "expression": "navigator.userAgent + ' ' + navigator.webdriver", "returnByValue": true })).unwrap();
        let said = said["result"]["value"].as_str().unwrap().to_string();
        assert!(said.contains(" Chrome/") && !said.contains("Headless") && said.ends_with(" false"), "{said}");
        // A passkey asked for is refused at once, not left waiting for a dialog nobody sees, and the tab is told.
        let refused = call(
            &one,
            "Runtime.evaluate",
            json!({ "expression": "navigator.credentials.get({ publicKey: { challenge: new Uint8Array(32) } }).then(() => 'given', (e) => e.name)", "awaitPromise": true, "returnByValue": true }),
        )
        .unwrap();
        assert_eq!(refused["result"]["value"], "NotAllowedError");
        wait(&one_rx, "the tab told of the passkey", &mut |m| matches!(m, In::Passkey));

        let chrome = slot(&owner).chrome.lock().unwrap().clone().unwrap();
        assert!(chrome.pipe.is_some() && chrome.port == 0);
        assert!(!chrome.profile.join("DevToolsActivePort").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&chrome.profile).unwrap().permissions().mode() & 0o777, 0o700);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let pid = chrome.child.lock().unwrap().id();
            assert_eq!(listening(pid), Vec::<String>::new());
        }
        drop(chrome);

        // A page closed ends its reader, and the other goes on.
        let _ = one.send(Out::Close);
        let ended = Instant::now();
        while !one_rx.is_closed() {
            assert!(ended.elapsed() < Duration::from_secs(10), "the closed page's reader ended");
            std::thread::sleep(Duration::from_millis(20));
        }
        let title = call(&two, "Runtime.evaluate", json!({ "expression": "document.title", "returnByValue": true })).unwrap();
        assert_eq!(title["result"]["value"], "Two");
        let _ = two.send(Out::Close);
    }

    /// A new task's Chrome starts signed in to the project's stands as its
    /// last task's was, and to no other site: a stand's cookies and local
    /// storage carried, a company's left. Needs Chrome:
    /// `cargo test -- --ignored a_new_task_is_signed_in_to_its_stands_only`.
    #[core::prelude::v1::test]
    #[ignore]
    fn a_new_task_is_signed_in_to_its_stands_only() {
        if no_chrome() {
            return;
        }
        let _one = ONE_CHROME.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("ideae-seed-test-{}", std::process::id()));
        let repo = base.join("repo");
        let root = |name: &str| {
            let dir = base.join(name);
            std::fs::create_dir_all(repo.join(".git/worktrees").join(name)).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".git"), format!("gitdir: {}", repo.join(".git/worktrees").join(name).display())).unwrap();
            dir
        };
        let (a, b) = (root("a"), root("b"));
        // The stand: a site of this machine that signs you in.
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let site = format!("http://127.0.0.1:{}/", server.local_addr().unwrap().port());
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let html = "<title>Stand</title><script>if (!localStorage.getItem('token')) localStorage.setItem('token', 'stand-token')</script>";
            for mut stream in server.incoming().flatten() {
                let mut asked = [0u8; 2048];
                let _ = stream.read(&mut asked);
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: session=stand; Max-Age=3600\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len());
            }
        });
        let open = |owner: &Owner| {
            let (tx, rx) = async_channel::bounded(256);
            let out = spawn_page(owner.clone(), Source::Url(site.clone()), tx);
            let started = Instant::now();
            loop {
                assert!(started.elapsed() < Duration::from_secs(30), "the stand's page");
                match rx.try_recv() {
                    Ok(In::Title(t)) if t == "Stand" => break,
                    Ok(In::Error(e)) => panic!("{e}"),
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            (out, rx)
        };
        let set_cookie = |out: &mpsc::Sender<Out>, name: &str, domain: &str| {
            let set = call(out, "Network.setCookie", json!({ "name": name, "value": "1", "domain": domain, "path": "/", "expires": 4102444800i64 })).unwrap();
            assert_eq!(set["success"], true, "{name} on {domain}");
        };

        let task_a = Owner::Task(a.clone());
        let quit_a = Quit(task_a.clone());
        let (out, rx) = open(&task_a);
        set_cookie(&out, "jira", "jira.example.com");
        set_cookie(&out, "seen", "app.stand.test");
        set_cookie(&out, "named", "api.corp-dev.test");
        set_cookie(&out, "parent", ".stand.test");
        // A host its pages were served from by this machine, as its tabs note them.
        // Its Chrome's own: a profile's name holds the state's folder, which tests running beside this one change.
        let running = |owner: &Owner| slot(owner).chrome.lock().unwrap().as_ref().map(|c| c.profile.clone()).unwrap();
        let profile_a = running(&task_a);
        std::fs::write(profile_a.join(seed::STAND_HOSTS), "app.stand.test\n").unwrap();
        let _ = out.send(Out::Close);
        while !rx.is_closed() {
            std::thread::sleep(Duration::from_millis(20));
        }
        // Ended as a task's Chrome ends, its profile written out.
        if let Some(chrome) = slot(&task_a).chrome.lock().unwrap().take() {
            end(chrome);
        }
        assert!(profile_a.join("Default/Cookies").is_file() || profile_a.join("Default/Network/Cookies").is_file());

        let mut own = crate::layers::Overrides::default();
        own.stand_hosts = Some("*.corp-dev.test".into());
        crate::layers::set_project(&b, Arc::new(crate::layers::resolve(&Default::default(), None, None, &own)));
        let task_b = Owner::Task(b.clone());
        let _quit_b = Quit(task_b.clone());
        let (out, _rx) = open(&task_b);
        let all = call(&out, "Network.getAllCookies", json!({})).unwrap();
        let mut names: Vec<String> = all["cookies"].as_array().unwrap().iter().map(|c| format!("{}@{}", c["name"].as_str().unwrap(), c["domain"].as_str().unwrap())).collect();
        names.sort();
        assert_eq!(names, ["named@api.corp-dev.test", "parent@.stand.test", "seen@app.stand.test", "session@127.0.0.1"]);
        let token = call(&out, "Runtime.evaluate", json!({ "expression": "localStorage.getItem('token')", "returnByValue": true })).unwrap();
        assert_eq!(token["result"]["value"], "stand-token", "the stand's local storage came along");
        assert_eq!(std::fs::read_to_string(running(&task_b).join(seed::STAND_HOSTS)).unwrap(), "app.stand.test\n");
        let _ = out.send(Out::Close);
        drop(quit_a);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A user's Chrome serves the tabs of many tasks: a page one of them
    /// opens comes up in that task, not in another with a tab of the same
    /// Chrome. Needs Chrome: `cargo test -- --ignored a_page_opened_by_a_user_tab_goes_to_its_task`.
    #[core::prelude::v1::test]
    #[ignore]
    fn a_page_opened_by_a_user_tab_goes_to_its_task() {
        if no_chrome() {
            return;
        }
        let _one = ONE_CHROME.lock().unwrap_or_else(|e| e.into_inner());
        let browser = UserBrowser::Space(format!("route-{}", std::process::id()));
        let owner = Owner::User(browser.clone());
        let _quit = Quit(owner.clone());
        let (a, b) = (PathBuf::from("/tasks/a"), PathBuf::from("/tasks/b"));
        let (to_a, to_b) = (user_page_events(&browser, &a), user_page_events(&browser, &b));
        let open = |root: &Path, title: &str| {
            let (tx, rx) = async_channel::bounded(256);
            let out = spawn_page(owner.clone(), Source::Url(format!("data:text/html,<title>{title}</title>")), tx);
            let started = Instant::now();
            loop {
                assert!(started.elapsed() < Duration::from_secs(30), "the page {title}");
                match rx.try_recv() {
                    // As a tab of the task tells its page's id.
                    Ok(In::Ready { target, .. }) => note_task_page(&owner, &target, root),
                    Ok(In::Title(t)) if t == title => break,
                    Ok(In::Error(e)) => panic!("{e}"),
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            (out, rx)
        };
        let (in_a, _rx_a) = open(&a, "A");
        let (_in_b, _rx_b) = open(&b, "B");
        let _ = in_a.send(Out::Command("Runtime.evaluate", json!({ "expression": "window.open('about:blank#from-a')", "userGesture": true })));
        let started = Instant::now();
        let opened = loop {
            assert!(started.elapsed() < Duration::from_secs(10), "the page A opened came to A");
            if let Ok(PageEvent::Opened { target, by_page }) = to_a.try_recv() {
                assert!(by_page);
                break target;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(to_b.try_recv().is_err(), "nothing of A's went to B");
        // Whatever the opened page opens is A's too.
        assert_eq!(slot(&owner).page_tasks.lock().unwrap().get(&opened), Some(&a));
        // A link Ctrl+clicked in A (Cmd on macOS): Chrome tells no opener, it is A's all the same, behind.
        let link = "document.body.innerHTML = \"<a href='about:blank#ctrl' style='position:fixed;left:0;top:0;width:200px;height:50px;display:block'>link</a>\"";
        let _ = in_a.send(Out::Command("Runtime.evaluate", json!({ "expression": link })));
        std::thread::sleep(Duration::from_millis(200));
        for kind in ["mousePressed", "mouseReleased"] {
            let _ = in_a.send(Out::Command("Input.dispatchMouseEvent", json!({ "type": kind, "x": 20., "y": 20., "button": "left", "buttons": u32::from(kind == "mousePressed"), "clickCount": 1, "modifiers": NEW_TAB })));
        }
        let started = Instant::now();
        loop {
            assert!(started.elapsed() < Duration::from_secs(10), "the page A's link opened came to A");
            if let Ok(PageEvent::Opened { by_page, .. }) = to_a.try_recv() {
                assert!(!by_page, "behind, as a browser opens it");
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(to_b.try_recv().is_err(), "nothing of A's went to B");
    }

    /// DevTools for a page of the user's Chrome, which has no port to serve
    /// them: loaded from inside Chrome, carried over its pipe to the page
    /// inspected, and kept from its other pages. Needs Chrome:
    /// `cargo test -- --ignored devtools_without_a_port`.
    #[core::prelude::v1::test]
    #[ignore]
    fn devtools_without_a_port() {
        if no_chrome() {
            return;
        }
        let _one = ONE_CHROME.lock().unwrap_or_else(|e| e.into_inner());
        let owner = Owner::User(UserBrowser::Space(format!("devtools-{}", std::process::id())));
        let _quit = Quit(owner.clone());
        let ready = |rx: &async_channel::Receiver<In>, what: &str| {
            let started = Instant::now();
            loop {
                assert!(started.elapsed() < Duration::from_secs(30), "{what}");
                match rx.try_recv() {
                    Ok(In::Ready { target, .. }) => return target,
                    Ok(In::Error(e)) => panic!("{e}"),
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        };
        let (tx, rx) = async_channel::bounded(256);
        let _page = spawn_page(owner.clone(), Source::Url("data:text/html,<title>Inspected</title>".into()), tx);
        let inspected = ready(&rx, "the page");
        let (tx, dev_rx) = async_channel::bounded(256);
        let devtools = spawn_page(owner.clone(), Source::DevTools(inspected), tx);
        ready(&dev_rx, "DevTools' page");
        // As DevTools does: a command through its WebSocket, its answer back by its id.
        let ask = |message: &str| {
            let expression = format!(
                "new Promise((done) => {{ const wait = () => {{ if (!window.__ideaeDevtools) return setTimeout(wait, 50); \
                 const deliver = window.__ideaeDevtools.deliver; \
                 window.__ideaeDevtools.deliver = (list) => {{ for (const m of list) if (m.id === 999999) done(JSON.stringify(m)); deliver(list); }}; \
                 window.__ideaeDevtoolsSend(JSON.stringify({message})); }}; wait(); }})"
            );
            let started = Instant::now();
            loop {
                if let Some(said) = call(&devtools, "Runtime.evaluate", json!({ "expression": expression, "awaitPromise": true, "returnByValue": true })) {
                    if let Some(text) = said["result"]["value"].as_str() {
                        return serde_json::from_str::<Value>(text).unwrap();
                    }
                }
                assert!(started.elapsed() < Duration::from_secs(20), "DevTools answered: {message}");
                std::thread::sleep(Duration::from_millis(200));
            }
        };
        let title = ask(r#"{ id: 999999, method: "Runtime.evaluate", params: { expression: "document.title", returnByValue: true } }"#);
        assert_eq!(title["result"]["result"]["value"], "Inspected", "{title}");
        let denied = ask(r#"{ id: 999999, method: "Target.createTarget", params: { url: "about:blank" } }"#);
        assert!(denied["error"]["message"].as_str().is_some_and(|m| m.contains("not given")), "{denied}");
    }
}
