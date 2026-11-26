//! A `.http` file that sends its requests (crate::http_client): a line over
//! its text with Run and the environment, the answer under it, as a `.sql`
//! file has its results; and the HTTP panel of the side: the project's
//! `.http` files and the requests this task sent.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use gpui_kit::component::menu::PopupMenuItem;

use super::*;
use crate::http_client::{self as client, Answer, Environments, Outcome, Ready};
use crate::http_port as port;

actions!(ideae_http, [RunRequest, RunAllRequests]);

/// The key context of a `.http` file's editor.
pub const HTTP_CONTEXT: &str = "HttpFile";
/// Where its keys work: Ctrl+Enter is a new line in any other input.
pub const HTTP_INPUT_CONTEXT: &str = "HttpFile > Input";

/// Answers a file keeps, the last first.
const ANSWERS_KEPT: usize = 50;
/// Bytes of bodies they may hold together: the older go first, the one in front stays.
const ANSWERS_BYTES: usize = 64 << 20;
/// Requests the panel lists.
const SENT_KEPT: usize = 50;
/// Characters of a body the view holds: the rest is a Copy away.
const BODY_SHOWN: usize = 2 << 20;

thread_local! {
    /// The environment chosen for each file, while the app runs.
    static CHOSEN: RefCell<HashMap<PathBuf, Option<String>>> = RefCell::default();
}

pub(super) fn is_http(path: &std::path::Path) -> bool {
    client::is_http(path)
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Body,
    Headers,
    Request,
    /// What its scripts tested and logged.
    Tests,
}

/// A request sent, and what came of it.
struct Sent {
    title: SharedString,
    ready: Option<Ready>,
    outcome: Result<Answer, String>,
    tests: Vec<client::Test>,
    logs: Vec<String>,
    script_error: Option<String>,
    skipped: Vec<String>,
}

impl Sent {
    fn of(title: SharedString, outcome: Outcome) -> Self {
        let Outcome { ready, answer, tests, logs, script_error, skipped } = outcome;
        Self { title, ready, outcome: answer, tests, logs, script_error, skipped }
    }

    fn not_sent(error: String) -> Self {
        Self { title: "Not sent".into(), ready: None, outcome: Err(error), tests: Vec::new(), logs: Vec::new(), script_error: None, skipped: Vec::new() }
    }

    /// The bytes it holds: what a file's answers are bounded by.
    fn weight(&self) -> usize {
        self.outcome.as_ref().map_or(0, |answer| answer.body.len()) + self.ready.as_ref().map_or(0, |ready| ready.body.len())
    }

    fn scripted(&self) -> bool {
        !self.tests.is_empty() || !self.logs.is_empty() || self.script_error.is_some()
    }

    fn tests_failed(&self) -> bool {
        self.script_error.is_some() || self.tests.iter().any(|t| !t.ok)
    }
}

/// How requests go, as Settings has it.
fn options() -> client::Options {
    crate::state::with_settings(|s| client::Options {
        timeout: Duration::from_secs(u64::from(s.http_timeout_secs.max(1))),
        follow_redirects: s.http_follow_redirects,
        verify_local_tls: s.http_verify_tls,
        scripts: s.http_scripts,
    })
}

pub(super) struct HttpFile {
    envs: Rc<Environments>,
    env: Option<String>,
    env_errors: Vec<String>,
    _envs: Task<()>,
    /// The request on its way.
    running: Option<SharedString>,
    _run: Task<()>,
    answers: VecDeque<Arc<Sent>>,
    /// The environment was chosen by hand: none is then none, not the first.
    chosen: bool,
    /// Counts what the view was asked to show: a text laid out for another is dropped.
    filled: u64,
    _fill: Task<()>,
    shown: usize,
    view: View,
    /// What the view shows of the answer in front: its body, headers or the request.
    text: Option<Entity<EditorState>>,
    panel: bool,
}

impl HttpFile {
    pub(super) fn new(path: &std::path::Path) -> Self {
        Self {
            envs: Rc::default(),
            env: CHOSEN.with_borrow(|chosen| chosen.get(path).cloned().flatten()),
            chosen: CHOSEN.with_borrow(|chosen| chosen.contains_key(path)),
            filled: 0,
            _fill: Task::ready(()),
            env_errors: Vec::new(),
            _envs: Task::ready(()),
            running: None,
            _run: Task::ready(()),
            answers: VecDeque::new(),
            shown: 0,
            view: View::Body,
            text: None,
            panel: false,
        }
    }
}

/// A request this task sent, for the panel.
#[derive(Clone)]
pub(super) struct SentLine {
    path: PathBuf,
    line: u32,
    method: SharedString,
    url: SharedString,
    status: Option<u16>,
    /// The request, or a test of its answer.
    failed: bool,
}

fn language_of(content_type: Option<&str>) -> &'static str {
    let Some(kind) = content_type.map(str::to_ascii_lowercase) else { return "text" };
    if kind.contains("json") {
        "json"
    } else if kind.contains("html") {
        "html"
    } else if kind.contains("xml") {
        "xml"
    } else if kind.contains("javascript") {
        "javascript"
    } else if kind.contains("css") {
        "css"
    } else {
        "text"
    }
}

