# ideae

An IDE for the agentic age: one window for running AI coding agents
(Claude Code by default), one task per git worktree.

Every task gets the agent in a terminal, more terminals, an editor, the
diff of its changes, git history, and a headless browser tab, and keeps
all of it while you switch between tasks.

Status: early. Linux (Ubuntu 24.04 or newer is what it is tested on); a
Windows build exists and is being tried out (see [Windows](#windows)).

## What it does

- **Projects and tasks.** Add a repository, start a task from a title. The
  title becomes a branch (`DEV-123 Fix login` gives `DEV-123_fixLogin`, any
  other text a slug with a random suffix). The task gets a worktree in
  `<repo>.worktrees/<branch>`, branched from the remote's default branch,
  with the main checkout's ignored `.env*` files copied in. Worktrees made
  outside the app show up as tasks too.
- **Variants.** A hard task can be started as up to four variants: the
  same prompt to as many agents, each in a worktree of its own from the
  same commit, with a line for each one alone if you like (a click
  fills one in: the smallest patch, a failing test first…). Their
  agents run in Claude Code's auto mode, so that the same question is
  not asked in each. They show
  as one group. Its screen has, for each variant, what its agent says it
  did, what it changed and what it cost, and which variant changed which
  file; any two of them can be compared with each other, file by file
  (A ↔ B), and going from one to the next keeps the file you were
  looking at in front; a message goes to all of them at once. A command typed there
  (tests, a benchmark) runs in every variant, one after another so that
  their times compare; with Auto it runs by itself when all of them
  have finished. Then a judge, one more Claude Code run that can only
  read, gives its opinion on which to keep and what to take from the
  others, and that can be sent on to the agent of the one it picks.
  What it said and how the command went are kept over a restart.
  For a site, "Start servers" runs every variant's services on ports of
  its own and Site on a card opens it; the judge, asked while they run,
  gets a picture of each and the cards show it. A, B, C above a variant switch
  between them with the same view in front, the same page of the site
  too. Keep one, or discard all: what is removed stays under
  `refs/ideae/variants` for two weeks. "Another variant…" on any task
  gives it a second try.
- **Agent status.** Claude Code runs with hooks passed through
  `--settings`. Nothing is written to `~/.claude` or the repository. The
  sidebar shows working / needs input / done, and a desktop notification
  fires when a task out of view wants you. The Changes and Files panels
  mark the file the agent is writing to and those it wrote to since your
  last prompt.
- **Files panel.** File and folder icons as JetBrains' new UI draws them.
  A name has the color of what git says of it: blue changed, green added,
  red-brown not added, olive ignored; a folder, of what is in it. An
  ignored folder (`target/`, `node_modules/`) is read from disk when it is
  opened. The Changes list colors its files the same, a deleted one gray.
- **Themes.** Ayu, Catppuccin, Gruvbox, Solarized, Everforest and
  Flexoki, each with a light and a dark side (the switch in the title
  bar: auto, light, dark). Theme… in the menu lists them and draws each
  as you go through the list: Enter keeps it, Esc goes back. Terminals take
  the theme's colors too.
- **Spaces.** Projects can be sorted into spaces: work, personal, a
  client (a project's menu in the sidebar; the spaces themselves under
  the mark at the right of the title bar). The one chosen there is all
  the window shows: the list, the search, what waits for you, the
  notifications, the tokens. The agents of the others go on working and
  nothing of them is told, but for a number by the mark when some wait;
  "All" shows everything. A filter and no more: accounts and the git
  identity are the same in every space.
- **Presenting.** Before the window is shown to others, ☰ → Enter
  presentation mode asks which tasks to leave in it. The rest leave the
  list, the search and the notifications and go on working; the load and
  token readouts are hidden, the buttons that add projects and tasks
  too. Files, diffs and terminals are drawn larger (Settings, 125% to
  start with), the sidebar becomes a rail, and a file git ignores (keys,
  local settings) shows a notice in place of what it holds until you ask
  for it. Ending it brings everything back and tells which tasks asked
  for you meanwhile. It stays on over a restart.
- **Sidebar as a rail.** Dragged as narrow as it goes, the sidebar
  becomes a rail of marks, one a task; it opens whole over the task while
  the pointer is on it, and "keep open" there makes it stay again.
- **Tokens.** By a task in the sidebar, a small dial of how full its
  agent's context is (yellow from three quarters, red from nine tenths;
  the numbers when the pointer rests on the task); in the title
  bar, what all the agents took in the last 24 hours. A session that
  has used more than a limit you set is told of once (Settings, Agent).
- **State.** Projects, tasks, open tabs, cursor lines and open folders
  survive a restart (`~/.local/share/ideae/state.json`).
- **Editor.** Tree-sitter highlighting, go to definition, usages and
  implementations (LSP when the language server is installed, else a
  search-based fallback; a missing server can be installed from the editor
  header into `~/.local/share/ideae/lsp`), file structure, blame of the cursor
  line, syntax-aware selection, autosave when you leave a file. Lines are
  deleted, duplicated, moved and joined on keys of their own, the next
  occurrence of a selection joins it, and words can end at the humps of
  camelCase and the underscores of snake_case (Settings, Editor). The
  gutter marks what differs from the last commit: green by the lines
  added, blue by those changed, red where some are gone. With a language
  server the editor also completes as you type (or on Ctrl+Space alone,
  when set so), underlines what the server
  finds wrong, offers its fixes (Alt+Enter), renames a symbol everywhere
  (Shift+F6) and reformats the file (Ctrl+Alt+L, or on save when set); names are colored by
  what the server knows them to be, Ctrl+P shows the parameters of the call
  being written, and Ctrl+N goes to a class, function or type anywhere in
  the project (without a server, to what the grammars find declared).
  Blocks fold from the gutter and with Ctrl+− / Ctrl+=, all of them with
  Shift. A right click on the line numbers annotates the file with git
  blame, as JetBrains IDEs do: the day and author by every line, the commit
  on hover, opened in History on a click. Lines are bookmarked (F11), go
  with their text as you edit, and are listed for the whole task
  (Shift+F11). Markdown has a Preview
  in its header.
- **Git.** Changes with stage/unstage/discard, commit, push, PR (`gh`), the
  branch against its base, history, safe pull: Pull is marked when what
  comes would conflict with your work, committed or only written, and is
  then not tried; conflict markers left in a file are named above the
  commit box. The working tree, or one file of it, compares with any
  branch, tag or commit as it is (▾ in Changes, a commit in History, the
  editor's menu). A click on the
  branch's name (Ctrl+Shift+`) lists the repository's branches: one opens
  as a task in a worktree of its own, compares with the working tree, or
  is deleted, here or on the remote, the merged ones all at once.
  History reads any branch, narrowed by words of the message, `@author`
  and `file:path`, with the branches and tags at each commit; a commit
  is cherry-picked into the task's branch, reverted, or made the start
  of a new task. Where a pull, a rebase, a pick or a revert would
  conflict, nothing is changed and the agent's prompt gets the request
  to do it and resolve the conflicts.
  Stash puts what is not committed away; the repository's
  stashes are listed under the changes, to look into, bring back or drop. Each hunk of a diff can be
  reverted on its own, and staged.
- **Terminals.** Built on libghostty-vt. Ctrl+click on paths, `file:line`,
  class and function names opens them in the editor.
- **Browser tabs.** Headless Chrome, streamed into a tab. ⌖ in its
  toolbar picks an element for the agent: a click puts its selector,
  markup, text, React components and a picture of it into the prompt.

## Services

Describe what a project runs while you work in `.ideae.toml` at its root,
and start it per task from Run ▾ in the branch bar:

```toml
[[service]]
name = "web"
command = "npm run dev -- --port {port}"
port = 3000
url = "http://localhost:{port}"
env = { PORT = "{port}" }
```

Each task has a port slot (0 for the main checkout, 1, 2… for worktrees)
and `{port}` is `port + 100 × slot`, so the same service runs in several
tasks at once, provided the program takes its port from the environment or
its arguments. A task's terminals, the agent's included, get
`IDEAE_PORT_<NAME>`. A service runs in its own terminal tab and survives a
window restart; a taken port is reported with the process holding it.

## macOS

Releases carry an Apple Silicon build: `ideae-<tag>-macos-arm64.app.zip`.
It is signed ad hoc, without an Apple Developer ID, so the first start
needs right click → Open (or `xattr -d com.apple.quarantine ideae.app`).
Shortcuts use Cmd where Linux uses Ctrl. Not there yet on macOS: clicks
on notifications, the Kotlin language server install.

A build of your own becomes an app the Dock can keep with
`scripts/macos-app.sh --link target/release/ideae ~/Applications/ideae.app`:
the bundle links to the binary, so the app offers a restart after each
`cargo build --release`.

## Updates

Release builds ask the GitHub API for this repository's latest release at
start and every six hours (Settings → Check for updates turns that off).
A newer one shows "↑ update" in the title bar: it downloads that release's
binary next to the running one, puts it in its place and restarts; on
Linux the agents keep running through the restart.

## Build

Requirements:

- Rust (stable, edition 2024)
- Zig 0.17.x (builds libghostty-vt)
- System libraries:
  `libxkbcommon-dev libxkbcommon-x11-dev libxcb-xkb-dev libx11-xcb-dev libwayland-dev libvulkan-dev libfontconfig-dev libfreetype-dev libasound2-dev`

```bash
cargo build --release
./target/release/ideae [REPO...]
```

Options: `--agent CMD` or `--no-agent`, `--theme auto|light|dark`.

A debug build is a separate app (`ideae-dev`): its own state, sockets and
launcher entry, so it can run next to a release build.

### Portable binary

To run on older glibc, build against glibc 2.31 with
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild):

```bash
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-Wl,--allow-shlib-undefined" \
  cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.31
```

### Releases

A push to `main` whose CI passes on every system becomes a release tagged
with its UTC time (`yyyymmddhhmmss`): a Linux binary (`chmod +x` it after
download), a Windows `.exe` and macOS builds (`.github/workflows/release.yml`). `ideae --version` names the release.

### Windows

Windows 10 1903 or newer, x64. The `.exe` is self-contained: it needs only
system DLLs. Terminals run PowerShell (`pwsh` when installed). Git must be
on PATH; Claude Code on Windows brings Git Bash, which also runs the agent
hooks. The headless browser tab uses Chrome, or Edge when there is no
Chrome.

State and language servers live in `%LOCALAPPDATA%\ideae`.

Not there yet on Windows: pinning to a launcher, the Kotlin language
server install.

To build on Windows: Rust (MSVC), Zig 0.17.x and the Windows SDK (gpui
compiles its shaders with `fxc.exe`; point `GPUI_FXC_PATH` at it when it is
not found), then `cargo build --release`.

A debug build cross-compiles on Linux with
`cargo build --target x86_64-pc-windows-gnu` (needs `mingw-w64`), enough to
check that it compiles and to run the tests under Wine. Such a build reads
its shaders from this checkout at run time, so it cannot be handed out, and
a release build needs a Windows host for `fxc.exe`.

## Keys

| Key | Action |
|---|---|
| Ctrl+Shift+T | New task |
| Ctrl+1 / Ctrl+2 / Ctrl+3 / Ctrl+\` | Agent / Changes / History / shell |
| Ctrl+Shift+N, Ctrl+Shift+F | Go to file, find in files |
| Ctrl+Shift+R | Replace in files |
| Ctrl+B / Ctrl+click | Go to definition; on a declaration, its usages |
| Ctrl+Alt+B | Implementations, then usages |
| Ctrl+F12 | File structure |
| F2 / Shift+F2 | Next / previous problem in the file |
| F11 / Shift+F11 | Bookmark the line / the task's bookmarks (Alt+PgDn / Alt+PgUp: next / previous in the file) |
| Alt+1 / Alt+2 / Alt+3 | Changes / Files / Structure panel |
| Ctrl+F4 / Ctrl+Shift+F4 | Close tab / reopen the last closed (more on a tab's right click) |
| Shift+Esc | Focus mode |

## License

[Mozilla Public License 2.0](LICENSE). Use it, ship it, build on it, in
closed products too; changes to its files stay open under the same license.

## Third-party code

`vendor/` holds patched copies of `gpui-component`, `gpui-base` and
`gpui-cn` (Apache-2.0, licenses kept in each directory). Changed places are
marked `ideae patch`.

`assets/file-icons` holds file type icons from
[IntelliJ IDEA Community Edition](https://github.com/JetBrains/intellij-community)
(Apache-2.0, unchanged; its NOTICE and the license kept beside them); the Go,
PHP and Rust ones were drawn for ideae in their style.

`vendor/tree-sitter-twig` is the generated parser of
[tree-sitter-twig](https://github.com/gbprod/tree-sitter-twig) (MIT, license
kept beside it). `queries/julia-highlights.scm` and
`queries/solidity-highlights.scm` come from tree-sitter-julia and
tree-sitter-solidity (MIT); the Solidity one has a pattern rewritten for this
tree-sitter version.
