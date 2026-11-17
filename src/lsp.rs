//! Minimal stdio LSP client: hover, go to definition, implementations and
//! references. Calls block, so run them on the background executor.

use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicI64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};

type Pending = Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value>>>>>;
/// Work-done progress the server has begun and not ended: token → title.
type Progress = Arc<Mutex<HashMap<String, String>>>;
type Import = Arc<Mutex<ImportState>>;
/// What the server last found wrong in each file, by uri, and a count of
/// its reports: a file's view looks again when that moved.
type Opened = Arc<Mutex<HashSet<String>>>;
type Problems = Arc<Mutex<(u64, HashMap<String, (u64, Vec<lsp_types::Diagnostic>)>)>>;

/// The server's import of the build: its errors so far, and how it ended.
#[derive(Default)]
struct ImportState {
    log: Vec<String>,
    failed: Option<String>,
}

/// The kinds of names a server tells apart, and the theme's name for the
/// color of each. Only what a grammar cannot know: whether a name is a
/// type, a parameter, a method. Strings, comments and keywords stay the
/// grammar's, which sees more in them (escapes, doc comments).
const SEMANTIC_TYPES: [(&str, &str); 16] = [
    ("namespace", ""),
    ("type", "type"),
    ("class", "type"),
    ("struct", "type"),
    ("interface", "type"),
    ("typeParameter", "type"),
    ("enum", "enum"),
    ("enumMember", "constant"),
    ("parameter", "variable.special"),
    ("variable", "variable"),
    ("property", "property"),
    ("function", "function"),
    ("method", "function"),
    ("macro", "preproc"),
    ("decorator", "attribute"),
    ("event", "property"),
];

/// The signature of the call being written, and where in it the parameter
/// the cursor is at stands.
pub fn signature_shown(help: &lsp_types::SignatureHelp) -> Option<(String, Option<std::ops::Range<usize>>)> {
    let signature = help.signatures.get(help.active_signature.unwrap_or(0) as usize).or(help.signatures.first())?;
    let active = signature.active_parameter.or(help.active_parameter).unwrap_or(0) as usize;
    let label = &signature.label;
    let range = signature.parameters.as_ref().and_then(|all| all.get(active)).and_then(|parameter| match &parameter.label {
        lsp_types::ParameterLabel::Simple(name) => label.find(name.as_str()).map(|start| start..start + name.len()),
        lsp_types::ParameterLabel::LabelOffsets([start, end]) => {
            // Counted in UTF-16 units, as the protocol does.
            let byte = |units: u32| {
                let mut seen = 0;
                label.char_indices().find(|(_, c)| {
                    let here = seen >= units;
                    seen += c.len_utf16() as u32;
                    here
                })
                .map_or(label.len(), |(at, _)| at)
            };
            Some(byte(*start)..byte(*end))
        }
    });
    Some((label.clone(), range.filter(|r| r.start < r.end)))
}

/// What a declaration is, from LSP's SymbolKind.
pub fn symbol_kind(kind: u32) -> &'static str {
    match kind {
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        22 => "enum member",
        23 => "struct",
        26 => "type parameter",
        _ => "",
    }
}

/// A declaration somewhere in the project, as `workspace/symbol` names it.
#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    /// LSP's SymbolKind: 5 a class, 12 a function, 23 a struct.
    pub kind: u32,
    pub container: String,
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
}

pub struct LspClient {
    /// To the thread that writes to the server.
    out: mpsc::Sender<Out>,
    pending: Pending,
    next_id: AtomicI64,
    ready: (Mutex<Option<Result<(), String>>>, Condvar),
    opened: Opened,
    progress: Progress,
    import: Import,
    problems: Problems,
    /// `capabilities` from the server's answer to `initialize`.
    capabilities: Mutex<Value>,
    child: Mutex<Option<Child>>,
}

/// Power saving: no language servers run, none start.
pub static POWER_SAVING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The server for a file: its language id, its name, the executable to run
/// (a name on PATH or a path ideae installed it at) and its arguments.
pub struct Server {
    pub language_id: &'static str,
    pub program: &'static str,
    pub command: PathBuf,
    pub args: &'static [&'static str],
}

/// A language server ideae can run: the program, its arguments, and the
/// file extensions it serves with their LSP language ids. The one list:
/// Settings shows what is here.
pub struct ServerDef {
    pub program: &'static str,
    pub args: &'static [&'static str],
    pub languages: &'static [(&'static str, &'static str)],
}

pub const SERVERS: &[ServerDef] = &[
    ServerDef { program: "gopls", args: &[], languages: &[("go", "go")] },
    ServerDef { program: "rust-analyzer", args: &[], languages: &[("rs", "rust")] },
    ServerDef {
        program: "typescript-language-server",
        args: &["--stdio"],
        languages: &[("ts", "typescript"), ("tsx", "typescriptreact"), ("js", "javascript"), ("jsx", "javascriptreact")],
    },
    ServerDef { program: "intelephense", args: &["--stdio"], languages: &[("php", "php")] },
    ServerDef { program: "kotlin-lsp", args: &["--stdio"], languages: &[("kt", "kotlin"), ("kts", "kotlin")] },
];

impl ServerDef {
    /// ".go", ".ts .tsx .js .jsx": what it is for, in Settings.
    pub fn extensions(&self) -> String {
        self.languages.iter().map(|(ext, _)| format!(".{ext}")).collect::<Vec<_>>().join(" ")
    }

    /// On PATH or installed by ideae.
    pub fn available(&self) -> bool {
        locate(self.program).is_some()
    }
}

/// The extension a file is read by, past a template's `.dist` and the like.
fn extension_of(path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    template_of(name).rsplit_once('.').map(|(_, ext)| ext.to_string()).unwrap_or_default()
}

fn known(path: &Path) -> Option<(&'static str, &'static str, &'static [&'static str])> {
    let ext = extension_of(path);
    SERVERS.iter().find_map(|def| {
        def.languages.iter().find(|(e, _)| *e == ext).map(|(_, language)| (*language, def.program, def.args))
    })
}

/// Which server handles a file, by extension, or why there is none, for the
/// editor's status line.
pub fn server_for(path: &Path) -> Result<Server, String> {
    if POWER_SAVING.load(Ordering::Relaxed) {
        return Err("language servers off (power saving)".into());
    }
    let ext = extension_of(path);
    let (language_id, program, args) = known(path).ok_or_else(|| format!("no language server for .{ext}"))?;
    if crate::state::settings().lsp_off.iter().any(|p| p == program) {
        return Err(format!("{program} off in Settings"));
    }
    let command = locate(program).ok_or_else(|| format!("{program} not installed"))?;
    Ok(Server { language_id, program, command, args })
}

/// The server a file would have, when it is neither on PATH nor installed.
pub fn missing_server(path: &Path) -> Option<&'static str> {
    if POWER_SAVING.load(Ordering::Relaxed) {
        return None;
    }
    known(path).map(|(_, program, _)| program).filter(|program| locate(program).is_none())
}

/// PATH first, then ideae's own copy.
fn locate(program: &str) -> Option<PathBuf> {
    if runs(program) {
        return Some(PathBuf::from(program));
    }
    crate::lsp_install::installed(program)
}

/// Whether the server is on PATH. Found there is enough, except for
/// rust-analyzer: rustup puts a proxy on PATH that fails when the component
/// is missing, so that one has to answer `--version`. Others cannot be asked
/// (intelephense crashes without a transport flag).
fn runs(program: &str) -> bool {
    static CACHE: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
    if let Some(known) = CACHE.lock().unwrap().as_ref().and_then(|cache| cache.get(program).copied()) {
        return known;
    }
    // Not under the lock: asking takes a process, and the UI thread asks too.
    let found = on_path(program)
        && (program != "rust-analyzer"
            || crate::procs::command(program)
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success()));
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(program.to_string(), found);
    found
}

/// Finds out which servers are there before a file asks, off the UI thread.
pub fn look_for_servers() {
    for def in SERVERS {
        runs(def.program);
    }
}