fn status_color(status: u16) -> Hsla {
    match status {
        200..=299 => gpui_kit::green(),
        300..=399 => hsla(0.12, 0.85, 0.5, 1.),
        _ => gpui_kit::red(),
    }
}

fn size_said(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

fn took_said(took: Duration) -> String {
    let ms = took.as_millis();
    if ms < 1000 { format!("{ms} ms") } else { format!("{:.2} s", took.as_secs_f64()) }
}

/// What a view shows of a sent request, and in which language.
fn view_text(sent: &Sent, view: View) -> (String, &'static str) {
    match (view, &sent.outcome) {
        (View::Tests, _) => {
            let mut text = String::new();
            if let Some(error) = &sent.script_error {
                text.push_str(&format!("{error}\n\n"));
            }
            for test in &sent.tests {
                text.push_str(&if test.ok { format!("✓ {}\n", test.name) } else { format!("✗ {}: {}\n", test.name, test.message) });
            }
            if !sent.logs.is_empty() {
                text.push_str("\nLog:\n");
                for line in &sent.logs {
                    text.push_str(&format!("{line}\n"));
                }
            }
            if text.is_empty() {
                text.push_str("No scripts ran: > {% client.test(\"name\", () => client.assert(response.status === 200)) %} after a request tests its answer.");
            }
            (text, "text")
        }
        (View::Request, _) => {
            let Some(ready) = &sent.ready else { return ("It was not sent.".into(), "text") };
            // As it went, with its keys and tokens in: not for an audience.
            if crate::workspace::present::on() {
                return (format!("{} {}\n\nIts headers and body are hidden while presenting.", ready.method, ready.url.split('?').next().unwrap_or_default()), "text");
            }
            let mut text = format!("{} {}\n", ready.method, ready.url);
            for (name, value) in &ready.headers {
                text.push_str(&format!("{name}: {value}\n"));
            }
            if !ready.body.is_empty() {
                text.push('\n');
                text.push_str(&String::from_utf8_lossy(&ready.body));
            }
            (text, "text")
        }
        (_, Err(error)) => (error.clone(), "text"),
        (View::Headers, Ok(answer)) => {
            let mut text = format!("{} {} {}\n", answer.version, answer.status, answer.reason);
            for (name, value) in &answer.headers {
                text.push_str(&format!("{name}: {value}\n"));
            }
            (text, "text")
        }
        (View::Body, Ok(answer)) => {
            let language = language_of(answer.content_type());
            let raw = String::from_utf8_lossy(&answer.body);
            let pretty = (language == "json").then(|| serde_json::from_str::<serde_json::Value>(&raw).ok().and_then(|v| serde_json::to_string_pretty(&v).ok())).flatten();
            let text = pretty.unwrap_or_else(|| raw.into_owned());
            let text = if text.chars().count() > BODY_SHOWN { format!("{}\n… (cut: Copy takes it whole)", text.chars().take(BODY_SHOWN).collect::<String>()) } else { text };
            (text, language)
        }
    }
}

/// A request as a curl command: to send it from a terminal, or to pass it on.
fn as_curl(ready: &Ready) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let mut command = format!("curl -X {} {}", ready.method, quote(&ready.url));
    for (name, value) in &ready.headers {
        command.push_str(&format!(" \\\n  -H {}", quote(&format!("{name}: {value}"))));
    }
    if !ready.body.is_empty() {
        command.push_str(&format!(" \\\n  --data-binary {}", quote(&String::from_utf8_lossy(&ready.body))));
    }
    if !ready.verify_tls {
        command.push_str(" \\\n  -k");
    }
    command
}

impl FileView {
    pub(super) fn http_panel_open(&self) -> bool {
        self.http.as_ref().is_some_and(|h| h.panel)
    }

    pub(super) fn http_busy(&self) -> bool {
        self.http.as_ref().is_some_and(|h| h.running.is_some())
    }

    /// The environments near the file, read again: an env file may have changed.
    pub(super) fn load_http_envs(&mut self, cx: &mut Context<Self>) {
        let Some(http) = &mut self.http else { return };
        let (path, root) = (self.path.clone(), self.root.clone());
        http._envs = cx.spawn(async move |this: WeakEntity<FileView>, cx| {
            let (envs, errors) = cx.background_spawn(async move { client::environments(&path, &root) }).await;
            let _ = this.update(cx, |this, cx| {
                let path = this.path.clone();
                let Some(http) = &mut this.http else { return };
                // Until one is chosen, the first that is not the real thing:
                // what is there is meant to be used, production only when asked for.
                let gone = http.env.as_ref().is_some_and(|env| !envs.names.contains(env));
                if !http.chosen || gone {
                    let safe = |name: &&String| !client::is_production(name);
                    http.env = envs.names.iter().find(|name| matches!(name.as_str(), "dev" | "local" | "development")).or_else(|| envs.names.iter().find(safe)).cloned();
                }
                let _ = path;
                http.envs = Rc::new(envs);
                http.env_errors = errors;
                cx.notify();
            });
        });
    }

    fn choose_env(&mut self, env: Option<String>, cx: &mut Context<Self>) {
        let path = self.path.clone();
        if let Some(http) = &mut self.http {
            CHOSEN.with_borrow_mut(|chosen| chosen.insert(path, env.clone()));
            http.env = env;
            http.chosen = true;
            cx.notify();
        }
    }

