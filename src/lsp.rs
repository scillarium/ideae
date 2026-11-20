//! Minimal stdio LSP client: hover, go to definition, implementations and
//! references. Calls block, so run them on the background executor.

use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
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
/// While a command of ours runs: the edits and the place the server asked
/// the editor to apply and show (`workspace/applyEdit`, `window/showDocument`).
type Commanded = Arc<Mutex<Option<Vec<(String, Value)>>>>;
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
    commanded: Commanded,
    /// One command at a time: what the server asks while it runs is its.
    command: Mutex<()>,
    /// How it serves several tasks' worktrees, when it does (`lsp_share`).
    sharing: Option<crate::lsp_share::Share>,
    /// The folders it serves, one per task that holds it.
    folders: Mutex<Vec<PathBuf>>,
    /// Folders added whose indexing has not ended: the work the server
    /// does now is theirs, not the others'.
    warming: Warming,
    /// Folders let go of: rust-analyzer keeps what it read of each.
    removed: AtomicUsize,
    /// Files opened for an agent's question alone (uri): closed after it,
    /// unless the editor opened them meanwhile.
    borrowed: Mutex<HashSet<String>>,
    /// Its output closed: the server has ended.
    gone: Arc<AtomicBool>,
}

/// Folders whose reading has not ended, and since when.
type Warming = Arc<Mutex<HashMap<PathBuf, std::time::Instant>>>;

/// Power saving: no language servers run, none start.
pub static POWER_SAVING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a command asked of the editor: edits, then a place to show.
#[derive(Debug, Default)]
pub struct CommandDone {
    pub edits: Vec<lsp_types::WorkspaceEdit>,
    pub show: Option<(String, lsp_types::Range)>,
}

/// The server for a file: its language id, its name, the executable to run
/// (a name on PATH or a path ideae installed it at) and its arguments.
pub struct Server {
    pub language_id: &'static str,
    pub program: &'static str,
    pub command: PathBuf,
    pub args: &'static [&'static str],
}

/// A language server ideae can run: the program, its arguments, the file
/// extensions it serves with their LSP language ids, files it serves by
/// name, and what marks a project of its own. The one list: Settings shows
/// what is here.
pub struct ServerDef {
    pub program: &'static str,
    pub args: &'static [&'static str],
    pub languages: &'static [(&'static str, &'static str)],
    /// Files it serves by their lowercase name, before any extension, with
    /// their language ids; a `*` stands for any part of the name.
    pub names: &'static [(&'static str, &'static str)],
    /// Files in a project's top directory: see `project_root`.
    pub markers: &'static [&'static str],
}

const fn server(
    program: &'static str,
    args: &'static [&'static str],
    languages: &'static [(&'static str, &'static str)],
    markers: &'static [&'static str],
) -> ServerDef {
    ServerDef { program, args, languages, names: &[], markers }
}

pub const SERVERS: &[ServerDef] = &[
    server("gopls", &[], &[("go", "go")], &["go.work", "go.mod"]),
    server("rust-analyzer", &[], &[("rs", "rust")], &["Cargo.toml"]),
    server(
        "typescript-language-server",
        &["--stdio"],
        &[
            ("ts", "typescript"),
            ("tsx", "typescriptreact"),
            ("mts", "typescript"),
            ("cts", "typescript"),
            ("js", "javascript"),
            ("jsx", "javascriptreact"),
            ("mjs", "javascript"),
            ("cjs", "javascript"),
        ],
        &["tsconfig.json", "jsconfig.json", "package.json"],
    ),
    server("intelephense", &["--stdio"], &[("php", "php")], &["composer.json"]),
    server("phpactor", &["language-server"], &[("php", "php")], &["composer.json"]),
    server("kotlin-lsp", &["--stdio"], &[("kt", "kotlin"), ("kts", "kotlin")], &["settings.gradle.kts", "settings.gradle"]),
    server(
        "basedpyright-langserver",
        &["--stdio"],
        &[("py", "python"), ("pyi", "python")],
        &["pyproject.toml", "pyrightconfig.json", "setup.py", "setup.cfg"],
    ),
    server(
        "clangd",
        &[],
        &[
            ("c", "c"),
            ("h", "c"),
            ("cpp", "cpp"),
            ("cc", "cpp"),
            ("cxx", "cpp"),
            ("c++", "cpp"),
            ("hpp", "cpp"),
            ("hxx", "cpp"),
            ("hh", "cpp"),
            ("h++", "cpp"),
            ("ipp", "cpp"),
            ("tpp", "cpp"),
            ("inl", "cpp"),
            ("cu", "cuda-cpp"),
            ("cuh", "cuda-cpp"),
            ("mm", "objective-cpp"),
        ],
        &[".clangd", "compile_commands.json", "compile_flags.txt", "CMakeLists.txt", "meson.build"],
    ),
    server("jdtls", &[], &[("java", "java")], &["pom.xml", "settings.gradle.kts", "settings.gradle", "build.gradle.kts", "build.gradle"]),
    server("csharp-ls", &[], &[("cs", "csharp"), ("csx", "csharp")], &[]),
    ServerDef {
        names: &[
            ("gemfile", "ruby"),
            ("rakefile", "ruby"),
            ("podfile", "ruby"),
            ("vagrantfile", "ruby"),
            ("fastfile", "ruby"),
            ("brewfile", "ruby"),
            ("guardfile", "ruby"),
        ],
        ..server("ruby-lsp", &[], &[("rb", "ruby"), ("rake", "ruby"), ("gemspec", "ruby"), ("ru", "ruby"), ("podspec", "ruby")], &["Gemfile"])
    },
    server("lua-language-server", &[], &[("lua", "lua")], &[".luarc.json", ".luarc.jsonc"]),
    ServerDef {
        names: &[
            (".bashrc", "shellscript"),
            (".bash_profile", "shellscript"),
            (".bash_aliases", "shellscript"),
            (".bash_logout", "shellscript"),
            (".profile", "shellscript"),
            (".envrc", "shellscript"),
            ("pkgbuild", "shellscript"),
            ("apkbuild", "shellscript"),
        ],
        ..server("bash-language-server", &["start"], &[("sh", "shellscript"), ("bash", "shellscript"), ("bats", "shellscript")], &[])
    },
    server("zls", &[], &[("zig", "zig"), ("zon", "zig")], &["build.zig"]),
    server("sourcekit-lsp", &[], &[("swift", "swift")], &["Package.swift"]),
    server("dart", &["language-server", "--protocol=lsp"], &[("dart", "dart")], &["pubspec.yaml"]),
    server("elixir-ls", &[], &[("ex", "elixir"), ("exs", "elixir")], &["mix.exs"]),
    server("metals", &[], &[("scala", "scala"), ("sc", "scala"), ("sbt", "scala")], &["build.sbt", "build.sc", "build.mill"]),
    // Files that are JSON with comments, which plain JSON marks as errors.
    ServerDef {
        names: &[
            ("tsconfig.json", "jsonc"),
            ("tsconfig.*.json", "jsonc"),
            ("jsconfig.json", "jsonc"),
            ("jsconfig.*.json", "jsonc"),
            (".eslintrc.json", "jsonc"),
            ("devcontainer.json", "jsonc"),
            (".devcontainer.json", "jsonc"),
            ("settings.json", "jsonc"),
            ("launch.json", "jsonc"),
            ("tasks.json", "jsonc"),
            ("extensions.json", "jsonc"),
        ],
        ..server("vscode-json-language-server", &["--stdio"], &[("json", "json"), ("jsonc", "jsonc")], &[])
    },
    server("vscode-css-language-server", &["--stdio"], &[("css", "css"), ("scss", "scss"), ("less", "less")], &[]),
    server("vscode-html-language-server", &["--stdio"], &[("html", "html"), ("htm", "html")], &[]),
    server("yaml-language-server", &["--stdio"], &[("yaml", "yaml"), ("yml", "yaml")], &[]),
    server("taplo", &["lsp", "stdio"], &[("toml", "toml")], &[]),
    server("marksman", &["server"], &[("md", "markdown"), ("markdown", "markdown")], &[".marksman.toml"]),
    ServerDef {
        names: &[("dockerfile", "dockerfile"), ("dockerfile.*", "dockerfile"), ("containerfile", "dockerfile"), ("containerfile.*", "dockerfile")],
        ..server("docker-langserver", &["--stdio"], &[("dockerfile", "dockerfile"), ("containerfile", "dockerfile")], &[])
    },
    server("terraform-ls", &["serve"], &[("tf", "terraform"), ("tfvars", "terraform-vars")], &[]),
];