pub fn on_path(program: &str) -> bool {
    crate::procs::which(program).is_some()
}

/// The directory a server should treat as its workspace: the outermost
/// ancestor of `file` holding one of the server's project markers, no higher
/// than `limit`. Dependencies inside a project carry markers of their own
/// (vendor/*/Cargo.toml, vendor/*/composer.json, node_modules/*/package.json,
/// included Gradle builds), and rooting at them would start one more server
/// indexing it all again, gigabytes apiece. Projects side by side in a repo
/// without a root marker still get one server each.
pub fn project_root(program: &str, file: &Path, limit: &Path) -> PathBuf {
    let markers: &[&str] = match program {
        "gopls" => &["go.work", "go.mod"],
        "rust-analyzer" => &["Cargo.toml"],
        "typescript-language-server" => &["tsconfig.json", "package.json"],
        "intelephense" => &["composer.json"],
        "kotlin-lsp" => &["settings.gradle.kts", "settings.gradle"],
        _ => &[],
    };
    let mut found = None;
    let mut dir = file.parent();
    while let Some(d) = dir {
        if markers.iter().any(|m| d.join(m).is_file()) {
            found = Some(d.to_path_buf());
        }
        if d == limit {
            break;
        }
        dir = d.parent();
    }
    found.unwrap_or_else(|| limit.to_path_buf())
}

/// The name a copy to fill in or keep aside stands for: `phpunit.xml.dist`
/// is read as `phpunit.xml`, `.env.example` as `.env`.
pub fn template_of(name: &str) -> &str {
    for suffix in [".dist", ".example", ".sample", ".template", ".orig", ".bak"] {
        if name.len() > suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix) {
            return &name[..name.len() - suffix.len()];
        }
    }
    name
}

/// Highlighter language name for the editor, the same table gpui-kit's diff
/// uses to detect a file's language (its own is crate-private).
pub fn language_for(path: &Path) -> &'static str {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_lowercase();
    let name = template_of(&name).to_string();
    match name.as_str() {
        "makefile" | "gnumakefile" => return "make",
        "cmakelists.txt" => return "cmake",
        "cargo.lock" => return "toml",
        "composer.lock" => return "json",
        ".editorconfig" | ".gitconfig" | ".npmrc" => return "ini",
        "jenkinsfile" => return "groovy",
        "gemfile" | "rakefile" | "podfile" | "vagrantfile" | "fastfile" | "brewfile" | "guardfile" | "appfile" => return "ruby",
        ".bashrc" | ".bash_profile" | ".bash_aliases" | ".bash_logout" | ".zshrc" | ".zprofile" | ".zshenv" | ".zlogin"
        | ".profile" | ".envrc" | "pkgbuild" | "apkbuild" => return "bash",
        "build.bazel" | "workspace.bazel" | "tiltfile" | "sconstruct" | "sconscript" => return "python",
        "pipfile" | "poetry.lock" | "uv.lock" | "gopkg.lock" => return "toml",
        "flake.lock" | "package.resolved" | ".babelrc" | ".jshintrc" => return "json",
        ".clang-format" | ".clang-tidy" | ".yamllint" => return "yaml",
        ".gitmodules" | ".pylintrc" | ".flake8" | ".coveragerc" | ".pypirc" | ".hgrc" => return "ini",
        _ => {}
    }
    if name.starts_with("dockerfile") || name.starts_with("containerfile") || name.ends_with(".dockerfile") {
        return "dockerfile";
    }
    // .env, .env.local, .env.example: KEY=value lines and # comments.
    if name == ".env" || name.starts_with(".env.") || name.ends_with(".env") {
        return "properties";
    }
    if name.ends_with(".conf") && path.to_string_lossy().contains("nginx") {
        return "nginx";
    }
    match name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or_default() {
        "go" => "go",
        "rs" => "rust",
        "kt" | "kts" => "kotlin",
        "java" => "java",
        "swift" => "swift",
        "scala" => "scala",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hxx" | "hh" | "h++" | "ipp" | "tpp" | "inl" | "ino" | "cu" | "cuh" | "mm" => "cpp",
        "cs" | "csx" => "csharp",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "php" | "phtml" => "php",
        "py" | "pyi" | "pyw" | "bzl" | "star" => "python",
        "rb" | "rake" | "gemspec" | "ru" | "podspec" => "ruby",
        "lua" => "lua",
        "zig" => "zig",
        "ex" | "exs" => "elixir",
        "erb" => "erb",
        "ejs" => "ejs",
        "svelte" => "svelte",
        "astro" => "astro",
        "json" | "jsonc" | "json5" | "jsonl" | "ndjson" | "geojson" | "webmanifest" | "ipynb" | "har" => "json",
        "md" | "mdx" | "markdown" | "mkd" => "markdown",
        "toml" => "toml",
        "yaml" | "yml" | "neon" => "yaml",
        "sql" | "psql" | "pgsql" | "ddl" => "sql",
        "graphql" | "gql" | "graphqls" => "graphql",
        "proto" => "proto",
        "sh" | "bash" | "zsh" | "ksh" | "bats" => "bash",
        "css" | "pcss" | "postcss" => "css",
        // tree-sitter-scss passes MSVC a flag it rejects; CSS's grammar
        // reads most of SCSS.
        "scss" | "sass" => "css",
        "less" => "less",
        "twig" => "twig",
        "dart" => "dart",
        "nix" => "nix",
        "hs" | "lhs" => "haskell",
        "r" => "r",
        "ml" | "mli" => "ocaml",
        "erl" | "hrl" => "erlang",
        "fish" => "fish",
        "glsl" | "vert" | "frag" | "geom" | "comp" | "tesc" | "tese" => "glsl",
        "sol" => "solidity",
        "jl" => "julia",
        "elm" => "elm",
        "gleam" => "gleam",
        "prisma" => "prisma",
        "html" | "htm" | "xhtml" | "shtml" | "vue" => "html",
        "cmake" => "cmake",
        "diff" | "patch" => "diff",
        "xml" | "svg" | "plist" | "xsd" | "xsl" | "xslt" | "csproj" | "fsproj" | "vbproj" | "props" | "targets" | "xaml"
        | "axaml" | "resx" | "nuspec" | "iml" | "wsdl" | "fxml" | "storyboard" | "xib" | "rss" | "atom" | "xlf" | "xliff"
        | "kml" | "gpx" | "pom" | "jelly" => "xml",
        "properties" => "properties",
        "gradle" | "groovy" | "gvy" => "groovy",
        "bat" | "cmd" => "batch",
        "ini" | "cfg" | "desktop" | "service" | "timer" | "socket" | "mount" | "target" | "path" | "inf" => "ini",
        "ps1" | "psm1" | "psd1" => "powershell",
        "tf" | "tfvars" | "hcl" | "nomad" => "hcl",
        "mk" | "mak" => "make",
        _ => "text",
    }
}

/// `/a/b.rs` → `file:///a/b.rs`; `C:\a\b.rs` → `file:///C:/a/b.rs`.
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = if cfg!(windows) { format!("/{}", text.replace('\\', "/")) } else { text.into_owned() };
    let mut uri = String::from("file://");
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'-' | b'.' | b'_' | b'~' | b'+' | b'@' => uri.push(b as char),
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let path = decode_uri_path(uri)?;
    if !cfg!(windows) {
        return Some(PathBuf::from(path));
    }
    // `/c:/a/b.rs`, as servers write it, is `C:\a\b.rs`, the drive in capitals
    // as the rest of the app has it.
    let path = path.strip_prefix('/').filter(|p| p.as_bytes().get(1) == Some(&b':')).unwrap_or(&path);
    let mut path = path.replace('/', "\\");
    if path.as_bytes().get(1) == Some(&b':') {
        path[..1].make_ascii_uppercase();
    }
    Some(PathBuf::from(path))
}