    /// Sends the request the cursor is in.
    pub(super) fn run_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_requests(false, window, cx);
    }

    /// Every request of the file, in its order, until one fails.
    pub(super) fn run_all_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_requests(true, window, cx);
    }

    fn run_requests(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(http) = &self.http else { return };
        if http.running.is_some() {
            return;
        }
        // To production only once it is unlocked, as its databases are.
        if http.env.as_deref().is_some_and(client::is_production) && !crate::db::guard::is_open() {
            let this = cx.weak_entity();
            let then: super::sql_guard::Then = Rc::new(move |window, cx| {
                let _ = this.update(cx, |file, cx| file.run_requests(all, window, cx));
            });
            return super::sql_guard::production_gate(then, window, cx);
        }
        let editor = self.editor.read(cx);
        let (rope, cursor) = (editor.text().clone(), editor.cursor());
        let (path, root, env) = (self.path.clone(), self.root.clone(), http.env.clone());
        let dir = path.parent().map(std::path::Path::to_path_buf).unwrap_or_else(|| root.clone());
        let options = options();
        let env_name = env.clone();
        let run = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            // Read and parsed off the UI thread; the environment read again, as it may have changed.
            let scope = root.clone();
            let read = cx
                .background_spawn(async move {
                    let text = rope.to_string();
                    let (requests, file_vars) = client::parse(&text);
                    let line_of = |request: &client::Request| text.get(..request.line.start).map_or(0, |before| before.matches('\n').count()) as u32;
                    let picked: Vec<(client::Request, u32)> = if all {
                        requests.iter().map(|r| (r.clone(), line_of(r))).collect()
                    } else {
                        client::at(&requests, cursor).map(|r| (r.clone(), line_of(r))).into_iter().collect()
                    };
                    let (envs, _) = client::environments(&path, &root);
                    (picked, file_vars, envs.variables(env.as_deref()), envs.unchecked(env.as_deref()))
                })
                .await;
            let (picked, file_vars, env, unchecked) = read;
            if picked.is_empty() {
                let _ = this.update_in(cx, |this, window, cx| this.show_sent(Sent::not_sent("No request here: a line such as GET https://example.com".into()), window, cx));
                return;
            }
            let (count, mut done, mut stopped, mut refused) = (picked.len(), 0, None, 0);
            let (file_vars, env) = (Arc::new(file_vars), Arc::new(env));
            for (request, line) in picked {
                let title: SharedString = request.title().into();
                let shown = this.update(cx, |this, cx| {
                    if let Some(http) = &mut this.http {
                        http.running = Some(if all { format!("{} of {count}: {title}", done + 1).into() } else { title.clone() });
                        cx.notify();
                    }
                });
                if shown.is_err() {
                    return;
                }
                let (file_vars, env, dir, scope, env_name) = (file_vars.clone(), env.clone(), dir.clone(), scope.clone(), env_name.clone());
                let outcome = cx
                    .background_spawn(async move {
                        let place = client::Place { dir: &dir, root: &scope, env: env_name.as_deref(), unchecked };
                        client::run(&request, &file_vars, &env, &place, &options)
                    })
                    .await;
                let failed = outcome.failed();
                refused += usize::from(outcome.answer.as_ref().is_ok_and(|answer| answer.status >= 400));
                done += 1;
                let _ = this.update_in(cx, |this, window, cx| {
                    if let Some(ready) = &outcome.ready {
                        let status = outcome.answer.as_ref().ok().map(|a| a.status);
                        // Without its query: a token is often there, and the list is in view of anyone.
                        let url: SharedString = ready.url.split('?').next().unwrap_or_default().to_string().into();
                        let sent = SentLine { path: this.path.clone(), line, method: ready.method.clone().into(), url, status, failed };
                        if let Some(task) = this.task.upgrade() {
                            task.update(cx, |task, cx| task.http_sent(sent, cx));
                        }
                    }
                    this.show_sent(Sent::of(title.clone(), outcome), window, cx);
                });
                if failed && all {
                    stopped = Some(title);
                    break;
                }
            }
            if all {
                let _ = this.update_in(cx, |_, window, cx| {
                    use gpui_kit::component::notification::Notification;
                    let note = match stopped {
                        Some(title) => Notification::error(format!("Stopped at {title}: {done} of {count} requests ran")),
                        None if refused > 0 => Notification::warning(format!("{count} requests sent, {refused} answered with an error status")),
                        None => Notification::success(format!("All {count} requests sent, none failed")),
                    };
                    window.push_notification(note, cx);
                });
            }
        });
        if let Some(http) = &mut self.http {
            http._run = run;
        }
    }

    pub(super) fn stop_request(&mut self, cx: &mut Context<Self>) {
        if let Some(http) = &mut self.http {
            // The request's thread ends on its own; what it brings goes nowhere.
            http._run = Task::ready(());
            http.running = None;
            cx.notify();
        }
    }

    fn show_sent(&mut self, sent: Sent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(http) = &mut self.http else { return };
        http.running = None;
        // A failed test is what to look at; else the answer.
        http.view = if sent.tests_failed() { View::Tests } else if matches!(http.view, View::Request | View::Tests) { View::Body } else { http.view };
        http.answers.push_front(Arc::new(sent));
        http.answers.truncate(ANSWERS_KEPT);
        let mut held = 0;
        let within = http.answers.iter().take_while(|sent| {
            held += sent.weight();
            held <= ANSWERS_BYTES
        });
        let keep = within.count().max(1);
        http.answers.truncate(keep);
        http.shown = 0;
        http.panel = true;
        self.fill_http_view(window, cx);
    }

    /// Lays out what the view shows of the answer in front, off the UI thread:
    /// a long body is parsed and set out there.
    fn fill_http_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(http) = &mut self.http else { return };
        let Some(sent) = http.answers.get(http.shown).cloned() else { return };
        http.filled += 1;
        let (view, filled) = (http.view, http.filled);
        http._fill = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            let (text, language) = cx.background_spawn(async move { view_text(&sent, view) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.http.as_ref().is_none_or(|http| http.filled != filled) {
                    return;
                }
                let editor = cx.new(|cx| EditorState::new(window, cx).language(language).line_number(true).soft_wrap(true).searchable(true));
                editor.update(cx, |state, cx| state.set_value(text, window, cx));
                if let Some(http) = &mut this.http {
                    http.text = Some(editor);
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn show_http(&mut self, shown: Option<usize>, view: Option<View>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(http) = &mut self.http else { return };
        if let Some(shown) = shown {
            http.shown = shown;
        }
        if let Some(view) = view {
            http.view = view;
        }
        self.fill_http_view(window, cx);
    }

    fn copy_http_body(&self, cx: &mut Context<Self>) {
        let Some(sent) = self.http.as_ref().and_then(|h| h.answers.get(h.shown)) else { return };
        let text = match &sent.outcome {
            Ok(answer) => String::from_utf8_lossy(&answer.body).into_owned(),
            Err(error) => error.clone(),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn copy_as_curl(&self, cx: &mut Context<Self>) {
        if let Some(ready) = self.http.as_ref().and_then(|h| h.answers.get(h.shown)).and_then(|sent| sent.ready.as_ref()) {
            cx.write_to_clipboard(ClipboardItem::new_string(as_curl(ready)));
        }
    }

    /// The request at the cursor as curl, without sending it: its scripts do not run.
    fn copy_request_as_curl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(http) = &self.http else { return };
        let editor = self.editor.read(cx);
        let (rope, cursor) = (editor.text().clone(), editor.cursor());
        let (path, root, env) = (self.path.clone(), self.root.clone(), http.env.clone());
        let options = options();
        cx.spawn_in(window, async move |_, cx| {
            let made = cx
                .background_spawn(async move {
                    let (requests, file_vars) = client::parse(&rope.to_string());
                    let request = client::at(&requests, cursor).ok_or("No request here.")?;
                    let (envs, _) = client::environments(&path, &root);
                    let mut variables = envs.variables(env.as_deref());
                    variables.extend(client::globals(&root).iter().map(|(name, value)| (name.clone(), client::global_text(value))));
                    variables.extend(file_vars);
                    let place = client::Place { dir: path.parent().unwrap_or(&root), root: &root, env: env.as_deref(), unchecked: envs.unchecked(env.as_deref()) };
                    client::prepare(request, &variables, &place, &options).map(|ready| as_curl(&ready))
                })
                .await;
            let _ = cx.update(|window, cx| {
                use gpui_kit::component::notification::Notification;
                match made {
                    Ok(command) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(command));
                        window.push_notification(Notification::success("Copied as curl"), cx);
                    }
                    Err(error) => window.push_notification(Notification::error(error), cx),
                }
            });
        })
        .detach();
    }

    /// A curl command in the clipboard, added to the file as a request.
    pub(super) fn paste_curl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::notification::Notification;
        let Some(command) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return window.push_notification(Notification::error("The clipboard has no text"), cx);
        };
        match port::from_curl(&command) {
            Ok(request) => self.editor.update(cx, |state, cx| {
                let end = state.text().len();
                let lead = if end == 0 { "" } else { "\n\n" };
                state.set_selected_range(end..end, cx);
                state.replace(format!("{lead}###\n{request}"), window, cx);
            }),
            Err(error) => window.push_notification(Notification::error(error), cx),
        }
    }

    /// The file's requests written as a Postman collection, where asked.
    fn export_postman(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rope = self.editor.read(cx).text().clone();
        let name = self.path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "requests".into());
        let dir = self.path.parent().unwrap_or(&self.root).to_path_buf();
        let picked = cx.prompt_for_new_path(&dir, Some(&format!("{name}.postman_collection.json")));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(target))) = picked.await else { return };
            let written = cx
                .background_spawn(async move {
                    let (requests, file_vars) = client::parse(&rope.to_string());
                    std::fs::write(&target, port::to_postman(&name, &requests, &file_vars)).map(|()| (target, requests.len())).map_err(|e| e.to_string())
                })
                .await;
            let _ = cx.update(|window, cx| {
                use gpui_kit::component::notification::Notification;
                let note = match written {
                    Ok((target, count)) => Notification::success(format!("{count} requests written to {}", target.display())),
                    Err(error) => Notification::error(error),
                };
                window.push_notification(note, cx);
            });
        })
        .detach();
    }

    pub(super) fn render_http_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let http = self.http.as_ref()?;
        let this = cx.weak_entity();
        let busy = http.running.is_some();
        let names = http.envs.names.clone();
        let chosen = http.env.clone();
        let env_label = match (&chosen, names.is_empty()) {
            (Some(env), _) => format!("Env: {env} ▾"),
            (None, true) => "No environment ▾".to_string(),
            (None, false) => "No environment ▾".to_string(),
        };
        let files: Vec<PathBuf> = http.envs.files.clone();
        let (root, dir) = (self.root.clone(), self.path.parent().map(std::path::Path::to_path_buf));
        let production = chosen.as_deref().is_some_and(client::is_production);
        let envs = Button::new("http-env").xsmall().ghost().child(div().when(production, |label| label.text_color(gpui_kit::red()).font_semibold()).child(env_label)).tooltip(format!("Variables from {} and {}", client::ENV_FILE, client::PRIVATE_ENV_FILE)).dropdown_menu(move |mut menu, _, _| {
            {
                let this = this.clone();
                menu = menu.item(PopupMenuItem::new("No environment").checked(chosen.is_none()).on_click(move |_, _, cx| {
                    let _ = this.update(cx, |f, cx| f.choose_env(None, cx));
                }));
            }
            for name in &names {
                let (this, env) = (this.clone(), name.clone());
                menu = menu.item(PopupMenuItem::new(name.clone()).checked(chosen.as_ref() == Some(name)).on_click(move |_, _, cx| {
                    let _ = this.update(cx, |f, cx| f.choose_env(Some(env.clone()), cx));
                }));
            }
            menu = menu.separator();
            for file in &files {
                let (this, file) = (this.clone(), file.clone());
                let name = file.strip_prefix(&root).unwrap_or(&file).display().to_string();
                menu = menu.item(PopupMenuItem::new(format!("Open {name}")).on_click(move |_, window, cx| {
                    let _ = this.update(cx, |f, cx| f.open_in_task(file.clone(), window, cx));
                }));
            }
            if files.is_empty() {
                let (this, dir) = (this.clone(), dir.clone());
                menu = menu.item(PopupMenuItem::new(format!("Create {}", client::ENV_FILE)).on_click(move |_, window, cx| {
                    let Some(dir) = dir.clone() else { return };
                    let _ = this.update(cx, |f, cx| f.create_env_file(dir, window, cx));
                }));
            }
            let this = this.clone();
            menu.item(PopupMenuItem::new("Read the environments again").on_click(move |_, _, cx| {
                let _ = this.update(cx, |f, cx| f.load_http_envs(cx));
            }))
        });
        let state = http.running.clone().map(|title| format!("sending {title}…"));
        let errors = (!http.env_errors.is_empty()).then(|| http.env_errors.join("; "));
        Some(
            h_flex()
                .id("http-bar")
                .test_support()
                .flex_none()
                .flex_wrap()
                .gap_1()
                .px_2()
                .py(px(2.))
                .items_center()
                .text_xs()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    Button::new("http-run")
                        .xsmall()
                        .ghost()
                        .label("▶ Run")
                        .disabled(busy)
                        .tooltip(format!("The request at the cursor ({})", crate::keymap::label("http_run")))
                        .on_click(cx.listener(|this, _, window, cx| this.run_request(window, cx))),
                )
                .child(
                    Button::new("http-run-all")
                        .xsmall()
                        .ghost()
                        .label("Run all")
                        .disabled(busy)
                        .tooltip(format!("Every request of the file in its order, until one fails ({})", crate::keymap::label("http_run_all")))
                        .on_click(cx.listener(|this, _, window, cx| this.run_all_requests(window, cx))),
                )
                .child(Button::new("http-stop").xsmall().ghost().label("■").disabled(!busy).tooltip("Stop waiting for the answer").on_click(cx.listener(|this, _, _, cx| this.stop_request(cx))))
                .child(envs)
                .child(Button::new("http-port").xsmall().ghost().label("Import / Export ▾").dropdown_menu({
                    let this = cx.weak_entity();
                    move |menu, _, _| {
                        let (paste, curl, postman, forget) = (this.clone(), this.clone(), this.clone(), this.clone());
                        menu.item(PopupMenuItem::new("Paste a curl command as a request").on_click(move |_, window, cx| {
                            let _ = paste.update(cx, |f, cx| f.paste_curl(window, cx));
                        }))
                        .separator()
                        .item(PopupMenuItem::new("Copy the request at the cursor as curl").on_click(move |_, window, cx| {
                            let _ = curl.update(cx, |f, cx| f.copy_request_as_curl(window, cx));
                        }))
                        .item(PopupMenuItem::new("Export the file as a Postman collection…").on_click(move |_, window, cx| {
                            let _ = postman.update(cx, |f, cx| f.export_postman(window, cx));
                        }))
                        .separator()
                        .item(PopupMenuItem::new("Forget cookies and what scripts kept (client.global)").on_click(move |_, _, cx| {
                            let _ = forget.update(cx, |f, _| client::forget(&f.root));
                        }))
                    }
                }))
                .when_some(errors, |bar, errors| bar.child(div().flex_none().text_color(gpui_kit::red()).child(errors)))
                .child(div().flex_1())
                .when_some(state, |bar, state| bar.child(div().flex_none().text_color(cx.theme().muted_foreground).child(state)))
                .into_any_element(),
        )
    }

    /// Opens `path` in the task, once this file is no longer being updated:
    /// the task reads the file in front to note where one came from.
    fn open_in_task(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.task.clone();
        window.defer(cx, move |window, cx| {
            let _ = task.update(cx, |task, cx| task.open_file(path, 0, window, cx));
        });
    }

    fn create_env_file(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = dir.join(client::ENV_FILE);
        let template = "{\n  \"dev\": {\n    \"host\": \"http://localhost:8080\"\n  },\n  \"prod\": {\n    \"host\": \"https://example.com\"\n  }\n}\n";
        cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            let target = path.clone();
            let written = cx.background_spawn(async move { crate::hostfs::write(&target, template.as_bytes()) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = written {
                    return this.show_sent(Sent::not_sent(format!("{}: {e}", path.display())), window, cx);
                }
                this.load_http_envs(cx);
                this.open_in_task(path, window, cx);
            });
        })
        .detach();
    }

    pub(super) fn render_http_results(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let http = self.http.as_ref().filter(|h| h.panel)?;
        let muted = cx.theme().muted_foreground;
        let sent = http.answers.get(http.shown);
        let summary = sent.map(|sent| match &sent.outcome {
            Ok(answer) => h_flex()
                .gap_2()
                .child(div().font_semibold().text_color(status_color(answer.status)).child(format!("{} {}", answer.status, answer.reason)))
                .child(div().text_color(muted).child(took_said(answer.took)))
                .child(div().text_color(muted).child(if answer.cut { format!("{}+ (cut)", size_said(answer.body.len())) } else { size_said(answer.body.len()) }))
                .into_any_element(),
            Err(_) => div().font_semibold().text_color(gpui_kit::red()).child("Failed").into_any_element(),
        });
        let tests_label = sent.filter(|s| s.scripted()).map(|s| {
            let passed = s.tests.iter().filter(|t| t.ok).count();
            let label = if s.script_error.is_some() { "Tests: script failed".to_string() } else { format!("Tests {passed}/{}", s.tests.len()) };
            (label, if s.tests_failed() { gpui_kit::red() } else { gpui_kit::green() })
        });
        let skipped = sent.filter(|s| !s.skipped.is_empty()).map(|s| format!("Not run here: {}", s.skipped.join(", ")));
        let tab = |id: &'static str, label: &'static str, view: View, cx: &mut Context<Self>| {
            Button::new(id).xsmall().ghost().selected(http.view == view).label(label).on_click(cx.listener(move |this, _, window, cx| this.show_http(None, Some(view), window, cx)))
        };
        let history = {
            let this = cx.weak_entity();
            let entries: Vec<(usize, SharedString, Option<u16>)> = http.answers.iter().enumerate().map(|(ix, s)| (ix, s.title.clone(), s.outcome.as_ref().ok().map(|a| a.status))).collect();
            let label = sent.map_or_else(|| SharedString::from("Answers"), |s| s.title.clone());
            let shown = http.shown;
            Button::new("http-answers").xsmall().ghost().label(format!("{label} ▾")).dropdown_menu(move |mut menu, _, _| {
                for (ix, title, status) in &entries {
                    let (this, ix) = (this.clone(), *ix);
                    let status = status.map_or_else(|| "failed".to_string(), |s| s.to_string());
                    menu = menu.item(PopupMenuItem::new(format!("{status}  {title}")).checked(ix == shown).on_click(move |_, window, cx| {
                        let _ = this.update(cx, |f, cx| f.show_http(Some(ix), None, window, cx));
                    }));
                }
                menu
            })
        };
        let header = h_flex()
            .id("http-answer-bar")
            .test_support()
            .flex_none()
            .flex_wrap()
            .gap_1()
            .px_2()
            .py(px(2.))
            .items_center()
            .text_xs()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(summary)
            .child(div().w(px(8.)))
            .child(tab("http-body", "Body", View::Body, cx))
            .child(tab("http-headers", "Headers", View::Headers, cx))
            .child(tab("http-request", "Request", View::Request, cx))
            .child(match tests_label {
                Some((label, color)) => Button::new("http-tests").xsmall().ghost().selected(http.view == View::Tests).child(div().text_color(color).child(label)).on_click(cx.listener(|this, _, window, cx| this.show_http(None, Some(View::Tests), window, cx))),
                None => tab("http-tests", "Tests", View::Tests, cx),
            })
            .child(history)
            .when_some(skipped, |bar, skipped| bar.child(div().flex_none().text_color(hsla(0.08, 0.9, 0.5, 1.)).child(skipped)))
            .child(div().flex_1())
            .child(Button::new("http-copy").xsmall().ghost().label("Copy").tooltip("The body as it came").on_click(cx.listener(|this, _, _, cx| this.copy_http_body(cx))))
            .child(Button::new("http-curl").xsmall().ghost().label("Copy as curl").on_click(cx.listener(|this, _, _, cx| this.copy_as_curl(cx))))
            .child(Button::new("http-close").xsmall().ghost().label("×").tooltip("Hide the answer").on_click(cx.listener(|this, _, _, cx| {
                if let Some(http) = &mut this.http {
                    http.panel = false;
                    cx.notify();
                }
            })));
        let body = match &http.text {
            Some(text) => div()
                .flex_1()
                .min_h_0()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(crate::zoom::size(crate::zoom::Kind::Editor, cx))
                .child(Editor::new(text).readonly(true).bordered(false).p_0().h_full())
                .into_any_element(),
            None => div().p_3().text_sm().text_color(muted).child(format!("{}: the request at the cursor", crate::keymap::label("http_run"))).into_any_element(),
        };
        Some(v_flex().id("http-results").test_support().size_full().child(header).child(body).into_any_element())
    }
}