/// Languages with more than one server, one of which runs: the key the
/// choice is kept by, the language's name, and the servers, the default
/// first.
pub const ALTERNATIVES: &[(&str, &str, &[&str])] = &[("php", "PHP", &["intelephense", "phpactor"])];

/// The sealed store's name for intelephense's licence key.
pub const INTELEPHENSE_LICENCE: &str = "lsp:intelephense/licence";

/// The server chosen for a language of [`ALTERNATIVES`].
pub fn chosen(key: &str, settings: &crate::state::Settings) -> &'static str {
    let Some((_, _, programs)) = ALTERNATIVES.iter().find(|(k, _, _)| *k == key) else { return "" };
    let choice = settings.lsp_choice.get(key);
    programs.iter().copied().find(|p| choice.is_some_and(|c| c == p)).unwrap_or(programs[0])
}

/// intelephense without a licence, which leaves out rename, go to
/// implementation, inlay hints and code actions. Waits for initialization.
pub fn without_licence(program: &str, client: &LspClient) -> bool {
    program == "intelephense" && !client.supports("renameProvider")
}

/// Another server than the one chosen for its language.
fn passed_over(program: &str) -> bool {
    ALTERNATIVES.iter().find(|(_, _, programs)| programs.contains(&program)).is_some_and(|(key, _, _)| {
        crate::state::with_settings(|s| chosen(key, s)) != program
    })
}

