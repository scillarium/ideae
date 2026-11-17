//! A browser tab: a page in a shared headless Chrome, shown through CDP
//! screencast frames, with mouse and keyboard sent back over CDP.
//!
//! Chrome listens on a local DevTools port, so Playwright MCP can drive the
//! very page you see: `--cdp-endpoint http://127.0.0.1:<port>`.

use std::{
    io::ErrorKind,
    net::TcpStream,
    path::PathBuf,
    process::{Child, Stdio},
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use base64::Engine as _;
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::ContextMenuExt as _,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

// ---------------------------------------------------------------- chrome

struct Chrome {
    port: u16,
    ws_path: String,
    child: Child,
    profile: PathBuf,
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

#[cfg(unix)]
fn find_chrome() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    for app in ["Google Chrome.app/Contents/MacOS/Google Chrome", "Chromium.app/Contents/MacOS/Chromium", "Microsoft Edge.app/Contents/MacOS/Microsoft Edge"] {
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
    let roots: Vec<PathBuf> =
        ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
    let in_roots = |rel: &str| roots.iter().map(|r| r.join(rel)).find(|p| p.is_file());
    in_roots(r"Google\Chrome\Application\chrome.exe")
        .or_else(|| crate::procs::which("chrome.exe"))
        .or_else(|| in_roots(r"Chromium\Application\chrome.exe"))
        .or_else(|| in_roots(r"Microsoft\Edge\Application\msedge.exe"))
}

static CHROME: OnceLock<Mutex<Option<Arc<Chrome>>>> = OnceLock::new();

/// The app's Chrome, started on first use.
fn chrome() -> Result<Arc<Chrome>> {
    let slot = CHROME.get_or_init(|| Mutex::new(None));
    let mut slot = slot.lock().unwrap();
    if let Some(chrome) = slot.as_ref() {
        return Ok(chrome.clone());
    }
    let program =
        std::env::var_os("IDEAE_CHROME").map(PathBuf::from).or_else(find_chrome).ok_or_else(|| anyhow!("no Chrome or Chromium found"))?;
    let program = program.as_path();
    let profile = crate::state::runtime_dir().join(format!("chrome-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile)?;
    let mut command = crate::procs::command(program);
    command
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--hide-scrollbars",
            "about:blank",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = crate::procs::spawn_tied(&mut command).with_context(|| format!("starting {}", program.display()))?;

    // Chrome writes the port it picked and the browser socket path here.
    let port_file = profile.join("DevToolsActivePort");
    let started = Instant::now();
    let (port, ws_path) = loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port.trim().parse() {
                    break (port, path.trim().to_string());
                }
            }
        }
        if started.elapsed() > Duration::from_secs(15) {
            return Err(anyhow!("Chrome did not open its DevTools port"));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    PORT.store(u32::from(port), std::sync::atomic::Ordering::Relaxed);
    let chrome = Arc::new(Chrome { port, ws_path, child, profile });
    *slot = Some(chrome.clone());
    Ok(chrome)
}

/// Ends Chrome as the app quits: nothing else does where a child is not
/// told of its parent's end (macOS), and its idle time is a minute.
pub fn quit() {
    if let Some(chrome) = CHROME.get().and_then(|slot| slot.lock().ok()?.take()) {
        PORT.store(0, std::sync::atomic::Ordering::Relaxed);
        drop(chrome);
    }
}

/// The DevTools endpoint for Playwright MCP (`--cdp-endpoint`), once Chrome runs.
pub fn endpoint() -> Option<String> {
    // Not through `CHROME`: it is locked for as long as Chrome takes to
    // start, and this is asked on every frame.
    Some(PORT.load(std::sync::atomic::Ordering::Relaxed)).filter(|port| *port > 0).map(|port| format!("http://127.0.0.1:{port}"))
}

/// The DevTools port of the Chrome that runs, 0 when none does.
static PORT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

// ---------------------------------------------------------------- page session

enum Out {
    Command(&'static str, Value),
    /// A command whose result the UI waits for.
    Call(&'static str, Value, mpsc::Sender<Value>),
    /// The tab came into view or left it.
    Visible(bool),
    Close,
}

enum In {
    Frame(Vec<u8>),
    Url(String),
    Error(String),
}

/// One page: a thread owning the CDP socket.
fn spawn_page(url: String, in_tx: async_channel::Sender<In>) -> mpsc::Sender<Out> {
    let (out_tx, out_rx) = mpsc::channel::<Out>();
    std::thread::Builder::new()
        .name("cdp-page".into())
        .spawn(move || {
            if let Err(e) = run_page(&url, &in_tx, out_rx) {
                let _ = in_tx.send_blocking(In::Error(format!("{e:#}")));
            }
        })
        .expect("spawn cdp thread");
    out_tx
}

struct Socket {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    session: Option<String>,
}

impl Socket {
    fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let mut msg = json!({ "id": self.next_id, "method": method, "params": params });
        if let Some(session) = &self.session {
            msg["sessionId"] = json!(session);
        }
        self.ws.send(Message::text(msg.to_string()))?;
        Ok(self.next_id)
    }

    /// Sends and waits for the reply, setting other messages aside.
    fn call(&mut self, method: &str, params: Value, aside: &mut Vec<Value>) -> Result<Value> {
        let id = self.send(method, params)?;
        loop {
            let Some(mut msg) = self.read()? else { continue };
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
        match self.ws.read() {
            Ok(Message::Text(text)) => Ok(serde_json::from_str(text.as_str()).ok()),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// The pages open now. Chrome ends a while after the last one closes: it
/// is a browser's worth of processes kept for nothing.
static PAGES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const CHROME_IDLE: Duration = Duration::from_secs(60);

struct OpenPage;

impl OpenPage {
    fn new() -> Self {
        PAGES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

impl Drop for OpenPage {
    fn drop(&mut self) {
        if PAGES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) != 1 {
            return;
        }
        // The idle time counts from the last page closed, not an earlier last one.
        static CLOSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let this = CLOSED.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            std::thread::sleep(CHROME_IDLE);
            let Some(slot) = CHROME.get() else { return };
            let mut slot = slot.lock().unwrap();
            // Under the lock a new page waits for: it finds Chrome there, or starts one.
            if PAGES.load(std::sync::atomic::Ordering::SeqCst) == 0 && CLOSED.load(std::sync::atomic::Ordering::SeqCst) == this {
                PORT.store(0, std::sync::atomic::Ordering::Relaxed);
                *slot = None;
            }
        });
    }
}

fn run_page(url: &str, in_tx: &async_channel::Sender<In>, out_rx: mpsc::Receiver<Out>) -> Result<()> {
    let _open = OpenPage::new();
    let chrome = chrome()?;
    let (ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{}{}", chrome.port, chrome.ws_path))?;
    let mut socket = Socket { ws, next_id: 0, session: None };
    let mut aside = Vec::new();

    let target = socket.call("Target.createTarget", json!({ "url": url }), &mut aside)?;
    let target_id = target["targetId"].as_str().unwrap_or_default().to_string();
    let attached = socket.call("Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }), &mut aside)?;
    socket.session = attached["sessionId"].as_str().map(str::to_string);
    socket.call("Page.enable", json!({}), &mut aside)?;

    // Short reads so commands from the UI go out promptly.
    if let MaybeTlsStream::Plain(stream) = socket.ws.get_mut() {
        stream.set_read_timeout(Some(Duration::from_millis(10)))?;
    }

    let mut replies: std::collections::HashMap<u64, mpsc::Sender<Value>> = Default::default();
    let handle = |mut msg: Value, socket: &mut Socket, replies: &mut std::collections::HashMap<u64, mpsc::Sender<Value>>| -> Result<()> {
        if let Some(reply) = msg["id"].as_u64().and_then(|id| replies.remove(&id)) {
            let _ = reply.send(msg["result"].take());
            return Ok(());
        }
        match msg["method"].as_str() {
            Some("Page.screencastFrame") => {
                let params = &msg["params"];
                socket.send("Page.screencastFrameAck", json!({ "sessionId": params["sessionId"] }))?;
                if let Some(data) = params["data"].as_str() {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                        let _ = in_tx.try_send(In::Frame(bytes));
                    }
                }
            }
            Some("Page.frameNavigated") if msg["params"]["frame"]["parentId"].is_null() => {
                if let Some(url) = msg["params"]["frame"]["url"].as_str() {
                    let _ = in_tx.try_send(In::Url(url.to_string()));
                }
            }
            _ => {}
        }
        Ok(())
    };
    for msg in aside.drain(..) {
        handle(msg, &mut socket, &mut replies)?;
    }

    loop {
        loop {
            match out_rx.try_recv() {
                Ok(Out::Command(method, params)) => {
                    socket.send(method, params)?;
                }
                Ok(Out::Call(method, params, reply)) => {
                    let id = socket.send(method, params)?;
                    replies.insert(id, reply);
                }
                Ok(Out::Visible(visible)) => {
                    // Out of view the page sends no frames, and this thread
                    // looks for commands four times a second, not a hundred.
                    if !visible {
                        socket.send("Page.stopScreencast", json!({}))?;
                    }
                    if let MaybeTlsStream::Plain(stream) = socket.ws.get_mut() {
                        stream.set_read_timeout(Some(Duration::from_millis(if visible { 10 } else { 250 })))?;
                    }
                }
                Ok(Out::Close) | Err(mpsc::TryRecvError::Disconnected) => {
                    socket.session = None;
                    let _ = socket.send("Target.closeTarget", json!({ "targetId": target_id }));
                    return Ok(());
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if let Some(msg) = socket.read()? {
            handle(msg, &mut socket, &mut replies)?;
        }
    }
}

// ---------------------------------------------------------------- pictures

/// A picture of the page at `url` as a window of this size shows it, into
/// `out` (PNG). In a page of its own, closed after; waits for the page to
/// load and a moment more, for what it draws once loaded. Not for the UI
/// thread.
pub fn shot(url: &str, width: u32, height: u32, out: &std::path::Path) -> Result<()> {
    let _open = OpenPage::new();
    let chrome = chrome()?;
    let (ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{}{}", chrome.port, chrome.ws_path))?;
    let mut socket = Socket { ws, next_id: 0, session: None };
    let mut aside = Vec::new();
    let target = socket.call("Target.createTarget", json!({ "url": "about:blank" }), &mut aside)?;
    let target_id = target["targetId"].as_str().unwrap_or_default().to_string();
    let taken = (|| -> Result<Vec<u8>> {
        let attached = socket.call("Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }), &mut aside)?;
        socket.session = attached["sessionId"].as_str().map(str::to_string);
        socket.call("Page.enable", json!({}), &mut aside)?;
        socket.call("Emulation.setDeviceMetricsOverride", json!({ "width": width, "height": height, "deviceScaleFactor": 1, "mobile": false }), &mut aside)?;
        if let MaybeTlsStream::Plain(stream) = socket.ws.get_mut() {
            stream.set_read_timeout(Some(Duration::from_millis(100)))?;
        }
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
    socket.session = None;
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
  return JSON.stringify({
    box,
    scroll: { x: window.scrollX, y: window.scrollY },
    url: location.href,
    selector,
    html: el.cloneNode(false).outerHTML.slice(0, 400),
    text: text.slice(0, 200),
    components,
    source,
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

/// The element at a point, written out for an agent, with a picture of it
/// saved under `dir`.
fn pick(out: &mpsc::Sender<Out>, x: f32, y: f32, dir: &std::path::Path) -> Option<String> {
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
    let picture = call(out, "Page.captureScreenshot", json!({ "format": "png", "clip": clip })).and_then(|shot| {
        let png = base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str()?).ok()?;
        std::fs::create_dir_all(dir).ok()?;
        // Old pictures go when a new one comes: the agent has long seen them.
        for old in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let age = old.metadata().and_then(|m| m.modified()).ok().and_then(|at| at.elapsed().ok());
            if age.is_some_and(|age| age > Duration::from_secs(24 * 3600)) {
                let _ = std::fs::remove_file(old.path());
            }
        }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let path = dir.join(format!("element-{stamp}.png"));
        std::fs::write(&path, png).ok()?;
        Some(path)
    });

    let mut message = format!("On {} the element `{}`:\n{}\n", text("url"), text("selector"), text("html"));
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
    if let Some(path) = picture {
        message.push_str(&format!("picture: {}\n", path.display()));
    }
    Some(message)
}

// ---------------------------------------------------------------- view

actions!(
    ideae_browser,
    [BrowserBack, BrowserForward, BrowserReload, BrowserCopy, BrowserPaste, BrowserCopyUrl, BrowserOpenExternal]
);

pub enum BrowserEvent {
    /// The page's URL changed, so the tab title should too.
    Navigated,
    /// An element was picked: what to tell the agent about it.
    Picked(String),
}

impl EventEmitter<BrowserEvent> for BrowserView {}

pub struct BrowserView {
    out: mpsc::Sender<Out>,
    url_input: Entity<InputState>,
    url: SharedString,
    frame: Option<Arc<RenderImage>>,
    error: Option<String>,
    focus: FocusHandle,
    /// Where the page is drawn, for mapping the pointer, and the size the
    /// page was last told, to resize it only on change.
    bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    sent_size: std::rc::Rc<std::cell::Cell<(u32, u32, u32)>>,
    /// In a tab that is shown, of a task in view.
    visible: bool,
    /// A click picks an element for the agent instead of going to the page.
    picking: bool,
    /// The element under the pointer while picking, in the page area, and
    /// whether the page is being asked for it.
    hover: Option<Bounds<Pixels>>,
    asking: bool,
    _frames: Task<()>,
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
    pub fn url(&self) -> String {
        self.url.to_string()
    }

    /// To another address.
    pub fn go(&self, url: &str) {
        self.command("Page.navigate", json!({ "url": url }));
    }

    pub fn new(url: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (in_tx, in_rx) = async_channel::bounded::<In>(4);
        let out = spawn_page(url.to_string(), in_tx);
        // The last frame's pixels go with the tab.
        cx.on_release(|this, cx| {
            if let Some(old) = this.frame.take() {
                cx.drop_image(old, None);
            }
        })
        .detach();
        let url_input = cx.new(|cx| InputState::new(window, cx).placeholder("http://localhost:3000"));
        url_input.update(cx, |input, cx| input.set_value(url.to_string(), window, cx));
        cx.subscribe_in(&url_input, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let mut url = input.read(cx).value().trim().to_string();
                if !url.contains("://") && !url.is_empty() {
                    url = format!("http://{url}");
                }
                this.command("Page.navigate", json!({ "url": url }));
            }
        })
        .detach();

        let frames = cx.spawn(async move |this, cx| {
            while let Ok(message) = in_rx.recv().await {
                let update = match message {
                    In::Frame(jpeg) => {
                        let image = cx.background_spawn(async move { decode(&jpeg, image::ImageFormat::Jpeg) }).await;
                        let Some(image) = image else { continue };
                        Some(Ok(image))
                    }
                    In::Url(url) => Some(Err(Ok(url))),
                    In::Error(e) => Some(Err(Err(e))),
                };
                let ok = this.update(cx, |this, cx| {
                    match update {
                        // One sent before the page was told to stop.
                        Some(Ok(image)) if !this.visible => cx.drop_image(image, None),
                        Some(Ok(image)) => {
                            this.show_frame(image, cx);
                            this.sharpen_later(cx);
                        }
                        Some(Err(Ok(url))) => {
                            this.url = url.into();
                            cx.emit(BrowserEvent::Navigated);
                        }
                        Some(Err(Err(e))) => this.error = Some(e),
                        None => {}
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        });

        Self {
            out,
            url_input,
            url: url.to_string().into(),
            frame: None,
            error: None,
            focus: cx.focus_handle(),
            bounds: Default::default(),
            sent_size: Default::default(),
            visible: true,
            picking: false,
            hover: None,
            asking: false,
            _frames: frames,
            _sharpen: Task::ready(()),
        }
    }

    pub fn title(&self) -> SharedString {
        let url = self.url.as_ref();
        url.split_once("://").map_or(url, |(_, rest)| rest).trim_end_matches('/').to_string().into()
    }

    /// Out of view the page stops sending frames and the last one is let
    /// go: megabytes of pixels per tab. Back in view, drawing the page area
    /// starts the frames again.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.visible, visible) == visible {
            return;
        }
        let _ = self.out.send(Out::Visible(visible));
        if !visible {
            self.sent_size.set((0, 0, 0));
            self._sharpen = Task::ready(());
            if let Some(old) = self.frame.take() {
                // Once this update is over: a window in the middle of one is
                // passed over, and its texture of the frame would stay.
                cx.defer(move |cx| cx.drop_image(old, None));
            }
        }
        cx.notify();
    }

    fn show_frame(&mut self, image: Arc<RenderImage>, cx: &mut Context<Self>) {
        if let Some(old) = self.frame.replace(image) {
            cx.drop_image(old, None);
        }
        cx.notify();
    }

    /// After 300 ms without new frames, replace the last JPEG with a PNG.
    fn sharpen_later(&mut self, cx: &mut Context<Self>) {
        let out = self.out.clone();
        self._sharpen = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let image = cx
                .background_spawn(async move {
                    let (tx, rx) = mpsc::channel();
                    out.send(Out::Call("Page.captureScreenshot", json!({ "format": "png" }), tx)).ok()?;
                    let result = rx.recv_timeout(Duration::from_secs(5)).ok()?;
                    let png = base64::engine::general_purpose::STANDARD.decode(result["data"].as_str()?).ok()?;
                    decode(&png, image::ImageFormat::Png)
                })
                .await;
            if let Some(image) = image {
                let _ = this.update(cx, |this, cx| this.show_frame(image, cx));
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
        self.set_picking(false, cx);
        let (x, y) = self.pointer(position);
        let out = self.out.clone();
        let dir = crate::state::runtime_dir().join("picked");
        cx.spawn(async move |this, cx| {
            let message = cx.background_spawn(async move { pick(&out, x, y, &dir) }).await;
            if let Some(message) = message {
                let _ = this.update(cx, |_, cx| cx.emit(BrowserEvent::Picked(message)));
            }
        })
        .detach();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let selected = self.eval_string("window.getSelection().toString()", cx);
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
        let event = |kind: &str| json!({ "type": kind, "key": key, "code": format!("Key{c}"), "windowsVirtualKeyCode": c as u32, "modifiers": modifiers });
        let mut down = event("rawKeyDown");
        down["commands"] = json!([command]);
        self.command("Input.dispatchKeyEvent", down);
        self.command("Input.dispatchKeyEvent", event("keyUp"));
    }

    /// Copies the selection, and takes it out of the page only once it is
    /// on the clipboard: one in a frame is not seen from the page's top.
    fn cut(&mut self, cx: &mut Context<Self>) {
        let selected = self.eval_string("window.getSelection().toString()", cx);
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

    fn pointer(&self, position: Point<Pixels>) -> (f32, f32) {
        let b = self.bounds.get();
        (f32::from(position.x - b.origin.x), f32::from(position.y - b.origin.y))
    }

    fn mouse(&self, kind: &str, position: Point<Pixels>, button: Option<MouseButton>, clicks: usize, modifiers: &Modifiers) {
        let (x, y) = self.pointer(position);
        let button = match button {
            Some(MouseButton::Left) => "left",
            Some(MouseButton::Right) => "right",
            Some(MouseButton::Middle) => "middle",
            _ => "none",
        };
        self.command(
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": x, "y": y, "button": button, "clickCount": clicks, "modifiers": cdp_modifiers(modifiers) }),
        );
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = &keystroke.modifiers;
        if self.picking && keystroke.key == "escape" {
            return self.set_picking(false, cx);
        }
        let cmd = cfg!(target_os = "macos") && m.platform;
        if (m.control || cmd) && !m.alt && !m.shift {
            match keystroke.key.as_str() {
                "c" => return self.copy(cx),
                "v" => return self.paste(cx),
                _ => {}
            }
        }
        // Cmd on a Mac: Chrome is told what to do, it does not know the key.
        if cmd && !m.alt && !m.control {
            match (keystroke.key.as_str(), m.shift) {
                ("x", false) => return self.cut(cx),
                ("a", false) => return self.edit("selectAll", 'a', false),
                ("z", false) => return self.edit("undo", 'z', false),
                ("z", true) => return self.edit("redo", 'z', true),
                _ => {}
            }
        }
        let modifiers = cdp_modifiers(&keystroke.modifiers);
        let shortcut = keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform;
        if let (Some(text), false) = (&keystroke.key_char, shortcut) {
            self.command("Input.insertText", json!({ "text": text }));
            return;
        }
        let (key, code, vk) = match keystroke.key.as_str() {
            "enter" => ("Enter", "Enter", 13),
            "backspace" => ("Backspace", "Backspace", 8),
            "tab" => ("Tab", "Tab", 9),
            "escape" => ("Escape", "Escape", 27),
            "delete" => ("Delete", "Delete", 46),
            "left" => ("ArrowLeft", "ArrowLeft", 37),
            "up" => ("ArrowUp", "ArrowUp", 38),
            "right" => ("ArrowRight", "ArrowRight", 39),
            "down" => ("ArrowDown", "ArrowDown", 40),
            "home" => ("Home", "Home", 36),
            "end" => ("End", "End", 35),
            "pageup" => ("PageUp", "PageUp", 33),
            "pagedown" => ("PageDown", "PageDown", 34),
            other if other.chars().count() == 1 => {
                // A shortcut such as Ctrl+A: send the key with its modifiers.
                let c = other.chars().next().unwrap().to_ascii_uppercase();
                let code = format!("Key{c}");
                for kind in ["rawKeyDown", "keyUp"] {
                    self.command(
                        "Input.dispatchKeyEvent",
                        json!({ "type": kind, "key": other, "code": code, "windowsVirtualKeyCode": c as u32, "modifiers": modifiers }),
                    );
                }
                return;
            }
            _ => return,
        };
        for kind in ["rawKeyDown", "keyUp"] {
            self.command(
                "Input.dispatchKeyEvent",
                json!({ "type": kind, "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers }),
            );
        }
        if key == "Enter" {
            self.command("Input.dispatchKeyEvent", json!({ "type": "char", "text": "\r", "modifiers": modifiers }));
        }
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
        let bounds = self.bounds.clone();
        let sent = self.sent_size.clone();
        let out = self.out.clone();
        let scale = window.scale_factor();
        v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &BrowserBack, _, _| {
                this.command("Runtime.evaluate", json!({ "expression": "history.back()" }))
            }))
            .on_action(cx.listener(|this, _: &BrowserForward, _, _| {
                this.command("Runtime.evaluate", json!({ "expression": "history.forward()" }))
            }))
            .on_action(cx.listener(|this, _: &BrowserReload, _, _| this.command("Page.reload", json!({}))))
            .on_action(cx.listener(|this, _: &BrowserCopy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &BrowserPaste, _, cx| this.paste(cx)))
            // The menu bar's Edit items are the editor's actions: here they are the page's.
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::Cut, _, cx| this.cut(cx)))
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::SelectAll, _, _| this.edit("selectAll", 'a', false)))
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::Undo, _, _| this.edit("undo", 'z', false)))
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::Redo, _, _| this.edit("redo", 'z', true)))
            .on_action(cx.listener(|this, _: &BrowserCopyUrl, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(this.url.to_string()))
            }))
            .on_action(cx.listener(|this, _: &BrowserOpenExternal, _, cx| cx.open_url(&this.url)))
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Button::new("back").xsmall().ghost().label("←").on_click(cx.listener(|this, _, _, _| {
                        this.command("Runtime.evaluate", json!({ "expression": "history.back()" }))
                    })))
                    .child(Button::new("forward").xsmall().ghost().label("→").on_click(cx.listener(|this, _, _, _| {
                        this.command("Runtime.evaluate", json!({ "expression": "history.forward()" }))
                    })))
                    .child(Button::new("reload").xsmall().ghost().label("⟳").on_click(cx.listener(|this, _, _, _| {
                        this.command("Page.reload", json!({}))
                    })))
                    .child(div().flex_1().child(Input::new(&self.url_input).small()))
                    .child(
                        Button::new("pick")
                            .xsmall()
                            .ghost()
                            .label("⌖")
                            .selected(self.picking)
                            .tooltip("Pick an element for the agent: its selector, markup and picture go into the prompt")
                            .on_click(cx.listener(|this, _, window, cx| {
                                // Focused, so Esc gets here to leave picking.
                                this.focus.focus(window, cx);
                                this.set_picking(!this.picking, cx);
                            })),
                    )
                    .when_some(endpoint(), |this, endpoint| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("CDP {endpoint}")),
                        )
                    }),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().px_3().py_1().text_xs().text_color(gpui_kit::red()).child(error))
            })
            .child(
                div()
                    .id("page")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .bg(gpui_kit::white())
                    .track_focus(&self.focus)
                    .when(self.picking, |page| page.cursor(CursorStyle::Crosshair))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, window, cx| {
                        this.focus.focus(window, cx);
                        if !this.picking {
                            this.mouse("mousePressed", e.position, Some(e.button), e.click_count, &e.modifiers)
                        }
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(|this, e: &MouseDownEvent, window, cx| {
                        // Focus first, so the context menu's actions reach this view.
                        this.focus.focus(window, cx);
                        this.mouse("mousePressed", e.position, Some(e.button), e.click_count, &e.modifiers)
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, e: &MouseUpEvent, _, cx| {
                        if this.picking {
                            return this.pick_at(e.position, cx);
                        }
                        this.mouse("mouseReleased", e.position, Some(e.button), e.click_count, &e.modifiers)
                    }))
                    .on_mouse_up(MouseButton::Right, cx.listener(|this, e: &MouseUpEvent, _, _| {
                        this.mouse("mouseReleased", e.position, Some(e.button), e.click_count, &e.modifiers)
                    }))
                    .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                        // The page still sees the pointer, so what shows on hover can be picked.
                        this.mouse("mouseMoved", e.position, e.pressed_button, 0, &e.modifiers);
                        if this.picking {
                            this.hover_at(e.position, cx);
                        }
                    }))
                    .on_scroll(cx.listener(|this, e: &ScrollEvent, window, _| {
                        let delta = e.delta.pixel_delta(window.line_height());
                        let (x, y) = this.pointer(e.position);
                        this.command(
                            "Input.dispatchMouseEvent",
                            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": -f32::from(delta.x), "deltaY": -f32::from(delta.y) }),
                        );
                    }))
                    .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                        this.key(e, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        // Tracks the page area: Chrome renders at exactly this
                        // size and the window's scale, so frames are 1:1.
                        canvas(
                            move |b, _, _| {
                                bounds.set(b);
                                let size = (f32::from(b.size.width) as u32, f32::from(b.size.height) as u32, (scale * 100.) as u32);
                                if size != sent.get() && size.0 > 0 && size.1 > 0 {
                                    sent.set(size);
                                    // A running screencast keeps its old size: restart it.
                                    let _ = out.send(Out::Command("Page.stopScreencast", json!({})));
                                    let _ = out.send(Out::Command(
                                        "Emulation.setDeviceMetricsOverride",
                                        json!({ "width": size.0, "height": size.1, "deviceScaleFactor": scale, "mobile": false }),
                                    ));
                                    let _ = out.send(Out::Command(
                                        "Page.startScreencast",
                                        json!({ "format": "jpeg", "quality": 85, "maxWidth": (size.0 as f32 * scale) as u32, "maxHeight": (size.1 as f32 * scale) as u32 }),
                                    ));
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .when_some(self.frame.clone(), |this, frame| {
                        // 1:1, never stretched: while a resize waits for frames
                        // of the new size, the old one stays sharp at its size.
                        let size = frame.size(0);
                        let (w, h) = (size.width.0 as f32 / scale, size.height.0 as f32 / scale);
                        this.child(img(frame).absolute().top_0().left_0().w(px(w)).h(px(h)))
                    })
                    .when_some(self.hover.filter(|_| self.picking), |this, hover| {
                        let color = cx.theme().primary;
                        this.child(
                            div()
                                .absolute()
                                .left(hover.origin.x)
                                .top(hover.origin.y)
                                .w(hover.size.width)
                                .h(hover.size.height)
                                .border_1()
                                .border_color(color)
                                .bg(color.opacity(0.2)),
                        )
                    })
                    // Headless Chrome has no menu of its own: that is browser UI.
                    .context_menu(|menu, _, _| {
                        menu.menu("Back", Box::new(BrowserBack))
                            .menu("Forward", Box::new(BrowserForward))
                            .menu("Reload", Box::new(BrowserReload))
                            .separator()
                            .menu("Copy", Box::new(BrowserCopy))
                            .menu("Paste", Box::new(BrowserPaste))
                            .separator()
                            .menu("Copy page address", Box::new(BrowserCopyUrl))
                            .menu("Open in system browser", Box::new(BrowserOpenExternal))
                    })
            )
    }
}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs Chrome (or `IDEAE_CHROME`): `cargo test -- --ignored pictures`.
    #[core::prelude::v1::test]
    #[ignore]
    fn pictures_a_page_at_the_size_asked() {
        let out = std::env::temp_dir().join(format!("ideae-shot-test-{}.png", std::process::id()));
        shot("data:text/html,<body style='margin:0;background:rgb(0,128,0)'>", 640, 400, &out).unwrap();
        let image = image::open(&out).unwrap().into_rgba8();
        assert_eq!(image.dimensions(), (640, 400));
        assert!(image.pixels().all(|p| p.0 == [0, 128, 0, 255]), "the page, all of the picture");
        assert!(shot("http://127.0.0.1:9/", 640, 400, &out).is_err(), "nothing serves there");
        let _ = std::fs::remove_file(&out);
        // The picture's Chrome ends with the test: on macOS nothing else ends it.
        if let Some(chrome) = CHROME.get().and_then(|slot| slot.lock().ok()?.take()) {
            drop(Arc::try_unwrap(chrome));
        }
    }

    /// Needs Chrome (or `IDEAE_CHROME`): `cargo test -- --ignored picks`.
    /// Rust's own `test`: the glob above brings in the kit's.
    #[core::prelude::v1::test]
    #[ignore]
    fn picks_an_element_out_of_a_scrolled_page() {
        let page = "data:text/html,<body style='margin:0'><div style='height:1500px'></div>\
            <ul id='users'><li class='row'>Ada</li><li class='row'>\
            <a class='name' href='/b' style='display:inline-block;width:120px;height:30px;background:rgb(255,0,0);color:rgb(255,0,0)'>Grace Hopper</a>\
            </li></ul><div style='height:1500px'></div>";
        /// Ends the test's Chrome, however the test ends: on macOS nothing else does.
        struct Quit;
        impl Drop for Quit {
            fn drop(&mut self) {
                let Some(mut chrome) = CHROME.get().and_then(|slot| slot.lock().ok()?.take()) else { return };
                let started = Instant::now();
                // The page's thread lets go of it once the page is closed.
                while started.elapsed() < Duration::from_secs(10) {
                    match Arc::try_unwrap(chrome) {
                        Ok(owned) => return drop(owned),
                        Err(shared) => chrome = shared,
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        let _quit = Quit;
        let (frames, _kept) = async_channel::bounded(4);
        let out = spawn_page(page.into(), frames);
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
        let message = pick(&out, x, y, &dir).expect("the element, described");
        assert!(message.contains("the element `a.name`:"), "the shortest selector that finds it alone: {message}");
        assert!(message.contains("<a class=\"name\" href=\"/b\"") && message.contains("text: \"Grace Hopper\""), "{message}");
        let picture = message.lines().find_map(|l| l.strip_prefix("picture: ")).expect("a picture");
        let image = image::open(picture).unwrap().into_rgba8();
        assert_eq!(image.dimensions(), (240, 60));
        assert!(image.pixels().all(|p| p.0 == [255, 0, 0, 255]), "the picture is the link and nothing around it");

        let at = eval("(() => { const r = document.querySelector('li').getBoundingClientRect(); return [r.right - 5, r.top + 5] })()").unwrap();
        let message = pick(&out, at[0].as_f64().unwrap() as f32, at[1].as_f64().unwrap() as f32, &dir).expect("the first row");
        assert!(message.contains("the element `li.row:nth-of-type(1)`:"), "one of two alike: {message}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