fn decode_uri_path(uri: &str) -> Option<String> {
    let raw = uri.strip_prefix("file://")?;
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

impl LspClient {
    /// Starts the server; initialization runs on its own thread, and every
    /// call waits for it.
    pub fn spawn(program: &str, executable: &Path, args: &[&str], root: &Path, init_options: Value) -> Result<Arc<Self>> {
        let mut command = crate::procs::command(executable);
        command.args(args).envs(server_env(program)).current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        // A group of its own: what it starts (cargo check, a proc-macro
        // server, tsserver) is ended with it, not left to notice.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        // Dies with the app even when it exits without running destructors.
        let mut child = crate::procs::spawn_tied(&mut command)
            .with_context(|| format!("starting {program}"))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let (out, outbox) = mpsc::channel();
        thread::Builder::new().name(format!("lsp-write-{program}")).spawn(move || write_loop(stdin, outbox))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let pending: Pending = Arc::default();
        let progress: Progress = Arc::default();

        let reader_out = out.clone();
        let reader_pending = pending.clone();
        let reader_progress = progress.clone();
        let import: Import = Arc::default();
        let reader_import = import.clone();
        let problems: Problems = Arc::default();
        let reader_problems = problems.clone();
        let opened: Opened = Arc::default();
        let reader_opened = opened.clone();
        thread::Builder::new()
            .name(format!("lsp-read-{program}"))
            .spawn(move || read_loop(stdout, reader_out, reader_pending, reader_progress, reader_import, reader_problems, reader_opened))?;

        let client = Arc::new(Self {
            out,
            pending,
            next_id: AtomicI64::new(1),
            ready: (Mutex::new(None), Condvar::new()),
            opened,
            progress,
            import,
            problems,
            capabilities: Mutex::new(Value::Null),
            child: Mutex::new(Some(child)),
        });

        let init = client.clone();
        let root = root.to_path_buf();
        thread::Builder::new()
            .name(format!("lsp-init-{program}"))
            .spawn(move || {
                let result = init.initialize(&root, init_options).map_err(|e| e.to_string());
                if result.is_ok() {
                    let _ = init.out.send(Out::Ready);
                }
                let (lock, cvar) = &init.ready;
                *lock.lock().unwrap() = Some(result);
                cvar.notify_all();
            })?;
        Ok(client)
    }

    fn initialize(&self, root: &Path, init_options: Value) -> Result<()> {
        let uri = path_to_uri(root);
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let answer = self.request(
            true,
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": uri,
                "workspaceFolders": [{ "uri": uri, "name": name }],
                "initializationOptions": init_options,
                "capabilities": {
                    // The editor counts columns in characters.
                    "general": { "positionEncodings": ["utf-32", "utf-16"] },
                    "textDocument": {
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        "definition": { "linkSupport": true },
                        "implementation": { "linkSupport": true },
                        "references": {},
                        "synchronization": { "didSave": false },
                        // Plain text to insert: the editor has no snippets.
                        "completion": { "completionItem": { "snippetSupport": false, "documentationFormat": ["markdown", "plaintext"] } },
                        "publishDiagnostics": {},
                        "codeAction": {
                            "codeActionLiteralSupport": { "codeActionKind": { "valueSet": ["quickfix", "refactor", "source"] } },
                        },
                        "rename": {},
                        "formatting": {},
                        "signatureHelp": {
                            "signatureInformation": {
                                "documentationFormat": ["plaintext", "markdown"],
                                "parameterInformation": { "labelOffsetSupport": true },
                                "activeParameterSupport": true,
                            },
                        },
                        "inlayHint": {},
                        "semanticTokens": {
                            "requests": { "range": true, "full": true },
                            "tokenTypes": SEMANTIC_TYPES.map(|(name, _)| name),
                            "tokenModifiers": [],
                            "formats": ["relative"],
                            "overlappingTokenSupport": false,
                            "multilineTokenSupport": false,
                        },
                    },
                    "workspace": {
                        "workspaceFolders": true,
                        "configuration": true,
                        "workspaceEdit": { "documentChanges": true },
                        "symbol": {},
                    },
                    "window": { "workDoneProgress": true },
                },
            }),
        )?;
        *self.capabilities.lock().unwrap() = answer["capabilities"].clone();
        self.send(Out::Now(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} })))
    }

    fn wait_ready(&self) -> Result<()> {
        let (lock, cvar) = &self.ready;
        let guard = lock.lock().unwrap();
        let (guard, _) = cvar
            .wait_timeout_while(guard, Duration::from_secs(60), |r| r.is_none())
            .unwrap();
        match guard.as_ref() {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(anyhow!("language server failed to start: {e}")),
            None => Err(anyhow!("language server did not initialize in time")),
        }
    }

    /// Title of some work the server is still doing, such as indexing.
    pub fn busy(&self) -> Option<String> {
        self.progress.lock().unwrap().values().next().cloned()
    }

    /// Why the server knows less of the project than it should: its import
    /// of the build failed, so what other modules and libraries declare is
    /// not found.
    pub fn problem(&self) -> Option<String> {
        self.import.lock().unwrap().failed.clone()
    }

    pub fn did_open(&self, path: &Path, language_id: &str, text: &str) -> Result<()> {
        self.wait_ready()?;
        let uri = path_to_uri(path);
        if !self.opened.lock().unwrap().insert(uri.clone()) {
            return Ok(());
        }
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": language_id, "version": 1, "text": text } }),
        )
    }

    /// The file's tab closed: the server lets go of the text, and opening
    /// the file again sends it anew.
    pub fn did_close(&self, path: &Path) -> Result<()> {
        let uri = path_to_uri(path);
        if !self.opened.lock().unwrap().remove(&uri) {
            return Ok(());
        }
        self.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri } }))
    }

    /// Full-text sync: a change without a range replaces the whole document,
    /// which every server accepts whatever sync kind it prefers.
    pub fn did_change(&self, path: &Path, _version: i32, text: &str) -> Result<()> {
        let uri = path_to_uri(path);
        // Before its file is opened there the server has nothing to change:
        // the text it is opened with is as new.
        if !self.opened.lock().unwrap().contains(&uri) {
            return Ok(());
        }
        // Its version is given where it is written, in the order written.
        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": { "textDocument": { "uri": uri, "version": 0 }, "contentChanges": [{ "text": text }] },
        });
        self.send(Out::Change { uri, message })
    }

    pub fn did_save(&self, path: &Path) -> Result<()> {
        self.notify("textDocument/didSave", json!({ "textDocument": { "uri": path_to_uri(path) } }))
    }

    pub fn hover(&self, uri: &str, pos: lsp_types::Position) -> Result<Option<lsp_types::Hover>> {
        self.wait_ready()?;
        let value = self.raw_request("textDocument/hover", position_params(uri, pos))?;
        Ok(serde_json::from_value(value).unwrap_or(None))
    }

    pub fn definition(&self, uri: &str, pos: lsp_types::Position) -> Result<Vec<lsp_types::LocationLink>> {
        self.wait_ready()?;
        let value = self.raw_request("textDocument/definition", position_params(uri, pos))?;
        if value.is_null() {
            return Ok(vec![]);
        }
        let response: lsp_types::GotoDefinitionResponse = serde_json::from_value(value)?;
        let to_link = |l: lsp_types::Location| lsp_types::LocationLink {
            origin_selection_range: None,
            target_uri: l.uri,
            target_range: l.range,
            target_selection_range: l.range,
        };
        Ok(match response {
            lsp_types::GotoDefinitionResponse::Scalar(l) => vec![to_link(l)],
            lsp_types::GotoDefinitionResponse::Array(ls) => ls.into_iter().map(to_link).collect(),
            lsp_types::GotoDefinitionResponse::Link(links) => links,
        })
    }

    /// Whether the server offers `provider` (such as `referencesProvider`):
    /// `true` or options. Waits for initialization.
    pub fn supports(&self, provider: &str) -> bool {
        self.wait_ready().is_ok()
            && match &self.capabilities.lock().unwrap()[provider] {
                Value::Bool(b) => *b,
                Value::Null => false,
                _ => true,
            }
    }

    /// textDocument/implementation: the types implementing an interface or
    /// trait, the overrides of a method.
    pub fn implementation(&self, uri: &str, pos: lsp_types::Position) -> Result<Vec<lsp_types::Location>> {
        self.wait_ready()?;
        let value = self.raw_request("textDocument/implementation", position_params(uri, pos))?;
        if value.is_null() {
            return Ok(vec![]);
        }
        let response: lsp_types::GotoDefinitionResponse = serde_json::from_value(value)?;
        Ok(match response {
            lsp_types::GotoDefinitionResponse::Scalar(l) => vec![l],
            lsp_types::GotoDefinitionResponse::Array(ls) => ls,
            lsp_types::GotoDefinitionResponse::Link(links) => links
                .into_iter()
                .map(|l| lsp_types::Location { uri: l.target_uri, range: l.target_selection_range })
                .collect(),
        })
    }

    /// textDocument/references, without the declaration itself.
    pub fn references(&self, uri: &str, pos: lsp_types::Position) -> Result<Vec<lsp_types::Location>> {
        self.wait_ready()?;
        let mut params = position_params(uri, pos);
        params["context"] = json!({ "includeDeclaration": false });
        let value = self.raw_request("textDocument/references", params)?;
        Ok(serde_json::from_value::<Option<Vec<lsp_types::Location>>>(value)?.unwrap_or_default())
    }

    /// textDocument/completion at a position; `trigger` is the character
    /// that asked for it, when one did.
    pub fn completion(&self, uri: &str, pos: lsp_types::Position, trigger: Option<String>) -> Result<lsp_types::CompletionResponse> {
        self.wait_ready()?;
        let mut params = position_params(uri, pos);
        params["context"] = match trigger {
            Some(character) => json!({ "triggerKind": 2, "triggerCharacter": character }),
            None => json!({ "triggerKind": 1 }),
        };
        let value = self.raw_request("textDocument/completion", params)?;
        Ok(serde_json::from_value::<Option<lsp_types::CompletionResponse>>(value)?.unwrap_or(lsp_types::CompletionResponse::Array(Vec::new())))
    }

    /// The characters after which the server wants to be asked to complete.
    pub fn completion_triggers(&self) -> Vec<String> {
        let capabilities = self.capabilities.lock().unwrap();
        capabilities["completionProvider"]["triggerCharacters"]
            .as_array()
            .map(|chars| chars.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    }

    /// What the server last found wrong in the file, and the count of its
    /// reports so far: unchanged, nothing is new.
    pub fn problems(&self, uri: &str) -> (u64, Vec<lsp_types::Diagnostic>) {
        let problems = self.problems.lock().unwrap();
        problems.1.get(uri).cloned().unwrap_or_default()
    }

    /// The same, when it is another report than the one `seen`: asked twice
    /// a second by every open file, most often there is none, and one about
    /// another file is none.
    pub fn problems_since(&self, uri: &str, seen: u64) -> Option<(u64, Vec<lsp_types::Diagnostic>)> {
        let problems = self.problems.lock().unwrap();
        problems.1.get(uri).filter(|(report, _)| *report != seen).cloned()
    }

    /// textDocument/codeAction: the fixes and refactorings for a range,
    /// with what is wrong there. Only those that are edits: a command would
    /// have the server change files behind the editor.
    pub fn code_actions(&self, uri: &str, range: lsp_types::Range) -> Result<Vec<lsp_types::CodeAction>> {
        self.wait_ready()?;
        let inside = |d: &lsp_types::Diagnostic| d.range.start <= range.end && range.start <= d.range.end;
        let wrong: Vec<lsp_types::Diagnostic> = self.problems(uri).1.into_iter().filter(inside).collect();
        let params = json!({ "textDocument": { "uri": uri }, "range": range, "context": { "diagnostics": wrong, "triggerKind": 1 } });
        let value = self.raw_request("textDocument/codeAction", params)?;
        let found = serde_json::from_value::<Option<Vec<lsp_types::CodeActionOrCommand>>>(value)?.unwrap_or_default();
        Ok(found
            .into_iter()
            .filter_map(|action| match action {
                lsp_types::CodeActionOrCommand::CodeAction(action) => Some(action),
                lsp_types::CodeActionOrCommand::Command(_) => None,
            })
            .collect())
    }

    /// codeAction/resolve: the edit of an action the server listed without one.
    pub fn resolve_code_action(&self, action: &lsp_types::CodeAction) -> Result<lsp_types::CodeAction> {
        self.wait_ready()?;
        Ok(serde_json::from_value(self.raw_request("codeAction/resolve", serde_json::to_value(action)?)?)?)
    }

    /// workspace/symbol: what the project declares under a name like `query`.
    pub fn workspace_symbols(&self, query: &str) -> Result<Vec<Symbol>> {
        self.wait_ready()?;
        let value = self.raw_request("workspace/symbol", json!({ "query": query }))?;
        let found = value.as_array().map(Vec::as_slice).unwrap_or_default();
        Ok(found
            .iter()
            .filter_map(|symbol| {
                let location = &symbol["location"];
                Some(Symbol {
                    name: symbol["name"].as_str()?.to_string(),
                    kind: symbol["kind"].as_u64().unwrap_or(0) as u32,
                    container: symbol["containerName"].as_str().unwrap_or_default().to_string(),
                    path: uri_to_path(location["uri"].as_str()?)?,
                    line: location["range"]["start"]["line"].as_u64().unwrap_or(0) as u32,
                    col: location["range"]["start"]["character"].as_u64().unwrap_or(0) as u32,
                })
            })
            .collect())
    }

    /// textDocument/signatureHelp: the call the position is in, its
    /// parameters and which of them is being written.
    pub fn signature_help(&self, uri: &str, pos: lsp_types::Position) -> Result<Option<lsp_types::SignatureHelp>> {
        self.wait_ready()?;
        Ok(serde_json::from_value(self.raw_request("textDocument/signatureHelp", position_params(uri, pos))?)?)
    }

    /// The server's token types, each under the name the editor's theme
    /// colors it by; empty for those left to the grammar, and no types at
    /// all when the server has no semantic tokens to give.
    pub fn semantic_legend(&self) -> Vec<String> {
        let capabilities = self.capabilities.lock().unwrap();
        let themed = |name: &str| SEMANTIC_TYPES.iter().find(|(lsp, _)| *lsp == name).map_or("", |(_, theme)| theme).to_string();
        capabilities["semanticTokensProvider"]["legend"]["tokenTypes"]
            .as_array()
            .map(|types| types.iter().map(|t| themed(t.as_str().unwrap_or_default())).collect())
            .unwrap_or_default()
    }

    /// textDocument/semanticTokens: what each name in a range is, as the
    /// server knows it and a grammar cannot. For the whole file where the
    /// server answers only for that.
    pub fn semantic_tokens(&self, uri: &str, range: lsp_types::Range) -> Result<lsp_types::SemanticTokens> {
        self.wait_ready()?;
        let ranged = {
            let capabilities = self.capabilities.lock().unwrap();
            let range = &capabilities["semanticTokensProvider"]["range"];
            range.as_bool().unwrap_or(range.is_object())
        };
        let value = if ranged {
            self.raw_request("textDocument/semanticTokens/range", json!({ "textDocument": { "uri": uri }, "range": range }))?
        } else {
            self.raw_request("textDocument/semanticTokens/full", json!({ "textDocument": { "uri": uri } }))?
        };
        Ok(serde_json::from_value::<Option<lsp_types::SemanticTokens>>(value)?.unwrap_or_default())
    }

    /// What a server asks of its settings by `workspace/configuration`; null,
/// and it takes its defaults.
fn configuration(section: &str) -> Value {
    match section {
        // kotlin-lsp shows only the kinds of hints named here as on; with
        // none it shows none.
        "jetbrains.kotlin" => {
            const ON: &[&str] = &[
                "parameters",
                "parameters.compiled",
                "type.property",
                "type.variable",
                "type.function.return",
                "type.function.parameter",
                "lambda.return",
                "lambda.receivers.parameters",
                "value.ranges",
                "value.kotlin.time",
            ];
            json!({ "hints": ON.iter().map(|kind| (kind.to_string(), Value::Bool(true))).collect::<serde_json::Map<_, _>>() })
        }
        _ => Value::Null,
    }
}

/// textDocument/inlayHint: the types and parameter names the server
    /// would show in a range; none from a server that has no hints.
    pub fn inlay_hints(&self, uri: &str, range: lsp_types::Range) -> Result<Vec<lsp_types::InlayHint>> {
        self.wait_ready()?;
        let supported = {
            let capabilities = self.capabilities.lock().unwrap();
            let provider = &capabilities["inlayHintProvider"];
            provider.as_bool().unwrap_or(provider.is_object())
        };
        if !supported {
            return Ok(Vec::new());
        }
        let value = self.raw_request("textDocument/inlayHint", json!({ "textDocument": { "uri": uri }, "range": range }))?;
        Ok(serde_json::from_value::<Option<Vec<lsp_types::InlayHint>>>(value)?.unwrap_or_default())
    }

    /// textDocument/rename: every edit that gives the symbol its new name.
    pub fn rename(&self, uri: &str, pos: lsp_types::Position, name: &str) -> Result<Option<lsp_types::WorkspaceEdit>> {
        self.wait_ready()?;
        let mut params = position_params(uri, pos);
        params["newName"] = json!(name);
        Ok(serde_json::from_value(self.raw_request("textDocument/rename", params)?)?)
    }

    /// textDocument/formatting: the edits that format the file.
    pub fn formatting(&self, uri: &str, tab_size: u32, spaces: bool) -> Result<Vec<lsp_types::TextEdit>> {
        self.wait_ready()?;
        let params = json!({ "textDocument": { "uri": uri }, "options": { "tabSize": tab_size, "insertSpaces": spaces } });
        Ok(serde_json::from_value::<Option<Vec<lsp_types::TextEdit>>>(self.raw_request("textDocument/formatting", params)?)?.unwrap_or_default())
    }

    fn raw_request(&self, method: &str, params: Value) -> Result<Value> {
        self.request(false, method, params)
    }

    /// `early`: part of the handshake, sent before the server is ready.
    fn request(&self, early: bool, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let sent = self.send(if early { Out::Now(message) } else { Out::Later(message) });
        // Ends at once when the server is gone: the reader drops what waits.
        let result = sent.and_then(|()| rx.recv_timeout(Duration::from_secs(30)).map_err(|_| anyhow!("{method}: no response")));
        self.pending.lock().unwrap().remove(&id);
        result?
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(Out::Later(json!({ "jsonrpc": "2.0", "method": method, "params": params })))
    }

    /// Never waits: one thread writes, in the order asked.
    fn send(&self, out: Out) -> Result<()> {
        self.out.send(out).map_err(|_| anyhow!("the language server is gone"))
    }
}