impl ServerDef {
    /// ".go", ".ts .tsx .js .jsx", ".sh .bash .bashrc": what it is for, in
    /// Settings; names that have an extension of its own go without saying.
    pub fn extensions(&self) -> String {
        let extensions = self.languages.iter().map(|(ext, _)| format!(".{ext}"));
        let names = self
            .names
            .iter()
            .filter(|(name, _)| !self.languages.iter().any(|(ext, _)| name.ends_with(&format!(".{ext}"))))
            .map(|(name, _)| name.to_string());
        extensions.chain(names).collect::<Vec<_>>().join(" ")
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
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_lowercase();
    let name = template_of(&name);
    let named = |pattern: &str| match pattern.split_once('*') {
        Some((start, end)) => name.len() >= start.len() + end.len() && name.starts_with(start) && name.ends_with(end),
        None => name == pattern,
    };
    let servers = || SERVERS.iter().filter(|def| !passed_over(def.program));
    let by_name = servers().find_map(|def| {
        def.names.iter().find(|(pattern, _)| named(pattern)).map(|(_, language)| (*language, def.program, def.args))
    });
    if by_name.is_some() {
        return by_name;
    }
    let ext = extension_of(path);
    servers().find_map(|def| {
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
    let markers = SERVERS.iter().find(|def| def.program == program).map_or(&[][..], |def| def.markers);
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
        // `get`: the end of a name not in ASCII may not be a whole character.
        if name.len() > suffix.len() && name.get(name.len() - suffix.len()..).is_some_and(|end| end.eq_ignore_ascii_case(suffix)) {
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
        Self::spawn_in(program, executable, args, root, root, None, init_options)
    }

    /// The same, run in `cwd`: for a server several worktrees share, the
    /// main checkout, which outlives each of them.
    pub fn spawn_in(
        program: &str,
        executable: &Path,
        args: &[&str],
        root: &Path,
        cwd: &Path,
        sharing: Option<crate::lsp_share::Share>,
        init_options: Value,
    ) -> Result<Arc<Self>> {
        let mut command = crate::procs::command(executable);
        command.args(args).args(project_args(program, root)).envs(server_env(program)).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
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
        let reader_settings = settings(program);
        let commanded: Commanded = Arc::default();
        let reader_commanded = commanded.clone();
        let warming: Warming = Arc::new(Mutex::new(HashMap::from([(root.to_path_buf(), std::time::Instant::now())])));
        let reader_warming = warming.clone();
        let gone = Arc::new(AtomicBool::new(false));
        let reader_gone = gone.clone();
        thread::Builder::new()
            .name(format!("lsp-read-{program}"))
            .spawn(move || {
                read_loop(stdout, reader_out, reader_pending, reader_progress, reader_import, reader_problems, reader_opened, reader_commanded, reader_settings, reader_warming);
                reader_gone.store(true, Ordering::Relaxed);
            })?;

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
            commanded,
            command: Mutex::new(()),
            sharing,
            folders: Mutex::new(vec![root.to_path_buf()]),
            warming,
            removed: AtomicUsize::new(0),
            borrowed: Mutex::new(HashSet::new()),
            gone,
        });

        let init = client.clone();
        let told = settings(program);
        let root = root.to_path_buf();
        thread::Builder::new()
            .name(format!("lsp-init-{program}"))
            .spawn(move || {
                let result = init.initialize(&root, init_options, &told).map_err(|e| e.to_string());
                if result.is_ok() {
                    let _ = init.out.send(Out::Ready);
                }
                let (lock, cvar) = &init.ready;
                *lock.lock().unwrap() = Some(result);
                cvar.notify_all();
            })?;
        Ok(client)
    }

    fn initialize(&self, root: &Path, init_options: Value, settings: &Value) -> Result<()> {
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
                        "completion": {
                            "completionItem": {
                                "snippetSupport": true,
                                "labelDetailsSupport": true,
                                "documentationFormat": ["markdown", "plaintext"],
                                "resolveSupport": { "properties": ["additionalTextEdits", "command", "detail", "documentation"] },
                            },
                        },
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
                        "applyEdit": true,
                        "symbol": {},
                    },
                    "window": { "workDoneProgress": true, "showDocument": { "support": true } },
                },
            }),
        )?;
        *self.capabilities.lock().unwrap() = answer["capabilities"].clone();
        self.send(Out::Now(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} })))?;
        // For the servers that wait to be told rather than ask.
        if settings.is_null() {
            return Ok(());
        }
        self.send(Out::Now(json!({ "jsonrpc": "2.0", "method": "workspace/didChangeConfiguration", "params": { "settings": settings } })))
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

    /// The same, as a file of one of its folders sees it: while a folder
    /// just added is read, that is the work, and the others' files are not
    /// told of it.
    pub fn busy_for(&self, path: &Path) -> Option<String> {
        let work = self.busy()?;
        let warming = self.warming.lock().unwrap();
        let mine = self.folder_of(path).is_some_and(|folder| warming.contains_key(&folder));
        (warming.is_empty() || mine).then_some(work)
    }

    /// Whether what it says of `path` may be short of the truth: it is still
    /// reading the folder (indexing), or was given it a moment ago and has
    /// not said it began.
    pub fn reading(&self, path: &Path) -> bool {
        if self.busy_for(path).is_some() {
            return true;
        }
        let since = self.folder_of(path).and_then(|folder| self.warming.lock().unwrap().get(&folder).copied());
        since.is_some_and(|at| at.elapsed() < Duration::from_secs(15))
    }

    /// The server has ended, crashed or killed.
    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::Relaxed)
    }

    pub fn sharing(&self) -> Option<crate::lsp_share::Share> {
        self.sharing
    }

    /// The folders it serves now.
    #[cfg(test)]
    pub fn folders(&self) -> Vec<PathBuf> {
        self.folders.lock().unwrap().clone()
    }

    /// How many folders it has let go of since it started.
    pub fn removed(&self) -> usize {
        self.removed.load(Ordering::Relaxed)
    }

    /// The folder of `path` among those it serves, the deepest.
    pub fn folder_of(&self, path: &Path) -> Option<PathBuf> {
        self.folders.lock().unwrap().iter().filter(|f| path.starts_with(f)).max_by_key(|f| f.as_os_str().len()).cloned()
    }

    /// A place found for a file of `asking` that is in another task's
    /// worktree: a server several share knows them all.
    pub fn foreign(&self, asking: &Path, found: &Path) -> bool {
        match (self.folder_of(asking), self.folder_of(found)) {
            (Some(mine), Some(theirs)) => mine != theirs,
            _ => false,
        }
    }

    /// The same for a task by its worktree: found in a folder outside it.
    pub fn foreign_to_root(&self, root: &Path, found: &Path) -> bool {
        self.folder_of(found).is_some_and(|folder| !folder.starts_with(root))
    }

    fn foreign_uri(&self, asking: &str, found: &str) -> bool {
        self.sharing.is_some()
            && match (uri_to_path(asking), uri_to_path(found)) {
                (Some(asking), Some(found)) => self.foreign(&asking, &found),
                _ => false,
            }
    }

    /// Serves one more folder; servers that read their folders are told of it.
    pub fn add_folder(&self, folder: &Path) {
        let mut folders = self.folders.lock().unwrap();
        if folders.iter().any(|f| f == folder) {
            return;
        }
        folders.push(folder.to_path_buf());
        drop(folders);
        self.warming.lock().unwrap().insert(folder.to_path_buf(), std::time::Instant::now());
        self.tell_folders(folder, true);
    }

    /// Lets go of a folder; whether none is left.
    pub fn remove_folder(&self, folder: &Path) -> bool {
        let mut folders = self.folders.lock().unwrap();
        let before = folders.len();
        folders.retain(|f| f != folder);
        let (went, left) = (folders.len() < before, folders.len());
        drop(folders);
        self.warming.lock().unwrap().remove(folder);
        if went && left > 0 {
            self.removed.fetch_add(1, Ordering::Relaxed);
            // Its files close with it: when the task comes back the editor
            // opens them again with their text then, not the one they had.
            let prefix = path_to_uri(folder) + "/";
            let theirs: Vec<String> = self.opened.lock().unwrap().iter().filter(|uri| uri.starts_with(&prefix)).cloned().collect();
            for uri in theirs {
                self.opened.lock().unwrap().remove(&uri);
                self.borrowed.lock().unwrap().remove(&uri);
                self.problems.lock().unwrap().1.remove(&uri);
                let _ = self.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri } }));
            }
            self.tell_folders(folder, false);
        }
        left == 0
    }

    fn tell_folders(&self, folder: &Path, added: bool) {
        if self.sharing != Some(crate::lsp_share::Share::Folders) {
            return;
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let one = json!([{ "uri": path_to_uri(folder), "name": name }]);
        let event = if added { json!({ "added": one, "removed": [] }) } else { json!({ "added": [], "removed": one }) };
        let _ = self.notify("workspace/didChangeWorkspaceFolders", json!({ "event": event }));
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
            // Opened for a question: the editor's now, with the editor's text.
            if self.borrowed.lock().unwrap().remove(&uri) {
                return self.did_change(path, 0, text);
            }
            return Ok(());
        }
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": language_id, "version": 1, "text": text } }),
        )
    }

    /// Whether the file is open in the server, its text the editor's.
    pub fn has_open(&self, path: &Path) -> bool {
        self.opened.lock().unwrap().contains(&path_to_uri(path))
    }

    /// Opens a file the editor has not, for a question; whether it did.
    pub fn borrow_file(&self, path: &Path, language_id: &str, text: &str) -> Result<bool> {
        if self.has_open(path) {
            return Ok(false);
        }
        self.did_open(path, language_id, text)?;
        self.borrowed.lock().unwrap().insert(path_to_uri(path));
        Ok(true)
    }

    /// Closes a file opened for a question, unless the editor took it.
    pub fn return_file(&self, path: &Path) {
        if self.borrowed.lock().unwrap().remove(&path_to_uri(path)) {
            let _ = self.did_close(path);
        }
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
        let links = match response {
            lsp_types::GotoDefinitionResponse::Scalar(l) => vec![to_link(l)],
            lsp_types::GotoDefinitionResponse::Array(ls) => ls.into_iter().map(to_link).collect(),
            lsp_types::GotoDefinitionResponse::Link(links) => links,
        };
        Ok(links.into_iter().filter(|l| !self.foreign_uri(uri, l.target_uri.as_str())).collect())
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
        let found = match response {
            lsp_types::GotoDefinitionResponse::Scalar(l) => vec![l],
            lsp_types::GotoDefinitionResponse::Array(ls) => ls,
            lsp_types::GotoDefinitionResponse::Link(links) => links
                .into_iter()
                .map(|l| lsp_types::Location { uri: l.target_uri, range: l.target_selection_range })
                .collect(),
        };
        Ok(found.into_iter().filter(|l| !self.foreign_uri(uri, l.uri.as_str())).collect())
    }

    /// textDocument/references, without the declaration itself.
    pub fn references(&self, uri: &str, pos: lsp_types::Position) -> Result<Vec<lsp_types::Location>> {
        self.wait_ready()?;
        let mut params = position_params(uri, pos);
        params["context"] = json!({ "includeDeclaration": false });
        let value = self.raw_request("textDocument/references", params)?;
        let found = serde_json::from_value::<Option<Vec<lsp_types::Location>>>(value)?.unwrap_or_default();
        Ok(found.into_iter().filter(|l| !self.foreign_uri(uri, l.uri.as_str())).collect())
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

    /// Whether an item may come with only part of itself, the rest asked for.
    pub fn resolves_completions(&self) -> bool {
        self.capabilities.lock().unwrap()["completionProvider"]["resolveProvider"] == true
    }

    /// completionItem/resolve: the item with what the server left out of the list.
    pub fn resolve_completion(&self, item: &lsp_types::CompletionItem) -> Result<lsp_types::CompletionItem> {
        let value = self.raw_request("completionItem/resolve", serde_json::to_value(item)?)?;
        Ok(serde_json::from_value(value)?)
    }

    /// workspace/executeCommand, with the edits and the place the server
    /// asked to apply and show while it ran.
    pub fn execute_command(&self, command: &lsp_types::Command) -> Result<CommandDone> {
        let _one = self.command.lock().unwrap_or_else(|e| e.into_inner());
        *self.commanded.lock().unwrap() = Some(Vec::new());
        let params = json!({ "command": command.command, "arguments": command.arguments.clone().unwrap_or_default() });
        let answer = self.raw_request("workspace/executeCommand", params);
        let asked = self.commanded.lock().unwrap().take().unwrap_or_default();
        answer?;
        let mut done = CommandDone::default();
        for (method, params) in asked {
            if method == "workspace/applyEdit" {
                done.edits.extend(serde_json::from_value::<lsp_types::WorkspaceEdit>(params["edit"].clone()).ok());
            } else if let (Some(uri), Ok(selection)) = (params["uri"].as_str(), serde_json::from_value::<lsp_types::Range>(params["selection"].clone())) {
                done.show = Some((uri.to_string(), selection));
            }
        }
        Ok(done)
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
        let mut edit = self.raw_request("textDocument/rename", params)?;
        // Another task's files are its own: a shared server would rename there too.
        if self.sharing.is_some() {
            if let Some(changes) = edit["changes"].as_object_mut() {
                changes.retain(|file, _| !self.foreign_uri(uri, file));
            }
            if let Some(changes) = edit["documentChanges"].as_array_mut() {
                changes.retain(|change| {
                    let file = change["textDocument"]["uri"].as_str().or(change["uri"].as_str()).or(change["oldUri"].as_str());
                    !file.is_some_and(|file| self.foreign_uri(uri, file))
                });
            }
        }
        Ok(serde_json::from_value(edit)?)
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
    let offset = offsets(text);
    let ranges = edits.iter().map(|e| (offset(&e.range.start), offset(&e.range.end), e.new_text.as_str())).collect();
    apply_ranges(text, ranges)
}

/// A position of `text` to its byte offset; past the end, the end.
fn offsets(text: &str) -> impl Fn(&lsp_types::Position) -> usize + '_ {
    let lines: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    move |pos: &lsp_types::Position| {
        let Some(&start) = lines.get(pos.line as usize) else { return text.len() };
        let line = &text[start..lines.get(pos.line as usize + 1).map_or(text.len(), |next| next - 1)];
        start + line.char_indices().nth(pos.character as usize).map_or(line.len(), |(i, _)| i)
    }
}

/// The byte offset of a place the server named in `text`.
pub fn offset_of(text: &str, pos: &lsp_types::Position) -> usize {
    offsets(text)(pos)
}

/// The edits that come with a completion (an import), placed in `before`,
/// made in `after`: `before` with `range` replaced by `inserted` bytes.
/// One that reaches into what the completion replaced is left out.
pub fn apply_edits_around(before: &str, range: std::ops::Range<usize>, inserted: usize, after: &str, edits: &[lsp_types::TextEdit]) -> String {
    let offset = offsets(before);
    let ranges = edits
        .iter()
        .filter_map(|e| {
            let (start, end) = (offset(&e.range.start), offset(&e.range.end));
            if end <= range.start {
                Some((start, end, e.new_text.as_str()))
            } else if start >= range.end {
                Some((start - range.end + range.start + inserted, end - range.end + range.start + inserted, e.new_text.as_str()))
            } else {
                None
            }
        })
        .collect();
    apply_ranges(after, ranges)
}