// ---------------------------------------------------------------- the panel

impl TaskView {
    /// The `.http` files among the project's: only when the list read changed.
    pub(super) fn find_http_files(&mut self, files: &[String]) {
        self.http_files = Rc::new(files.iter().filter(|f| client::is_http(std::path::Path::new(f.as_str()))).map(|f| SharedString::from(f.clone())).collect());
    }

    pub(super) fn http_sent(&mut self, sent: SentLine, cx: &mut Context<Self>) {
        let mut all: VecDeque<SentLine> = self.http_sent.iter().cloned().collect();
        all.push_front(sent);
        all.truncate(SENT_KEPT);
        self.http_sent = Rc::new(all.into_iter().collect());
        cx.notify();
    }

    pub(super) fn http_panel_tools(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let muted = cx.theme().muted_foreground;
        let drawn = move |id: &'static str, drawing: &'static str, tip: &'static str| Button::new(id).xsmall().ghost().child(svg().path(drawing).size(px(14.)).text_color(muted)).tooltip(tip);
        vec![
            drawn("http-new", "side-icons/file-plus.svg", "New .http file").on_click(cx.listener(|this, _, window, cx| this.new_http_file(window, cx))).into_any_element(),
            drawn("http-import-curl", "side-icons/clipboard-paste.svg", "A curl command in the clipboard, as a request in imported.http")
                .on_click(cx.listener(|this, _, window, cx| this.import_curl(window, cx)))
                .into_any_element(),
            drawn("http-import-postman", "side-icons/archive-restore.svg", "Import a Postman collection…").on_click(cx.listener(|this, _, window, cx| this.import_postman(window, cx))).into_any_element(),
        ]
    }