impl LspClient {
    /// Ends the server now, even while files still hold the client.
    pub fn shut_down(&self) {
        let Some(mut child) = self.child.lock().unwrap().take() else { return };
        // SAFETY: a signal to the group the server was started as; not reaped yet, its id is still its own.
        #[cfg(unix)]
        unsafe {
            libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = child.kill();
        // Reaping one that held gigabytes takes a while: not on this thread,
        // which is the UI's more often than not.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// The edits of a workspace edit, by file.
pub fn edits_by_file(edit: &lsp_types::WorkspaceEdit) -> Vec<(PathBuf, Vec<lsp_types::TextEdit>)> {
    let mut found: Vec<(PathBuf, Vec<lsp_types::TextEdit>)> = Vec::new();
    let mut add = |uri: &str, edits: Vec<lsp_types::TextEdit>| {
        if let Some(path) = uri_to_path(uri) {
            match found.iter_mut().find(|(known, _)| *known == path) {
                Some((_, all)) => all.extend(edits),
                None => found.push((path, edits)),
            }
        }
    };
    for (uri, edits) in edit.changes.iter().flatten() {
        add(uri.as_str(), edits.clone());
    }
    let documents = match &edit.document_changes {
        Some(lsp_types::DocumentChanges::Edits(edits)) => edits.iter().collect::<Vec<_>>(),
        Some(lsp_types::DocumentChanges::Operations(operations)) => operations
            .iter()
            .filter_map(|operation| match operation {
                lsp_types::DocumentChangeOperation::Edit(edit) => Some(edit),
                lsp_types::DocumentChangeOperation::Op(_) => None,
            })
            .collect(),
        None => Vec::new(),
    };
    for document in documents {
        let edits = document
            .edits
            .iter()
            .map(|edit| match edit {
                lsp_types::OneOf::Left(edit) => edit.clone(),
                lsp_types::OneOf::Right(annotated) => annotated.text_edit.clone(),
            })
            .collect();
        add(document.text_document.uri.as_str(), edits);
    }
    found
}

/// `text` with the edits made: each range is of the text as it was.
/// Columns count characters, as the client told the server.
pub fn apply_edits(text: &str, edits: &[lsp_types::TextEdit]) -> String {
    let lines: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let offset = |pos: &lsp_types::Position| {
        let Some(&start) = lines.get(pos.line as usize) else { return text.len() };
        let line = &text[start..lines.get(pos.line as usize + 1).map_or(text.len(), |next| next - 1)];
        start + line.char_indices().nth(pos.character as usize).map_or(line.len(), |(i, _)| i)
    };
    let mut ranges: Vec<(usize, usize, &str)> = edits.iter().map(|e| (offset(&e.range.start), offset(&e.range.end), e.new_text.as_str())).collect();
    // From the end, so that what is before an edit keeps its place.
    ranges.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    // What is still as it was: an edit that reaches into one already made
    // (a server's mistake) is left out, not put into text it was not for.
    let mut untouched = text.len();
    ranges.retain(|&(start, end, _)| {
        let fits = start <= end && end <= untouched && text.is_char_boundary(start) && text.is_char_boundary(end);
        if fits {
            untouched = start;
        }
        fits
    });
    // Written out once, from the start: a formatter's thousands of small
    // edits do not each move the rest of the file.
    let mut out = String::with_capacity(text.len() + ranges.iter().map(|(_, _, new)| new.len()).sum::<usize>());
    let mut at = 0;
    for (start, end, new) in ranges.into_iter().rev() {
        out.push_str(&text[at..start]);
        out.push_str(new);
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

fn position_params(uri: &str, pos: lsp_types::Position) -> Value {
    json!({ "textDocument": { "uri": uri }, "position": { "line": pos.line, "character": pos.character } })
}

/// A message for the server.
enum Out {
    /// Of the handshake, or an answer to the server: sent as it comes.
    Now(Value),
    /// Kept until the server is ready, then sent in the order asked.
    Later(Value),
    /// A file's new text, kept as `Later`: of those waiting for one file
    /// only the newest is sent.
    Change { uri: String, message: Value },
    /// The handshake is over.
    Ready,
}

/// Writes to the server, alone: callers on many threads would write out of
/// order, a text before the file is opened or an older one after a newer,
/// and all of them would wait on a server that has stopped reading.
fn write_loop(mut stdin: ChildStdin, outbox: mpsc::Receiver<Out>) {
    let mut ready = false;
    let mut held: Vec<Out> = Vec::new();
    // A file is opened as its version 1.
    let mut version = 1i64;
    while let Ok(first) = outbox.recv() {
        let mut queue = Vec::new();
        for out in std::iter::once(first).chain(outbox.try_iter()) {
            match out {
                Out::Ready => {
                    ready = true;
                    queue.append(&mut held);
                }
                Out::Now(_) => queue.push(out),
                _ if ready => queue.push(out),
                _ => held.push(out),
            }
        }
        for waiting in [&mut queue, &mut held] {
            // A text is left out only when a newer one of the file follows
            // with nothing asked between them: what is asked is asked of
            // the text that went before it.
            let mut newer: HashSet<String> = HashSet::new();
            let mut keep = vec![true; waiting.len()];
            for (ix, out) in waiting.iter().enumerate().rev() {
                match out {
                    Out::Change { uri, .. } => keep[ix] = newer.insert(uri.clone()),
                    Out::Later(_) => newer.clear(),
                    _ => {}
                }
            }
            let mut keep = keep.into_iter();
            waiting.retain(|_| keep.next().unwrap_or(true));
        }
        for out in queue {
            let message = match out {
                Out::Now(message) | Out::Later(message) => message,
                // Numbered as it is written: a file's versions only grow, whichever thread asked.
                Out::Change { mut message, .. } => {
                    version += 1;
                    message["params"]["textDocument"]["version"] = version.into();
                    message
                }
                Out::Ready => continue,
            };
            if write_message(&mut stdin, &message).is_err() {
                return;
            }
        }
    }
}

fn write_message(stdin: &mut ChildStdin, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()?;
    Ok(())
}

fn read_loop(stdout: impl Read, out: mpsc::Sender<Out>, pending: Pending, progress: Progress, import: Import, problems: Problems, opened: Opened) {
    read_messages(stdout, out, &pending, progress, import, problems, opened);
    // The server is gone: what waits for its answer need not wait out its time.
    pending.lock().unwrap().clear();
}

fn read_messages(stdout: impl Read, out: mpsc::Sender<Out>, pending: &Pending, progress: Progress, import: Import, problems: Problems, opened: Opened) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut length = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                length = v.trim().parse::<usize>().ok();
            }
        }
        let Some(length) = length else { continue };
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&body) else { continue };

        match (message.get("id"), message.get("method")) {
            // A request from the server. Answer so it never waits on us.
            (Some(id), Some(method)) => {
                let result = if method == "workspace/configuration" {
                    let items = message["params"]["items"].as_array().map(Vec::as_slice).unwrap_or_default();
                    Value::Array(items.iter().map(|item| LspClient::configuration(item["section"].as_str().unwrap_or_default())).collect())
                } else {
                    Value::Null
                };
                let _ = out.send(Out::Now(json!({ "jsonrpc": "2.0", "id": id, "result": result })));
            }
            (Some(id), None) => {
                let Some(id) = id.as_i64() else { continue };
                if let Some(tx) = pending.lock().unwrap().remove(&id) {
                    let result = match message.get("error") {
                        Some(error) => Err(anyhow!("{}", error["message"].as_str().unwrap_or("lsp error"))),
                        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(result);
                }
            }
            (None, Some(method)) if method == "textDocument/publishDiagnostics" => {
                let params = &message["params"];
                let (Some(uri), Ok(found)) = (params["uri"].as_str(), serde_json::from_value::<Vec<lsp_types::Diagnostic>>(params["diagnostics"].clone())) else {
                    continue;
                };
                let mut problems = problems.lock().unwrap();
                // A server tells of every file it looked at, most with
                // nothing: kept only for an open one, whose editor clears
                // what was told before.
                if found.is_empty() && !opened.lock().unwrap().contains(uri) {
                    problems.1.remove(uri);
                    continue;
                }
                problems.0 += 1;
                let report = problems.0;
                problems.1.insert(uri.to_string(), (report, found));
            }
            (None, Some(method)) if method == "$/progress" => {
                let params = &message["params"];
                let token = params["token"].to_string();
                match params["value"]["kind"].as_str() {
                    Some("begin") => {
                        let title = params["value"]["title"].as_str().unwrap_or("working").to_string();
                        progress.lock().unwrap().insert(token, title);
                    }
                    Some("end") => {
                        progress.lock().unwrap().remove(&token);
                    }
                    _ => {}
                }
            }
            // kotlin-lsp tells how its import of the Gradle or Maven build went.
            (None, Some(method)) if method == "intellij/importLog" => {
                let params = &message["params"];
                let mut import = import.lock().unwrap();
                if params["started"] == true {
                    import.log.clear();
                }
                let text = params["message"].as_str().unwrap_or_default();
                if params["type"].as_i64().is_some_and(|t| t <= 2) && !text.starts_with('\t') && import.log.len() < 200 {
                    import.log.push(text.trim_end().to_string());
                }
            }
            (None, Some(method)) if method == "intellij/workspaceImportState" => {
                let params = &message["params"];
                if params["phase"] != "FINISHED" {
                    continue;
                }
                let failed = params["folders"].as_array().and_then(|f| f.iter().find(|f| f["status"] == "FAILED"));
                let mut import = import.lock().unwrap();
                import.failed = failed.map(|folder| {
                    let what = folder["message"].as_str().unwrap_or("import failed");
                    match build_failure(&import.log) {
                        Some(reason) => format!("{what}: {reason}"),
                        None => what.to_string(),
                    }
                });
            }
            _ => {}
        }
    }
}

/// What Gradle says went wrong, from its output: the lines between
/// "* What went wrong:" and the next section.
fn build_failure(log: &[String]) -> Option<String> {
    let start = log.iter().position(|l| l.starts_with("* What went wrong"))? + 1;
    let reason: Vec<&str> = log[start..]
        .iter()
        .take_while(|l| !l.starts_with("* "))
        .map(|l| l.trim().trim_start_matches("> "))
        .filter(|l| !l.is_empty())
        .collect();
    (!reason.is_empty()).then(|| reason.join(" "))
}

/// Variables a server needs that the user's environment does not give.
/// Android's Gradle plugin fails to configure a project without the SDK,
/// which Android Studio names in an untracked local.properties: a new
/// worktree has none.
fn server_env(program: &str) -> Vec<(&'static str, PathBuf)> {
    if program != "kotlin-lsp" || ["ANDROID_HOME", "ANDROID_SDK_ROOT"].iter().any(|v| std::env::var_os(v).is_some()) {
        return vec![];
    }
    let sdk = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Android/sdk"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Android").join("Sdk"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Android/Sdk"))
    };
    sdk.filter(|dir| dir.is_dir()).map(|dir| ("ANDROID_HOME", dir)).into_iter().collect()
}