fn apply_ranges(text: &str, mut ranges: Vec<(usize, usize, &str)>) -> String {
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

/// A server's completions as the menu lists them: those the typed word
/// does not match left out (by `filterText`, else the label, its letters
/// in order), in the server's order (`sortText`, else the label), and an
/// item that is another's twin in all that is shown and done merged into
/// it. Twins in look that differ in what they add (an import) show it.
pub fn arrange_completions(items: Vec<lsp_types::CompletionItem>, typed: &str) -> Vec<lsp_types::CompletionItem> {
    let typed: Vec<char> = typed.chars().flat_map(char::to_lowercase).collect();
    let matches = |item: &lsp_types::CompletionItem| {
        let mut wanted = typed.iter().peekable();
        for c in item.filter_text.as_deref().unwrap_or(&item.label).chars().flat_map(char::to_lowercase) {
            if wanted.peek() == Some(&&c) {
                wanted.next();
            }
        }
        wanted.peek().is_none()
    };
    let mut items: Vec<lsp_types::CompletionItem> = items.into_iter().filter(matches).collect();
    items.sort_by(|a, b| a.sort_text.as_deref().unwrap_or(&a.label).cmp(b.sort_text.as_deref().unwrap_or(&b.label)));
    let new_text = |item: &lsp_types::CompletionItem| match &item.text_edit {
        Some(lsp_types::CompletionTextEdit::Edit(edit)) => Some(edit.new_text.clone()),
        Some(lsp_types::CompletionTextEdit::InsertAndReplace(edit)) => Some(edit.new_text.clone()),
        None => item.insert_text.clone(),
    };
    let look = |item: &lsp_types::CompletionItem| {
        serde_json::to_string(&(&item.label, item.kind, &item.detail, &item.label_details)).unwrap_or_default()
    };
    let mut seen: HashSet<String> = HashSet::new();
    items.retain(|item| {
        let does = serde_json::to_string(&(new_text(item), &item.additional_text_edits, item.command.as_ref().map(|c| &c.command))).unwrap_or_default();
        seen.insert(look(item) + &does)
    });
    let mut looks: HashMap<String, usize> = HashMap::new();
    for item in &items {
        *looks.entry(look(item)).or_default() += 1;
    }
    for item in &mut items {
        if looks.get(&look(item)).copied().unwrap_or(0) < 2 {
            continue;
        }
        let Some(added) = item.additional_text_edits.as_ref().and_then(|edits| edits.iter().map(|e| e.new_text.trim()).find(|t| !t.is_empty())) else {
            continue;
        };
        let added = added.lines().next().unwrap_or_default().to_string();
        let details = item.label_details.get_or_insert_default();
        details.description = Some(match details.description.take() {
            Some(was) => format!("{was} · {added}"),
            None => added,
        });
    }
    items
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

#[allow(clippy::too_many_arguments)]
fn read_loop(
    stdout: impl Read,
    out: mpsc::Sender<Out>,
    pending: Pending,
    progress: Progress,
    import: Import,
    problems: Problems,
    opened: Opened,
    commanded: Commanded,
    settings: Value,
    warming: Warming,
) {
    read_messages(stdout, out, &pending, progress, import, problems, opened, &commanded, &settings, &warming);
    // The server is gone: what waits for its answer need not wait out its time.
    pending.lock().unwrap().clear();
}

#[allow(clippy::too_many_arguments)]
fn read_messages(
    stdout: impl Read,
    out: mpsc::Sender<Out>,
    pending: &Pending,
    progress: Progress,
    import: Import,
    problems: Problems,
    opened: Opened,
    commanded: &Commanded,
    settings: &Value,
    warming: &Warming,
) {
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
                let method = method.as_str().unwrap_or_default();
                let result = if method == "workspace/configuration" {
                    let items = message["params"]["items"].as_array().map(Vec::as_slice).unwrap_or_default();
                    Value::Array(items.iter().map(|item| section(settings, item["section"].as_str().unwrap_or_default())).collect())
                } else if method == "workspace/applyEdit" || method == "window/showDocument" {
                    // Applied by the editor once the command answers; asked
                    // outside of one, refused rather than left undone unsaid.
                    let taken = match commanded.lock().unwrap().as_mut() {
                        Some(asked) => {
                            asked.push((method.to_string(), message["params"].clone()));
                            true
                        }
                        None => false,
                    };
                    match method {
                        "workspace/applyEdit" if taken => json!({ "applied": true }),
                        "workspace/applyEdit" => json!({ "applied": false, "failureReason": "only during a command the editor ran" }),
                        _ => json!({ "success": taken }),
                    }
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
                        let mut progress = progress.lock().unwrap();
                        progress.remove(&token);
                        // All it had to do is done: the folders added are read.
                        if progress.is_empty() {
                            warming.lock().unwrap().clear();
                        }
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

/// The settings a server is given, as an editor's settings file would
/// hold them: asked for by `workspace/configuration` a section at a time,
/// and told once after `initialized`, for those that only listen. Mostly
/// hints, which several servers leave off unless asked.
fn settings(program: &str) -> Value {
    match program {
        "gopls" => json!({ "gopls": init_options(program) }),
        // kotlin-lsp shows only the kinds of hints named here as on; with
        // none it shows none.
        "kotlin-lsp" => {
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
            let hints: serde_json::Map<_, _> = ON.iter().map(|kind| (kind.to_string(), Value::Bool(true))).collect();
            json!({ "jetbrains": { "kotlin": { "hints": hints } } })
        }
        // Its own default, "recommended", underlines most of a project not
        // written for it; a project's pyproject.toml still has the last word.
        "basedpyright-langserver" => json!({ "basedpyright": { "analysis": { "typeCheckingMode": "standard" } } }),
        "lua-language-server" => json!({ "Lua": { "hint": { "enable": true, "setType": true, "paramName": "All", "arrayIndex": "Disable" } } }),
        "jdtls" => init_options(program)["settings"].clone(),
        "metals" => json!({
            "metals": {
                "inlayHints": {
                    "inferredTypes": { "enable": true },
                    "implicitArguments": { "enable": true },
                    "typeParameters": { "enable": true },
                    "hintsInPatternMatch": { "enable": true },
                },
            },
        }),
        "yaml-language-server" => json!({ "yaml": { "schemaStore": { "enable": true }, "hover": true, "completion": true, "validate": true } }),
        "vscode-json-language-server" => json!({ "json": { "validate": { "enable": true }, "schemas": json_schemas() } }),
        "taplo" => json!({ "evenBetterToml": { "schema": { "enabled": true, "catalogs": [], "associations": toml_schemas() } } }),
        _ => Value::Null,
    }
}

/// One section of the settings, by its dotted name: `jetbrains.kotlin`.
fn section(settings: &Value, name: &str) -> Value {
    name.split('.').filter(|part| !part.is_empty()).fold(settings, |at, part| &at[part]).clone()
}

/// `initializationOptions` the server needs, wherever it was installed.
pub fn init_options(program: &str) -> Value {
    match program {
        // Go to symbol finds functions and fields too: by itself it knows only types.
        "rust-analyzer" => json!({ "workspace": { "symbol": { "search": { "kind": "all_symbols" } } } }),
        "gopls" => json!({
            "hints": {
                "assignVariableTypes": true,
                "compositeLiteralFields": true,
                "constantValues": true,
                "functionTypeParameters": true,
                "parameterNames": true,
                "rangeVariableTypes": true,
            },
        }),
        "typescript-language-server" => json!({
            "preferences": {
                "includeInlayParameterNameHints": "all",
                "includeInlayParameterNameHintsWhenArgumentMatchesName": false,
                "includeInlayVariableTypeHints": true,
                "includeInlayVariableTypeHintsWhenTypeMatchesName": false,
                "includeInlayFunctionParameterTypeHints": true,
                "includeInlayFunctionLikeReturnTypeHints": true,
                "includeInlayPropertyDeclarationTypeHints": true,
                "includeInlayEnumMemberValueHints": true,
            },
        }),
        // Its own copy of the key, or none: it looks for licence.txt itself.
        "intelephense" => match crate::secrets::get(INTELEPHENSE_LICENCE) {
            Some(key) => json!({ "licenceKey": key }),
            None => Value::Null,
        },
        "phpactor" => json!({
            "language_server_worse_reflection.inlay_hints.enable": true,
            "language_server_worse_reflection.inlay_hints.types": true,
            "language_server_worse_reflection.inlay_hints.params": true,
        }),
        "jdtls" => json!({ "settings": { "java": { "inlayHints": { "parameterNames": { "enabled": "all" } } } } }),
        "ruby-lsp" => json!({ "featuresConfiguration": { "inlayHint": { "enableAll": true } } }),
        "vscode-json-language-server" | "vscode-css-language-server" | "vscode-html-language-server" => {
            json!({ "provideFormatter": true })
        }
        _ => Value::Null,
    }
}

/// The schemas of JSON files most projects have, which the server fetches
/// from SchemaStore: what a key means, on hover, and a wrong one marked.
fn json_schemas() -> Value {
    const SCHEMAS: &[(&[&str], &str)] = &[
        (&["package.json"], "package"),
        (&["tsconfig.json", "tsconfig.*.json", "jsconfig.json"], "tsconfig"),
        (&[".eslintrc", ".eslintrc.json"], "eslintrc"),
        (&[".prettierrc", ".prettierrc.json"], "prettierrc"),
        (&[".babelrc", "babel.config.json"], "babelrc"),
        (&["renovate.json"], "renovate"),
        (&["appsettings.json", "appsettings.*.json"], "appsettings"),
    ];
    let mut schemas: Vec<Value> = SCHEMAS
        .iter()
        .map(|(files, name)| json!({ "fileMatch": files, "url": format!("https://www.schemastore.org/{name}.json") }))
        .collect();
    schemas.push(json!({ "fileMatch": ["composer.json"], "url": "https://getcomposer.org/schema.json" }));
    Value::Array(schemas)
}

/// The same for TOML. Taplo would find these in SchemaStore's catalog,
/// but cannot read the catalog as it is now.
fn toml_schemas() -> Value {
    const SCHEMAS: &[(&str, &str)] = &[
        (r"(^|/)Cargo\.toml$", "cargo"),
        (r"(^|/)\.cargo/config(\.toml)?$", "cargo-config"),
        (r"(^|/)pyproject\.toml$", "pyproject"),
        (r"(^|/)\.?rustfmt\.toml$", "rustfmt"),
        (r"(^|/)\.?ruff\.toml$", "ruff"),
        (r"(^|/)uv\.toml$", "uv"),
        (r"(^|/)\.?taplo\.toml$", "taplo"),
    ];
    SCHEMAS.iter().map(|(file, name)| (file.to_string(), json!(format!("https://www.schemastore.org/{name}.json")))).collect::<serde_json::Map<_, _>>().into()
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

/// Arguments that depend on the project. jdtls keeps its index in a
/// workspace of its own, by default one per directory *name*: two tasks'
/// worktrees of one name would share it, and the second server fails on
/// its lock. One per project path instead.
fn project_args(program: &str, root: &Path) -> Vec<std::ffi::OsString> {
    if program != "jdtls" {
        return Vec::new();
    }
    use sha2::Digest as _;
    let hash = sha2::Sha256::digest(root.to_string_lossy().as_bytes());
    let name: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let data = jdtls_home().join(name);
    // Which project it is of, for `prune_jdtls` once the worktree is gone.
    let (marker, project) = (data.join(PROJECT_MARK), root.to_path_buf());
    std::thread::spawn(move || {
        let _ = std::fs::create_dir_all(marker.parent().unwrap_or(&marker));
        let _ = std::fs::write(&marker, project.to_string_lossy().as_bytes());
    });
    vec!["-data".into(), data.into_os_string()]
}

const PROJECT_MARK: &str = ".ideae-project";

fn jdtls_home() -> PathBuf {
    crate::state::data_home().join("ideae").join("jdtls")
}

/// jdtls keeps an index per project, tens of MB: those of worktrees that
/// are gone go. Run off the UI thread.
pub fn prune_jdtls() {
    prune_jdtls_in(&jdtls_home());
}

fn prune_jdtls_in(home: &Path) {
    let Ok(entries) = std::fs::read_dir(home) else { return };
    for entry in entries.flatten() {
        let project = std::fs::read_to_string(entry.path().join(PROJECT_MARK)).ok();
        if project.is_some_and(|project| !Path::new(project.trim()).exists()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Variables a server needs that the user's environment does not give.
/// Android's Gradle plugin fails to configure a project without the SDK,
/// which Android Studio names in an untracked local.properties: a new
/// worktree has none.
fn server_env(program: &str) -> Vec<(&'static str, std::ffi::OsString)> {
    let mut env = Vec::new();
    if let Some(options) = java_options(program, std::env::var_os("JAVA_TOOL_OPTIONS")) {
        env.push(("JAVA_TOOL_OPTIONS", options));
    }
    if program != "kotlin-lsp" || ["ANDROID_HOME", "ANDROID_SDK_ROOT"].iter().any(|v| std::env::var_os(v).is_some()) {
        return env;
    }
    let sdk = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Android/sdk"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Android").join("Sdk"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Android/Sdk"))
    };
    env.extend(sdk.filter(|dir| dir.is_dir()).map(|dir| ("ANDROID_HOME", dir.into_os_string())));
    env
}

/// The JVM servers keep the heap they once grew to, gigabytes, for as long
/// as they run; G1 gives back what is free when it collects every 30 s of
/// quiet (about a fifth of kotlin-lsp and its Gradle daemon). Every JVM
/// they start reads it too, of any Java: one that does not know a flag
/// (before 12) or another collector leaves it, rather than not start. The
/// user's own options go after, and win.
fn java_options(program: &str, user: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    if !matches!(program, "kotlin-lsp" | "jdtls") {
        return None;
    }
    let mut options = std::ffi::OsString::from(
        "-XX:+IgnoreUnrecognizedVMOptions -XX:G1PeriodicGCInterval=30000 -XX:MinHeapFreeRatio=10 -XX:MaxHeapFreeRatio=30",
    );
    if let Some(user) = user.filter(|u| !u.is_empty()) {
        options.push(" ");
        options.push(user);
    }
    Some(options)
}

#[cfg(test)]
mod completion_tests {
    use lsp_types::{CompletionItem, CompletionItemLabelDetails, Position, Range, TextEdit};

    fn item(label: &str, sort: &str) -> CompletionItem {
        CompletionItem { label: label.into(), sort_text: Some(sort.into()), ..Default::default() }
    }

    fn import(line: &str) -> Option<Vec<TextEdit>> {
        Some(vec![TextEdit { range: Range::new(Position::new(0, 0), Position::new(0, 0)), new_text: format!("{line}\n") }])
    }

    #[test]
    fn twins_are_merged_and_namesakes_told_apart() {
        let package = |label: &str, sort: &str, package: Option<&str>| CompletionItem {
            label_details: package.map(|p| CompletionItemLabelDetails { detail: None, description: Some(p.into()) }),
            ..item(label, sort)
        };
        let items = vec![
            package("Main", "1", Some("app")),
            package("Main", "2", Some("app")),
            package("Main", "3", Some("tools")),
            CompletionItem { additional_text_edits: import("import a.Main"), ..item("Main", "4") },
            CompletionItem { additional_text_edits: import("import b.Main"), ..item("Main", "5") },
            CompletionItem { additional_text_edits: import("import b.Main"), ..item("Main", "6") },
            item("main", "0"),
        ];
        let shown: Vec<(String, Option<String>)> = super::arrange_completions(items, "Ma")
            .into_iter()
            .map(|i| (i.label, i.label_details.and_then(|d| d.description)))
            .collect();
        let expected = [
            ("main", None),
            ("Main", Some("app")),
            ("Main", Some("tools")),
            ("Main", Some("import a.Main")),
            ("Main", Some("import b.Main")),
        ];
        assert_eq!(shown, expected.map(|(l, d)| (l.to_string(), d.map(str::to_string))));
    }

    #[test]
    fn the_typed_word_filters_and_sort_text_orders() {
        let items = vec![
            item("zeta", "2"),
            CompletionItem { filter_text: Some("println".into()), ..item("println!", "1") },
            item("print", "3"),
            item("other", "0"),
        ];
        let labels: Vec<String> = super::arrange_completions(items, "PRn").into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["println!", "print"]);
        assert_eq!(super::arrange_completions(vec![item("b", "2"), item("a", "1")], "").len(), 2);
    }

    #[test]
    fn edits_beside_a_completion_land_around_it() {
        let before = "fn main() {\n    Has\n}\n";
        let at = before.find("Has").unwrap();
        let after = before.replace("Has", "HashMap::new()");
        let edits = vec![
            TextEdit { range: Range::new(Position::new(0, 0), Position::new(0, 0)), new_text: "use std::collections::HashMap;\n\n".into() },
            TextEdit { range: Range::new(Position::new(2, 1), Position::new(2, 1)), new_text: " // end".into() },
            TextEdit { range: Range::new(Position::new(1, 5), Position::new(1, 6)), new_text: "x".into() },
        ];
        let made = super::apply_edits_around(before, at..at + 3, "HashMap::new()".len(), &after, &edits);
        assert_eq!(made, "use std::collections::HashMap;\n\nfn main() {\n    HashMap::new()\n} // end\n");
        assert_eq!(super::offset_of("ab\nc你d", &Position::new(1, 2)), 7);
    }
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
        // A name whose end is not a whole character there.
        assert_eq!(super::template_of("Отчёт для клиента"), "Отчёт для клиента");
        assert_eq!(of("примечание.md"), "markdown");
        assert_eq!(of("настройки.json.example"), "json");
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    /// A jdtls workspace goes with its project; one without a mark is not ours to judge.
    #[test]
    fn jdtls_workspaces_of_projects_gone_are_pruned() {
        let home = std::env::temp_dir().join(format!("ideae-jdtls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for (name, project) in [("kept", Some(home.clone())), ("gone", Some(home.join("no-such-project"))), ("unmarked", None)] {
            std::fs::create_dir_all(home.join(name)).unwrap();
            if let Some(project) = project {
                std::fs::write(home.join(name).join(PROJECT_MARK), project.to_string_lossy().as_bytes()).unwrap();
            }
        }
        prune_jdtls_in(&home);
        assert!(home.join("kept").exists() && home.join("unmarked").exists() && !home.join("gone").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// kotlin-lsp shows a kind of hint only when it is named as on.
    #[test]
    fn kotlin_lsp_is_told_which_hints_to_show() {
        let kotlin = section(&settings("kotlin-lsp"), "jetbrains.kotlin");
        assert_eq!(kotlin["hints"]["type.variable"], Value::Bool(true));
        assert_eq!(kotlin["hints"]["parameters"], Value::Bool(true));
        assert_eq!(section(&settings("intelephense"), "intelephense"), Value::Null);
    }

    /// gopls asks for its settings after starting with them: the answer
    /// must not take back the hints its options turned on.
    #[test]
    fn gopls_is_asked_what_it_started_with() {
        let asked = section(&settings("gopls"), "gopls");
        assert_eq!(asked, init_options("gopls"));
        assert_eq!(asked["hints"]["parameterNames"], Value::Bool(true));
        assert_eq!(section(&settings("lua-language-server"), "Lua.hint.enable"), Value::Bool(true));
        assert_eq!(section(&settings("gopls"), "nothing.here"), Value::Null);
    }

    #[test]
    fn files_find_their_server_by_extension_or_name() {
        let program = |path: &str| known(Path::new(path)).map(|(language, program, _)| (language, program));
        assert_eq!(program("/a/Dockerfile"), Some(("dockerfile", "docker-langserver")));
        assert_eq!(program("/a/Dockerfile.dev"), Some(("dockerfile", "docker-langserver")));
        assert_eq!(program("/a/app.dockerfile"), Some(("dockerfile", "docker-langserver")));
        assert_eq!(program("/a/.bashrc"), Some(("shellscript", "bash-language-server")));
        assert_eq!(program("/a/Gemfile"), Some(("ruby", "ruby-lsp")));
        assert_eq!(program("/a/main.py"), Some(("python", "basedpyright-langserver")));
        assert_eq!(program("/a/x.hpp"), Some(("cpp", "clangd")));
        assert_eq!(program("/a/Cargo.toml"), Some(("toml", "taplo")));
        assert_eq!(program("/a/dockerfile_test.go"), Some(("go", "gopls")));
        assert_eq!(program("/a/Gemfile.lock"), None);
        assert_eq!(program("/a/tsconfig.app.json"), Some(("jsonc", "vscode-json-language-server")));
        assert_eq!(program("/a/tsconfig.ts"), Some(("typescript", "typescript-language-server")));
        assert_eq!(program("/a/data.json"), Some(("json", "vscode-json-language-server")));
        assert_eq!(program("/a/notes.txt"), None);
    }

    #[test]
    fn every_extension_has_one_server() {
        let mut seen = HashSet::new();
        for def in SERVERS {
            assert!(!def.languages.is_empty(), "{}", def.program);
            // A language's other servers serve what its first does.
            if ALTERNATIVES.iter().any(|(_, _, programs)| programs[1..].contains(&def.program)) {
                continue;
            }
            for (ext, _) in def.languages {
                assert!(seen.insert(*ext), ".{ext} twice");
            }
        }
    }

    #[test]
    fn jvm_servers_give_memory_back() {
        assert_eq!(java_options("rust-analyzer", None), None);
        let ours = java_options("kotlin-lsp", None).unwrap();
        assert!(ours.to_string_lossy().starts_with("-XX:+IgnoreUnrecognizedVMOptions "));
        let with_user = java_options("jdtls", Some("-Xmx4g".into())).unwrap();
        assert!(with_user.to_string_lossy().ends_with(" -Xmx4g"));
    }

    #[test]
    fn a_language_runs_the_server_chosen_for_it() {
        let mut settings = crate::state::Settings::default();
        assert_eq!(chosen("php", &settings), "intelephense");
        settings.lsp_choice.insert("php".into(), "phpactor".into());
        assert_eq!(chosen("php", &settings), "phpactor");
        settings.lsp_choice.insert("php".into(), "gone".into());
        assert_eq!(chosen("php", &settings), "intelephense");
        for (_, _, programs) in ALTERNATIVES {
            for program in *programs {
                assert!(SERVERS.iter().any(|d| d.program == *program), "{program}");
            }
        }
    }
}

/// Real servers, one project each: what the editor asks of every one, with
/// the settings and options the app gives it. Each needs its server on PATH
/// or installed under IDEAE_LSP_DIR (the lsp_install tests put it there
/// when given one): `cargo test lsp::server_tests -- --ignored`.
#[cfg(test)]
mod server_tests {
    use super::*;

    struct Answers {
        hover: String,
        definition: Vec<lsp_types::LocationLink>,
        hints: Vec<String>,
    }

    fn label(hint: &lsp_types::InlayHint) -> String {
        match &hint.label {
            lsp_types::InlayHintLabel::String(text) => text.clone(),
            lsp_types::InlayHintLabel::LabelParts(parts) => parts.iter().map(|p| p.value.as_str()).collect(),
        }
    }

    /// Writes `files`, opens the first and asks at the first `at` in it,
    /// waiting out the server's indexing; hints too when they are expected.
    fn ask(program: &str, files: &[(&str, &str)], at: &str, hints: bool) -> Answers {
        let base = std::env::temp_dir().join(format!("ideae-server-{program}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for (name, text) in files {
            let path = base.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let (name, text) = files[0];
        let file = base.join(name);
        let (language, _, args) = known(&file).unwrap();
        let command = locate(program).unwrap_or_else(|| panic!("{program} is not installed"));
        let root = project_root(program, &file, &base);
        let options = crate::lsp_install::init_options(program, &command);
        let client = LspClient::spawn(program, &command, args, &root, options).unwrap();
        client.did_open(&file, language, text).unwrap();
        let uri = path_to_uri(&file);
        let (line, row) = text.lines().enumerate().find(|(_, l)| l.contains(at)).unwrap();
        let pos = lsp_types::Position::new(line as u32, row.find(at).unwrap() as u32 + 1);
        let started = std::time::Instant::now();
        let waited = || started.elapsed() > Duration::from_secs(120);
        let hover = loop {
            let hover = client.hover(&uri, pos).ok().flatten().map(|h| format!("{:?}", h.contents)).unwrap_or_default();
            // LuaLS answers with its progress until it has read the project.
            if (!hover.is_empty() && !hover.contains("Workspace loading")) || waited() {
                break hover;
            }
            thread::sleep(Duration::from_millis(500));
        };
        let definition = client.definition(&uri, pos).unwrap_or_default();
        let whole = lsp_types::Range::new(lsp_types::Position::new(0, 0), lsp_types::Position::new(text.lines().count() as u32, 0));
        let hints = loop {
            let found: Vec<String> = client.inlay_hints(&uri, whole).unwrap_or_default().iter().map(label).collect();
            if !found.is_empty() || !hints || waited() {
                break found;
            }
            thread::sleep(Duration::from_millis(500));
        };
        eprintln!(
            "{program} after {:?}\n  hover {hover:.300}\n  definition {:?}\n  hints {hints:?}",
            started.elapsed(),
            definition.first().map(|l| l.target_range)
        );
        drop(client);
        let _ = std::fs::remove_dir_all(&base);
        Answers { hover, definition, hints }
    }

    fn position_of(text: &str, offset: usize) -> lsp_types::Position {
        let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
        let line = text[..line_start].matches('\n').count() as u32;
        lsp_types::Position::new(line, text[line_start..offset].chars().count() as u32)
    }

    /// The first file with `|` where the cursor is: completion there, the
    /// item `chosen` picks taken as the editor takes it (its edit or
    /// snippet over the typed word, resolved, the edits beside it, then its
    /// command and what that asks back). The text after it, and the cursor.
    fn accept(program: &str, files: &[(&str, &str)], chosen: &dyn Fn(&lsp_types::CompletionItem) -> bool) -> (String, usize) {
        let base = std::env::temp_dir().join(format!("ideae-accept-{program}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let text = files[0].1.replace('|', "");
        let cursor = files[0].1.find('|').unwrap();
        for (i, (name, content)) in files.iter().enumerate() {
            let path = base.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, if i == 0 { text.as_str() } else { content }).unwrap();
        }
        let file = base.join(files[0].0);
        let (language, _, args) = known(&file).unwrap();
        let command = locate(program).unwrap_or_else(|| panic!("{program} is not installed"));
        let root = project_root(program, &file, &base);
        let options = crate::lsp_install::init_options(program, &command);
        let client = LspClient::spawn(program, &command, args, &root, options).unwrap();
        client.did_open(&file, language, &text).unwrap();
        let uri = path_to_uri(&file);
        let typed: String = text[..cursor].chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<Vec<_>>().into_iter().rev().collect();
        let started = std::time::Instant::now();
        let item = loop {
            let items = match client.completion(&uri, position_of(&text, cursor), None) {
                Ok(lsp_types::CompletionResponse::Array(items)) => items,
                Ok(lsp_types::CompletionResponse::List(list)) => list.items,
                Err(_) => Vec::new(),
            };
            if let Some(item) = arrange_completions(items, &typed).into_iter().find(|item| chosen(item)) {
                break item;
            }
            assert!(started.elapsed() < Duration::from_secs(240), "{program}: the item never came");
            thread::sleep(Duration::from_secs(1));
        };
        eprintln!("{program}: {}", serde_json::to_string(&item).unwrap());

        let word = cursor - typed.len();
        let (mut range, mut new_text) = (word..cursor, item.label.clone());
        match &item.text_edit {
            Some(lsp_types::CompletionTextEdit::Edit(edit)) => {
                range = offset_of(&text, &edit.range.start)..offset_of(&text, &edit.range.end);
                new_text.clone_from(&edit.new_text);
            }
            Some(lsp_types::CompletionTextEdit::InsertAndReplace(edit)) => {
                range = offset_of(&text, &edit.replace.start)..offset_of(&text, &edit.replace.end);
                new_text.clone_from(&edit.new_text);
            }
            None => new_text = item.insert_text.clone().unwrap_or(new_text),
        }
        let mut caret = new_text.len()..new_text.len();
        if item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET) {
            (new_text, caret) = gpui_kit::base::input::expand_snippet(&new_text);
        }
        let mut after = format!("{}{new_text}{}", &text[..range.start], &text[range.end..]);
        let mut at = range.start + caret.start;
        let item = if item.additional_text_edits.is_none() && client.resolves_completions() {
            let resolved = client.resolve_completion(&item).unwrap();
            lsp_types::CompletionItem { command: resolved.command.clone().or(item.command), ..resolved }
        } else {
            item
        };
        if let Some(edits) = &item.additional_text_edits {
            let made = apply_edits_around(&text, range.clone(), new_text.len(), &after, edits);
            at += made.len() - after.len();
            after = made;
        }
        if let Some(command) = &item.command {
            client.did_change(&file, 0, &after).unwrap();
            let done = client.execute_command(command).unwrap();
            for edit in &done.edits {
                for (path, edits) in edits_by_file(edit) {
                    assert_eq!(path.canonicalize().unwrap(), file.canonicalize().unwrap());
                    after = apply_edits(&after, &edits);
                }
            }
            if let Some((_, selection)) = done.show {
                at = offset_of(&after, &selection.start);
            }
        }
        drop(client);
        let _ = std::fs::remove_dir_all(&base);
        (after, at)
    }

    /// rust-analyzer: a type not imported yet comes with its `use`, a
    /// method as a snippet with the caret inside its parentheses.
    #[test]
    #[ignore]
    fn rust_analyzer_completion_imports_and_snippets() {
        let cargo = ("Cargo.toml", "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        let main = "fn main() {\n    let m: HashMa| = Default::default();\n}\n";
        let (text, _) = accept("rust-analyzer", &[("src/main.rs", main), cargo], &|item| item.label.starts_with("HashMap"));
        assert!(text.starts_with("use std::collections::HashMap;\n"), "{text}");
        assert!(text.contains("let m: HashMap<> = "), "the caret between the brackets: {text}");

        let main = "fn main() {\n    let mut v: Vec<u8> = Vec::new();\n    v.pu|\n}\n";
        let (text, at) = accept("rust-analyzer", &[("src/main.rs", main), cargo], &|item| item.label.starts_with("push"));
        assert!(text.contains("v.push("), "{text}");
        assert_eq!(&text[at - 1..at], "(", "the caret inside: {text}");
    }

    /// gopls: a package not imported yet gets its import.
    #[test]
    #[ignore]
    fn gopls_completion_imports() {
        let main = "package main\n\nfunc main() {\n\t_ = strings.Spl|\n}\n";
        let (text, _) = accept("gopls", &[("main.go", main), ("go.mod", "module example.com/m\n\ngo 1.21\n")], &|item| item.label == "Split");
        assert!(text.contains("import \"strings\""), "{text}");
        assert!(text.contains("strings.Split("), "{text}");
    }

    /// kotlin-lsp: the item's edit is empty; its command asks the editor
    /// to make the edit and to put the caret after it.
    #[test]
    #[ignore]
    fn kotlin_lsp_completion_by_command() {
        let main = "data class Point(val x: Int, val yield: Int)\n\nfun main() {\n    val p = Point(1, 2)\n    println(p.yi|)\n}\n";
        let files = [
            ("src/main/kotlin/Main.kt", main),
            ("settings.gradle.kts", "rootProject.name = \"p\"\n"),
            ("build.gradle.kts", "plugins { kotlin(\"jvm\") version \"2.1.0\" }\nrepositories { mavenCentral() }\n"),
        ];
        let (text, at) = accept("kotlin-lsp", &files, &|item| item.label == "yield");
        assert!(text.contains("println(p.yield)"), "{text}");
        assert_eq!(&text[at..at + 1], ")", "the caret after it: {text}");
    }

    /// basedpyright: a name from the standard library not imported yet.
    #[test]
    #[ignore]
    fn basedpyright_completion_imports() {
        let main = "def main():\n    return OrderedDic|\n";
        let (text, _) = accept("basedpyright-langserver", &[("main.py", main), ("pyproject.toml", "[project]\nname = \"p\"\n")], &|item| {
            item.label == "OrderedDict" && serde_json::to_string(item).unwrap().contains("collections")
        });
        assert!(text.contains("from collections import OrderedDict"), "{text}");
        assert!(text.contains("return OrderedDict"), "{text}");
    }

    /// typescript-language-server: an export of another file, imported.
    #[test]
    #[ignore]
    fn typescript_completion_imports() {
        let main = "const x = greetEvery|\n";
        let other = "export function greetEveryone(): string { return \"hi\" }\n";
        let files = [("a.ts", main), ("b.ts", other), ("package.json", "{}\n"), ("tsconfig.json", "{}\n")];
        let (text, _) = accept("typescript-language-server", &files, &|item| item.label == "greetEveryone");
        assert!(text.contains("import { greetEveryone } from \"./b\""), "{text}");
        assert!(text.contains("const x = greetEveryone"), "{text}");
    }

    #[test]
    #[ignore]
    fn gopls_shows_types_and_parameter_names() {
        let main = "package main\n\nfunc add(left, right int) int { return left + right }\n\nfunc main() {\n\tx := add(1, 2)\n\t_ = x\n}\n";
        let answers = ask("gopls", &[("main.go", main), ("go.mod", "module example.com/m\n\ngo 1.21\n")], "add(1", true);
        assert!(answers.hover.contains("func add"), "{}", answers.hover);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 2);
        assert!(answers.hints.iter().any(|h| h.contains("left")) && answers.hints.iter().any(|h| h.contains("int")), "{:?}", answers.hints);
    }

    #[test]
    #[ignore]
    fn typescript_shows_types_and_parameter_names() {
        let main = "function greet(name: string, times: number) {\n  return name.repeat(times)\n}\nconst x = greet(\"a\", 2)\n";
        let answers = ask("typescript-language-server", &[("a.ts", main), ("package.json", "{}\n")], "greet(\"a", true);
        assert!(answers.hover.contains("function greet"), "{}", answers.hover);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 0);
        assert!(answers.hints.iter().any(|h| h.contains("name:")) && answers.hints.iter().any(|h| h.contains("string")), "{:?}", answers.hints);
    }

    /// Imports the Gradle build first: a minute or more, with network.
    #[test]
    #[ignore]
    fn kotlin_lsp_shows_types_and_parameter_names() {
        let main = "fun greet(name: String, times: Int): String = name.repeat(times)\n\nfun main() {\n    val word = greet(\"hi\", 3)\n    println(word)\n}\n";
        let files = [
            ("src/main/kotlin/Main.kt", main),
            ("settings.gradle.kts", "rootProject.name = \"p\"\n"),
            ("build.gradle.kts", "plugins { kotlin(\"jvm\") version \"2.1.0\" }\nrepositories { mavenCentral() }\n"),
        ];
        let answers = ask("kotlin-lsp", &files, "greet(\"hi", true);
        assert!(answers.hover.contains("greet"), "{}", answers.hover);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 0);
        assert!(answers.hints.iter().any(|h| h.contains("name")) && answers.hints.iter().any(|h| h.contains("String")), "{:?}", answers.hints);
    }

    #[test]
    #[ignore]
    fn basedpyright_shows_types_and_parameter_names() {
        let main = "def greet(name: str, times: int):\n    return name * times\n\n\nx = greet(\"a\", 2)\n";
        let answers = ask("basedpyright-langserver", &[("main.py", main), ("pyproject.toml", "[project]\nname = \"p\"\n")], "greet(\"a", true);
        assert!(answers.hover.contains("def greet"), "{}", answers.hover);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 0);
        assert!(answers.hints.iter().any(|h| h.contains("name")) && answers.hints.iter().any(|h| h.contains("str")), "{:?}", answers.hints);
    }

    #[test]
    #[ignore]
    fn clangd_shows_types_and_parameter_names() {
        let main = "int add(int left, int right) { return left + right; }\nint main() {\n  auto x = add(1, 2);\n  return x;\n}\n";
        let answers = ask("clangd", &[("main.cpp", main), ("compile_flags.txt", "-std=c++17\n")], "add(1", true);
        assert!(answers.hover.contains("add"), "{}", answers.hover);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 0);
        assert!(answers.hints.iter().any(|h| h.contains("left")) && answers.hints.iter().any(|h| h.contains("int")), "{:?}", answers.hints);
    }

    #[test]
    #[ignore]
    fn jdtls_shows_parameter_names() {
        let main = "public class Main {\n    static int add(int left, int right) { return left + right; }\n\n    public static void main(String[] args) {\n        var x = add(1, 2);\n        System.out.println(x);\n    }\n}\n";
        let answers = ask("jdtls", &[("src/Main.java", main)], "add(1", true);
        assert!(answers.hover.contains("add"), "{}", answers.hover);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 1);
        assert!(answers.hints.iter().any(|h| h.contains("left")), "{:?}", answers.hints);
    }

    #[test]
    #[ignore]
    fn csharp_ls_shows_parameter_names() {
        let main = "class Program {\n    static int Add(int left, int right) => left + right;\n    static void Main() {\n        var x = Add(1, 2);\n        System.Console.WriteLine(x);\n    }\n}\n";
        let project = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n    <TargetFramework>net10.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n";
        let answers = ask("csharp-ls", &[("Program.cs", main), ("p.csproj", project)], "Add(1", true);
        assert!(answers.hover.contains("Add"), "{}", answers.hover);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 1);
        assert!(answers.hints.iter().any(|h| h.contains("left")), "{:?}", answers.hints);
    }

    #[test]
    #[ignore]
    fn lua_language_server_shows_types_and_parameter_names() {
        let main = "local function add(left, right)\n  return left + right\nend\n\nlocal x = add(1, 2)\nprint(x)\n";
        let answers = ask("lua-language-server", &[("main.lua", main)], "add(1", true);
        assert!(answers.hover.contains("add"), "{}", answers.hover);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 0);
        assert!(answers.hints.iter().any(|h| h.contains("left")), "{:?}", answers.hints);
    }

    #[test]
    #[ignore]
    fn zls_shows_types_and_parameter_names() {
        let main = "const std = @import(\"std\");\n\nfn add(left: i32, right: i32) i32 {\n    return left + right;\n}\n\npub fn main() void {\n    const x = add(1, 2);\n    std.debug.print(\"{}\\n\", .{x});\n}\n";
        let answers = ask("zls", &[("main.zig", main)], "add(1", true);
        assert!(answers.hover.contains("add"), "{}", answers.hover);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 2);
        assert!(answers.hints.iter().any(|h| h.contains("left")) && answers.hints.iter().any(|h| h.contains("i32")), "{:?}", answers.hints);
    }

    #[test]
    #[ignore]
    fn bash_language_server_finds_functions() {
        let main = "greet() {\n  echo hi\n}\n\ngreet world\n";
        let answers = ask("bash-language-server", &[("main.sh", main)], "greet world", false);
        assert_eq!(answers.definition[0].target_selection_range.start.line, 0);
    }

    #[test]
    #[ignore]
    fn the_extracted_servers_hover() {
        let css = ask("vscode-css-language-server", &[("a.css", "a {\n  color: red;\n}\n")], "color", false);
        assert!(css.hover.contains("color"), "{}", css.hover);
        let html = ask("vscode-html-language-server", &[("a.html", "<html>\n<body>\n<div class=\"x\"></div>\n</body>\n</html>\n")], "div", false);
        assert!(html.hover.contains("div"), "{}", html.hover);
        // Its schema comes from SchemaStore: needs network.
        let json = ask("vscode-json-language-server", &[("package.json", "{\n  \"name\": \"x\",\n  \"version\": \"1.0.0\"\n}\n")], "\"version", false);
        assert!(json.hover.to_lowercase().contains("version"), "{}", json.hover);
    }

    #[test]
    #[ignore]
    fn yaml_language_server_knows_a_workflow() {
        let workflow = "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n";
        let answers = ask("yaml-language-server", &[(".github/workflows/ci.yml", workflow)], "runs-on", false);
        assert!(!answers.hover.is_empty());
    }

    #[test]
    #[ignore]
    fn taplo_knows_cargo_toml() {
        let manifest = "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
        let answers = ask("taplo", &[("Cargo.toml", manifest)], "edition", false);
        assert!(answers.hover.to_lowercase().contains("edition"), "{}", answers.hover);
    }

    #[test]
    #[ignore]
    fn marksman_follows_links() {
        let doc = "# Title\n\nText.\n\n## Usage\n\nSee [usage](#usage).\n";
        let answers = ask("marksman", &[("README.md", doc), (".marksman.toml", "")], "(#usage", false);
        assert_eq!(answers.definition[0].target_range.start.line, 4);
    }

    #[test]
    #[ignore]
    fn docker_langserver_hovers() {
        let answers = ask("docker-langserver", &[("Dockerfile", "FROM alpine:3\nRUN echo hi\n")], "RUN", false);
        assert!(answers.hover.contains("RUN"), "{}", answers.hover);
    }

    #[test]
    #[ignore]
    fn terraform_ls_finds_variables() {
        let main = "variable \"region\" {\n  default = \"x\"\n}\n\noutput \"where\" {\n  value = var.region\n}\n";
        let answers = ask("terraform-ls", &[("main.tf", main)], "var.region", false);
        assert_eq!(answers.definition[0].target_range.start.line, 0);
    }
}