    /// A curl command in the clipboard, added to `imported.http`.
    fn import_curl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::notification::Notification;
        let Some(command) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return window.push_notification(Notification::error("The clipboard has no text"), cx);
        };
        let request = match port::from_curl(&command) {
            Ok(request) => request,
            Err(error) => return window.push_notification(Notification::error(error), cx),
        };
        let secret = request.lines().any(|line| {
            let line = line.to_ascii_lowercase();
            ["authorization:", "cookie:", "x-api-key:", "x-auth-token:"].iter().any(|name| line.starts_with(name))
        });
        let path = self.root.join("imported.http");
        // Open with edits not saved, the file on disk is not the one to add to.
        let open = self.tabs.iter().find_map(|tab| match &tab.kind {
            TabKind::File(file) if file.read(cx).path == path => Some(file.clone()),
            _ => None,
        });
        if let Some(file) = open {
            file.update(cx, |file, cx| file.paste_curl(window, cx));
            return self.open_file(path, 0, window, cx);
        }
        cx.spawn_in(window, async move |this, cx| {
            let target = path.clone();
            let written = cx
                .background_spawn(async move {
                    // A file that is there and cannot be read is not written over.
                    let before = match crate::hostfs::read_to_string(&target) {
                        Ok(before) => before,
                        Err(_) if crate::hostfs::stat(&target).is_err() => String::new(),
                        Err(e) => return Err(e),
                    };
                    let line = before.matches('\n').count() as u32 + if before.is_empty() { 1 } else { 3 };
                    let lead = if before.is_empty() { "" } else { "\n\n" };
                    crate::hostfs::write(&target, format!("{}{lead}###\n{request}", before.trim_end()).as_bytes()).map(|()| line)
                })
                .await;
            let _ = this.update_in(cx, |this: &mut TaskView, window, cx| match written {
                Ok(line) => {
                    if secret {
                        window.push_notification(Notification::warning("imported.http now holds a key, a password or a cookie from that command: keep the file out of git, or move the value to http-client.private.env.json."), cx);
                    }
                    this.open_file(path, line, window, cx)
                }
                Err(e) => window.push_notification(Notification::error(format!("imported.http: {e}")), cx),
            });
        })
        .detach();
    }

    /// A Postman collection chosen, written as `.http` files under `http/`.
    fn import_postman(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::notification::Notification;
        let picked = cx.prompt_for_paths(gpui_kit::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import".into()) });
        let root = self.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(source) = paths.into_iter().next() else { return };
            let written = cx
                .background_spawn(async move {
                    let text = std::fs::read_to_string(&source).map_err(|e| format!("{}: {e}", source.display()))?;
                    let files = port::from_postman(&text)?;
                    // Never over a file that is there: it may be yours, changed since.
                    // Looked for before any is written, so an import is whole or not at all.
                    if let Some((name, _)) = files.iter().find(|(name, _)| crate::hostfs::stat(&root.join("http").join(name)).is_ok()) {
                        return Err(format!("http/{name} is there already: move it away, then import again."));
                    }
                    let mut made = Vec::new();
                    for (name, content) in files {
                        let path = root.join("http").join(&name);
                        if let Some(dir) = path.parent() {
                            // A remote task's folder is made there.
                            if crate::hosts::is_remote(&root) {
                                let _ = crate::hostfs::run(&root, "mkdir", &["-p", &dir.to_string_lossy()], &[], None);
                            } else {
                                let _ = std::fs::create_dir_all(dir);
                            }
                        }
                        crate::hostfs::write(&path, content.as_bytes()).map_err(|e| format!("http/{name}: {e}"))?;
                        made.push(path);
                    }
                    Ok::<_, String>(made)
                })
                .await;
            let _ = this.update_in(cx, |this: &mut TaskView, window, cx| match written {
                Ok(made) => {
                    window.push_notification(Notification::success(format!("{} .http files written to http/", made.len())), cx);
                    if let Some(first) = made.into_iter().next() {
                        this.open_file(first, 0, window, cx);
                    }
                }
                Err(error) => window.push_notification(Notification::error(error), cx),
            });
        })
        .detach();
    }

    fn new_http_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let taken: Vec<SharedString> = self.http_files.to_vec();
        let name = (1..).map(|n| if n == 1 { "requests.http".to_string() } else { format!("requests-{n}.http") }).find(|name| !taken.iter().any(|t| t.as_ref() == name)).unwrap_or_default();
        let path = root.join(&name);
        let template = format!(
            "### {run} sends the request the cursor is in; ### starts the next one.\n\
             # Variables: {{{{name}}}} from {env} (Env ▾ above), or @name = value here.\n\
             GET https://httpbin.org/get\n\
             Accept: application/json\n\
             \n\
             ###\n\
             POST https://httpbin.org/post\n\
             Content-Type: application/json\n\
             \n\
             {{\n  \"id\": \"{{{{$uuid}}}}\"\n}}\n",
            run = crate::keymap::label("http_run"),
            env = client::ENV_FILE,
        );
        cx.spawn_in(window, async move |this, cx| {
            let target = path.clone();
            // Not over a file of that name the list has not caught up with.
            let written = cx
                .background_spawn(async move {
                    if crate::hostfs::stat(&target).is_ok() {
                        return Ok(());
                    }
                    crate::hostfs::write(&target, template.as_bytes())
                })
                .await;
            let _ = this.update_in(cx, |this: &mut TaskView, window, cx| match written {
                Ok(()) => this.open_file(path, 2, window, cx),
                Err(e) => window.push_notification(gpui_kit::component::notification::Notification::error(format!("{name}: {e}")), cx),
            });
        })
        .detach();
    }

    pub(super) fn render_http_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let (files, sent) = (self.http_files.clone(), self.http_sent.clone());
        let mut panel = v_flex().id("http-panel").size_full().overflow_y_scroll().py_1().text_sm();
        if files.is_empty() && sent.is_empty() {
            return panel
                .child(
                    v_flex()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .child(div().font_semibold().child("HTTP requests"))
                        .child(div().text_xs().text_color(muted).child(format!(
                            "Requests in .http files, as JetBrains' HTTP Client writes them: {} sends the one at the cursor, \
                             with variables from {}.",
                            crate::keymap::label("http_run"),
                            client::ENV_FILE
                        )))
                        .child(Button::new("http-first").small().outline().label("New .http file").on_click(cx.listener(|this, _, window, cx| this.new_http_file(window, cx)))),
                )
                .into_any_element();
        }
        let heading = |text: &'static str| div().px_3().pt_2().pb_1().text_xs().text_color(muted).child(text);
        let line = |id: (&'static str, usize)| h_flex().id(id).gap_2().px_3().py(px(3.)).cursor_pointer().hover(|this| this.bg(cx.theme().secondary));
        if !files.is_empty() {
            panel = panel.child(heading("FILES"));
            for (ix, file) in files.iter().enumerate() {
                let path = self.root.join(file.as_ref());
                panel = panel.child(
                    line(("http-file", ix))
                        .child(svg().path("side-icons/send.svg").size(px(13.)).text_color(muted))
                        .child(div().truncate().child(file.clone()))
                        .on_click(cx.listener(move |this, _, window, cx| this.open_file(path.clone(), 0, window, cx))),
                );
            }
        }
        if !sent.is_empty() {
            panel = panel.child(heading("SENT"));
            for (ix, s) in sent.iter().enumerate() {
                let (path, at) = (s.path.clone(), s.line);
                let status = s.status.map_or_else(|| ("failed".to_string(), gpui_kit::red()), |status| (status.to_string(), if s.failed { gpui_kit::red() } else { status_color(status) }));
                panel = panel.child(
                    line(("http-sent", ix))
                        .child(div().flex_none().w(px(42.)).text_color(status.1).child(status.0))
                        .child(div().flex_none().font_semibold().text_xs().child(s.method.clone()))
                        .child(div().truncate().text_color(muted).child(s.url.clone()))
                        .on_click(cx.listener(move |this, _, window, cx| this.open_file(path.clone(), at, window, cx))),
                );
            }
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{language_of, size_said};

    #[test]
    fn an_answer_is_shown_in_the_language_of_its_type() {
        assert_eq!(language_of(Some("application/json; charset=utf-8")), "json");
        assert_eq!(language_of(Some("application/problem+json")), "json");
        assert_eq!(language_of(Some("text/html")), "html");
        assert_eq!(language_of(Some("text/plain")), "text");
        assert_eq!(language_of(None), "text");
        assert_eq!(size_said(512), "512 B");
        assert_eq!(size_said(2048), "2.0 KB");
    }
}