#[cfg(test)]
mod import_tests {
    #[test]
    fn gradle_failure_reason() {
        let log: Vec<String> = [
            "FAILURE: Build failed with an exception.",
            "",
            "* What went wrong:",
            "A problem occurred configuring project ':app'.",
            "> SDK location not found.",
            "",
            "* Try:",
            "> Run with --info",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(super::build_failure(&log).as_deref(), Some("A problem occurred configuring project ':app'. SDK location not found."));
        assert_eq!(super::build_failure(&log[..2]), None);
    }
}

#[cfg(test)]
mod root_tests {
    use super::project_root;
    use std::path::Path;

    #[test]
    fn dependencies_share_the_project_server() {
        let base = std::env::temp_dir().join(format!("ideae-root-{}", std::process::id()));
        let vendored = base.join("vendor/dep/src");
        std::fs::create_dir_all(&vendored).unwrap();
        std::fs::write(base.join("Cargo.toml"), "").unwrap();
        std::fs::write(base.join("vendor/dep/Cargo.toml"), "").unwrap();
        // Two web apps side by side, no package.json above them.
        for app in ["apps/a", "apps/b"] {
            std::fs::create_dir_all(base.join(app).join("node_modules/lib")).unwrap();
            std::fs::write(base.join(app).join("package.json"), "").unwrap();
            std::fs::write(base.join(app).join("node_modules/lib/package.json"), "").unwrap();
        }
        let file = vendored.join("lib.rs");
        assert_eq!(project_root("rust-analyzer", &file, &base), base);
        let ts = "typescript-language-server";
        assert_eq!(project_root(ts, &base.join("apps/a/node_modules/lib/x.d.ts"), &base), base.join("apps/a"));
        assert_eq!(project_root(ts, &base.join("apps/b/src.ts"), &base), base.join("apps/b"));
        assert_eq!(project_root("rust-analyzer", &base.join("_"), Path::new(&base)), base);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parameter_being_written_is_found_in_the_signature() {
        let help = |label: &str, parameters: Vec<lsp_types::ParameterLabel>, active: u32| lsp_types::SignatureHelp {
            signatures: vec![lsp_types::SignatureInformation {
                label: label.into(),
                documentation: None,
                parameters: Some(parameters.into_iter().map(|label| lsp_types::ParameterInformation { label, documentation: None }).collect()),
                active_parameter: None,
            }],
            active_signature: Some(0),
            active_parameter: Some(active),
        };
        let offsets = |start, end| lsp_types::ParameterLabel::LabelOffsets([start, end]);
        let (label, range) = signature_shown(&help("fn put(key: &str, value: u32)", vec![offsets(7, 16), offsets(18, 28)], 1)).unwrap();
        assert_eq!(&label[range.unwrap()], "value: u32");
        // Offsets are in UTF-16 units: `é` is one, two bytes.
        let (label, range) = signature_shown(&help("fn é(a: u8, b: u8)", vec![offsets(5, 10), offsets(12, 17)], 1)).unwrap();
        assert_eq!(&label[range.unwrap()], "b: u8");
        let named = lsp_types::ParameterLabel::Simple("b int".into());
        let (label, range) = signature_shown(&help("func f(a int, b int)", vec![lsp_types::ParameterLabel::Simple("a int".into()), named], 1)).unwrap();
        assert_eq!(&label[range.unwrap()], "b int");
        assert_eq!(signature_shown(&help("fn none()", vec![], 0)).unwrap().1, None);
    }

    #[test]
    fn edits_apply_from_the_end() {
        let at = |line: u32, from: u32, to: u32, new: &str| lsp_types::TextEdit {
            range: lsp_types::Range::new(lsp_types::Position::new(line, from), lsp_types::Position::new(line, to)),
            new_text: new.to_string(),
        };
        let text = "let aé = foo(aé);\nprint(aé)\n";
        let renamed = apply_edits(text, &[at(0, 4, 6, "total"), at(0, 13, 15, "total"), at(1, 6, 8, "total")]);
        assert_eq!(renamed, "let total = foo(total);\nprint(total)\n", "columns are characters, and every range is of the text as it was");
        assert_eq!(apply_edits("a\n", &[at(5, 0, 0, "end")]), "a\nend", "past the end is the end");
        // One reaching into another, and one inside a letter of two bytes: left out, the rest made.
        assert_eq!(apply_edits("abcdef\n", &[at(0, 2, 5, "X"), at(0, 0, 3, "Y"), at(0, 5, 6, "Z")]), "abXZ\n");
        assert_eq!(apply_edits("\u{e9}a\n", &[at(0, 1, 2, "b")]), "\u{e9}b\n");
    }

    #[test]
    fn uris_round_trip() {
        let path = if cfg!(windows) { PathBuf::from(r"C:\Users\A B\src\main.rs") } else { PathBuf::from("/home/a b/src/main.rs") };
        let uri = path_to_uri(&path);
        assert!(!uri.contains(' ') && !uri.contains('\\'), "{uri}");
        assert_eq!(uri_to_path(&uri), Some(path));
        if cfg!(windows) {
            assert_eq!(uri, "file:///C:/Users/A%20B/src/main.rs");
            assert_eq!(uri_to_path("file:///c%3A/Users/x.rs"), Some(PathBuf::from(r"C:\Users\x.rs")));
        }
    }

    /// Needs gopls: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn gopls_definition_across_packages() {
        let root = std::env::temp_dir().join(format!("ideae-gopls-{}", std::process::id()));
        std::fs::create_dir_all(root.join("greet")).unwrap();
        std::fs::write(root.join("go.mod"), "module example.com/m\n\ngo 1.21\n").unwrap();
        std::fs::write(root.join("greet/greet.go"), "package greet\n\n// Hello says hi.\nfunc Hello() string { return \"hi\" }\n").unwrap();
        let file = root.join("main.go");
        let text = "package main\n\nimport \"example.com/m/greet\"\n\nfunc main() { _ = greet.Hello() }\n";
        std::fs::write(&file, text).unwrap();
        let (line, row) = text.lines().enumerate().find(|(_, l)| l.contains("greet.Hello(")).unwrap();
        let col = row.find("Hello").unwrap() as u32;

        let client = LspClient::spawn("gopls", Path::new("gopls"), &[], &root, Value::Null).unwrap();
        client.did_open(&file, "go", text).unwrap();
        let links = client
            .definition(&path_to_uri(&file), lsp_types::Position::new(line as u32, col + 2))
            .unwrap();
        let target = uri_to_path(links[0].target_uri.as_str()).unwrap();
        assert_eq!(target, root.join("greet/greet.go"));
        assert_eq!(links[0].target_selection_range.start.line, 3);

        let hover = client.hover(&path_to_uri(&file), lsp_types::Position::new(line as u32, col + 2)).unwrap();
        assert!(format!("{hover:?}").contains("Hello"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Needs rust-analyzer: `cargo test -- --ignored`. The root comes from
    /// project_root, as in the app: the directory above is no cargo project.
    /// What the editor asks of a server beyond navigation, on a crate made
    /// for it: what is wrong, the fix offered, completion, rename, format.
    #[test]
    #[ignore]
    fn rust_analyzer_completes_fixes_renames_and_formats() {
        let root = std::env::temp_dir().join(format!("ideae-ra-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        let file = root.join("src/main.rs");
        let text = "fn greeting() -> String {\n    String::new()\n}\n\nfn main() {\n    let   words = greeting();\n    let n: u32 = words;\n    println!(\"{}\", n);\n}\n";
        std::fs::write(&file, text).unwrap();
        let client = LspClient::spawn("rust-analyzer", Path::new("rust-analyzer"), &[], &root, Value::Null).unwrap();
        client.did_open(&file, "rust", text).unwrap();
        let uri = path_to_uri(&file);
        let wait = |what: &str, done: &mut dyn FnMut() -> bool| {
            let started = std::time::Instant::now();
            while !done() {
                assert!(started.elapsed() < Duration::from_secs(240), "{what} never came");
                thread::sleep(Duration::from_secs(1));
            }
        };

        let mut wrong = Vec::new();
        wait("the type error", &mut || {
            wrong = client.problems(&uri).1;
            wrong.iter().any(|d| d.range.start.line == 6)
        });

        let mut items = Vec::new();
        wait("completion", &mut || {
            let typed = text.replace("words = greeting()", "words = gre()");
            client.did_change(&file, 0, &typed).unwrap();
            items = match client.completion(&uri, lsp_types::Position::new(5, 21), None) {
                Ok(lsp_types::CompletionResponse::Array(items)) => items,
                Ok(lsp_types::CompletionResponse::List(list)) => list.items,
                Err(_) => Vec::new(),
            };
            items.iter().any(|item| item.label.starts_with("greeting"))
        });
        client.did_change(&file, 0, text).unwrap();

        let mut renamed = None;
        wait("rename", &mut || {
            renamed = client.rename(&uri, lsp_types::Position::new(0, 4), "salute").ok().flatten();
            renamed.is_some()
        });
        let edits = edits_by_file(&renamed.unwrap());
        let (path, edits) = &edits[0];
        assert_eq!(path.canonicalize().unwrap(), file.canonicalize().unwrap());
        let after = apply_edits(text, edits);
        assert!(after.contains("fn salute()") && after.contains("words = salute()"), "{after}");

        let formatted = apply_edits(text, &client.formatting(&uri, 4, true).unwrap());
        assert!(formatted.contains("    let words = greeting();"), "{formatted}");

        let line = lsp_types::Range::new(lsp_types::Position::new(5, 10), lsp_types::Position::new(5, 10));
        let actions = client.code_actions(&uri, line).unwrap();
        assert!(!actions.is_empty(), "something is offered on a `let`");
        let action = match actions[0].edit {
            Some(_) => actions[0].clone(),
            None => client.resolve_code_action(&actions[0]).unwrap(),
        };
        assert!(action.edit.is_some(), "{action:?}");

        let found = client.workspace_symbols("greet").unwrap();
        assert!(found.iter().any(|s| s.name == "greeting" && s.kind == 12 && s.line == 0), "{found:?}");
        let typed = text.replace("greeting();", "greeting(");
        client.did_change(&file, 0, &typed).unwrap();
        let help = client.signature_help(&uri, lsp_types::Position::new(5, 31)).unwrap().expect("inside a call");
        assert!(help.signatures[0].label.contains("fn greeting() -> String"), "{help:?}");
        client.did_change(&file, 0, text).unwrap();
        let legend = client.semantic_legend();
        assert!(legend.iter().any(|name| name == "function"), "{legend:?}");
        let all = lsp_types::Range::new(lsp_types::Position::new(0, 0), lsp_types::Position::new(9, 0));
        let tokens = client.semantic_tokens(&uri, all).unwrap();
        // `fn greeting`: the first token named is the function, at 0:3.
        let first = tokens.data.iter().find(|t| legend[t.token_type as usize] == "function").unwrap();
        assert_eq!((first.delta_start, first.length), (3, 8));
        // `let   words`: its type, after the name.
        let hints = client.inlay_hints(&uri, all).unwrap();
        let words = hints.iter().find(|h| h.position == lsp_types::Position::new(5, 15)).unwrap_or_else(|| panic!("no hint after `words`: {hints:?}"));
        let label = match &words.label {
            lsp_types::InlayHintLabel::String(label) => label.clone(),
            lsp_types::InlayHintLabel::LabelParts(parts) => parts.iter().map(|p| p.value.as_str()).collect(),
        };
        assert!(label.contains("String"), "{hints:?}");
        assert_eq!(words.kind, Some(lsp_types::InlayHintKind::TYPE));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore]
    fn rust_analyzer_local_definition() {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let file = crate_dir.join("src/git.rs");
        let root = project_root("rust-analyzer", &file, crate_dir.parent().unwrap());
        assert_eq!(root, crate_dir);

        let text = std::fs::read_to_string(&file).unwrap();
        let (use_line, row) = text.lines().enumerate().find(|(_, l)| l.contains("} else if line.starts_with")).unwrap();
        let col = row.find("line.").unwrap() as u32 + 1;
        let (decl_line, _) = text.lines().enumerate().find(|(_, l)| l.trim_start().starts_with("for line in patch.lines()")).unwrap();

        let client = LspClient::spawn("rust-analyzer", Path::new("rust-analyzer"), &[], &root, Value::Null).unwrap();
        client.did_open(&file, "rust", &text).unwrap();
        let uri = path_to_uri(&file);
        let started = std::time::Instant::now();
        let links = loop {
            let links = client.definition(&uri, lsp_types::Position::new(use_line as u32, col)).unwrap_or_default();
            if !links.is_empty() || started.elapsed() > Duration::from_secs(180) {
                break links;
            }
            thread::sleep(Duration::from_secs(2));
        };
        eprintln!("answered after {:?}", started.elapsed());
        assert!(!links.is_empty(), "no definition");
        assert_eq!(uri_to_path(links[0].target_uri.as_str()).unwrap(), file);
        assert_eq!(links[0].target_selection_range.start.line as usize, decl_line);
    }
}

#[cfg(all(test, unix))]
mod write_tests {
    use super::*;

    /// A server is ended with what it started.
    #[test]
    fn what_a_server_started_ends_with_it() {
        let dir = std::env::temp_dir().join(format!("ideae-lsp-group-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (script, pid_file) = (dir.join("server.sh"), dir.join("child.pid"));
        std::fs::write(&script, format!("sleep 300 &\necho $! > '{}'\ncat >/dev/null\n", pid_file.display())).unwrap();
        // Read by sh, not run itself: a file just written, run while another
        // test's fork still holds it open, fails with "Text file busy".
        let script_arg = script.to_string_lossy().into_owned();
        let client = LspClient::spawn("test-server", std::path::Path::new("/bin/sh"), &[script_arg.as_str()], &dir, Value::Null).unwrap();
        let started = std::time::Instant::now();
        let child = loop {
            match std::fs::read_to_string(&pid_file).ok().and_then(|pid| pid.trim().parse::<u32>().ok()) {
                Some(pid) => break pid,
                None if started.elapsed() > Duration::from_secs(10) => panic!("the server did not start"),
                None => thread::sleep(Duration::from_millis(20)),
            }
        };
        assert!(crate::procs::alive(child));
        client.shut_down();
        let ended = std::time::Instant::now();
        while crate::procs::alive(child) && ended.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!crate::procs::alive(child), "what the server started is left running");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the server is sent, read back from a `cat` in its place.
    #[test]
    fn the_server_is_written_to_in_order_and_only_the_newest_text() {
        let mut child = std::process::Command::new("cat").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let (out, outbox) = mpsc::channel();
        let named = |name: &str| json!({ "method": name });
        let change = |text: &str| Out::Change { uri: "file:///a".into(), message: json!({ "method": "change", "text": text }) };
        // All of it asked before a byte is written, the handshake in the middle.
        for message in [
            Out::Later(named("open")),
            change("one"),
            Out::Now(named("initialize")),
            change("two"),
            Out::Change { uri: "file:///b".into(), message: named("other") },
            Out::Ready,
            Out::Later(named("hover")),
            change("three"),
        ] {
            out.send(message).unwrap();
        }
        drop(out);
        write_loop(child.stdin.take().unwrap(), outbox);
        let mut written = String::new();
        child.stdout.take().unwrap().read_to_string(&mut written).unwrap();
        let _ = child.wait();
        let sent: Vec<String> = written
            .split("Content-Length: ")
            .filter_map(|part| part.split_once("\r\n\r\n"))
            .map(|(_, body)| {
                let message: Value = serde_json::from_str(body).unwrap();
                message["text"].as_str().or(message["method"].as_str()).unwrap().to_string()
            })
            .collect();
        // The handshake at once; then what waited, in the order asked. Of a
        // file's texts with nothing asked between them the newest; what is
        // asked comes after the text it was asked of.
        assert_eq!(sent, ["initialize", "open", "two", "other", "hover", "three"]);
    }
}

#[cfg(test)]
mod language_tests {
    use super::language_for;
    use std::path::Path;

    #[test]
    fn a_template_is_read_as_what_it_stands_for() {
        let of = |name: &str| language_for(Path::new(name));
        assert_eq!(of("phpunit.xml.dist"), "xml");
        assert_eq!(of("behat.yml.dist"), "yaml");
        assert_eq!(of("phpstan.neon.dist"), "yaml");
        assert_eq!(of("config.json.example"), "json");
        assert_eq!(of(".env.example"), "properties");
        assert_eq!(of("composer.lock"), "json");
        assert_eq!(of("notes.dist"), "text");
        assert_eq!(of("App.csproj"), "xml");
        assert_eq!(of("Gemfile"), "ruby");
        assert_eq!(of(".zshrc"), "bash");
        assert_eq!(of("main.tf"), "hcl");
        assert_eq!(super::extension_of(Path::new("/a/phpunit.xml.dist")), "xml");
        assert_eq!(super::extension_of(Path::new("/a/Makefile")), "");
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    /// kotlin-lsp shows a kind of hint only when it is named as on.
    #[test]
    fn kotlin_lsp_is_told_which_hints_to_show() {
        let kotlin = LspClient::configuration("jetbrains.kotlin");
        assert_eq!(kotlin["hints"]["type.variable"], Value::Bool(true));
        assert_eq!(kotlin["hints"]["parameters"], Value::Bool(true));
        assert_eq!(LspClient::configuration("intelephense"), Value::Null);
    }
}
