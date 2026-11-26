# CLAUDE.md

Context for AI coding agents working on ideae. Human docs: [README.md](README.md) and [docs/](docs).

## Layout

One Rust binary on GPUI (`gpui-kit`), Linux first, Windows too.

```
src/main.rs        CLI, key bindings, title bar, window
src/workspace.rs   projects and tasks: load, switch, create/close/delete, state, hooks
src/workspace/variants.rs  several tries at one task as a group: create, compare, keep one
src/workspace/spaces.rs    spaces: projects sorted into sets, one in the window at a time; `Workspace::shows` is the one test of what is seen; found by id, a rename changes nothing
src/workspace/layered.rs   each project's layers resolved into an `Arc<Resolved>` when one changes, the team's read from the main checkout (not a task's worktree)
src/workspace/links_ui.rs  a task's links: its ticket opened, a link added, an integration offered for a tracker not set up, a request's changes told
src/workspace/tickets_ui.rs  tickets read with your token: a new task named from its ticket, its mark checked, still open at a merge
src/workspace/side.rs      worktrees an agent made for itself: told apart, folded under their task, no agent of their own
src/workspace/automation.rs  a project's setup, teardown and check, the trust its .ideae.toml needs, hints, Project settings…
src/workspace/agents_ui.rs choosing agents: the picker, a project's agent, Continue with another agent
src/workspace/crew.rs      agents a task's agent starts through the MCP server: a question beside the main thread, a task the user allows; waiting for what they say
src/workspace/questions.rs questions asked aside: an agent of their own beside the main thread's (`Who::Question`), their rows, closing and asking again
src/workspace/archive_ui.rs Archive (a task's everyday end), the archived list, Restore, an agent's worktree removed
src/workspace/present.rs   presenting: only the tasks chosen stay in the window, the rest out of the list and the notifications
src/sidebar.rs     left sidebar (inbox, projects, tasks, drag and drop)
src/task/          one task, a module: what is private to it is seen by all of its files (`use super::*`)
  mod.rs           the task itself: its state, the refresh loop, tabs and panes
  agent_tab.rs     the task's agents (its own and its questions', `Who`, `AgentState`): what is sent to them, their restart, queued prompts
  activity.rs      the status of an agent that reports little, guessed from its terminal
  open.rs          opening a file and giving it its language server
  http.rs          a `.http` file that sends its requests: Run and the environment over its text, the answer under it; the HTTP panel
  sql.rs           a `.sql` or `.redis` file that runs: its line of connection and database, Output, results, transactions, export, filter and pages, editing a cell
  sql_grid.rs      a result's typed rows for the table
  sql_schema.rs    its Schema tab and the editor's completions from it
  sql_dialog.rs    a connection's form, import from DataGrip
  preview.rs       a file drawn beside or instead of its text: Markdown, SVG, CSV/TSV; HTML in a Web tab
  refresh.rs       what a refresh read, applied: diffs, files, the branch
  git_ui.rs        the commit box, the changed files, stashes, single changes, git actions
  branch_bar.rs    the branch line above the tabs
  branches.rs      the branches of the repository: open, compare, delete
  panels.rs        the tab bar and the side panels
  panes.rs         where tabs are: two columns of an upper pane and, when a tab is put there, a lower one
  strip.rs         the side: the strip of the panels' marks, a panel in each half, one come over the task from its mark
  nav.rs           back and forward
  lookups.rs       outline, usages and places, installing a language server
  file.rs          an open file: editor, save and reload, blame, the language server's bridge
  changes.rs       the Changes tab: the diff, review notes, single changes, review by keys (J/K, N/P, V, S, R, C)
  change_popup.rs  a change of an open file, from its mark in the gutter: what was there, Revert, Stage
  term.rs          a terminal tab
  chat.rs          an ACP agent's tab: the conversation, permission answers, the agent's choices, the prompt
  tell.rs          what a task tells its agent through ideae's MCP server, and its language servers' answers to it
  session.rs       session save/restore, language servers asleep
  services.rs      the Run panel
  auto.rs          the project's setup (a terminal) and check (background) in this task, CI failure to the agent
  pr.rs            pull request feedback
  annotate.rs      git blame by every line, in the gutter
  bookmarks.rs     bookmarked lines: toggle, next/previous, the task's list
  change_tree.rs   the changed files as a tree
  file_tree.rs     the Files panel: names colored by git status, ignored folders read when opened
  unicode_tests.rs  everything that cuts text, run at every position of text in many scripts
  helpers.rs       patches parsed for the panels, symbol search, definition providers
  outline.rs       a file's structure: a query per language (`queries/outline/`, each with its case in `cases.txt`), regions, a `.sql` file's statements
src/copying.rs     text taken out of the app: selectable text, Copy items of menus, an error shown whole
src/menu.rs        the macOS menu bar, kept short: tasks, git and the window, not the editor's commands
src/commands.rs    the commands on a task, its project, its variants, its git: names and what allows them, for the menu bar, the sidebar and the palette
src/file_icons.rs  file and folder icons (assets/file-icons, JetBrains' new UI), by name
src/state.rs       state.json; APP_ID (ideae vs ideae-dev)
src/layers.rs      settings in layers: Global (Settings) → a space's → the team's (.ideae.toml [project], [integrations.<id>]) → a project's own; `resolve`, lists by id, a project's fields of before layers read and written back
src/layers_ui.rs   Settings over a layer (its switch): a space's or a project's page, where each value comes from, ↺; integrations listed and added (docs/integrations.md)
src/integrations/  trackers and code hosts, set up as layered entries: `Tracker` (links and keys, no I/O), `CodeHost` (requests, review, CI, merged branches; blocking)
  mod.rs           kinds, an entry made an integration, which code host a remote is, IntError
  tickets.rs       tickets in text: links of known trackers, keys owned by prefix, a task's links
  github.rs gitlab.rs   through gh and glab, signed in here; a stand-in glab in the tests
  http.rs read.rs  a tracker's API with your token (ureq, a stand-in server in the tests); a ticket read: Jira Cloud/DC, Linear, YouTrack
  secret_store.rs  tokens by host and account in the keyring, asked on first use; sealed here where there is none
src/archive.rs     an archived task's .tar.gz (task.json, sessions, unpushed bundle, patch): write, list, restore, clean up
src/acp/           agents that speak ACP, shown as a chat (docs/agents.md); their host is ideae-core's (core/src/acp), re-exported here
  link.rs          the window's end of a host (in the daemon, here or on a remote task's host, or a thread), paths keyed each way for a remote one
  spend.rs         what agents cost today, against a profile's daily limit
  fake.rs          a stand-in agent for the tests: the test binary run again
core/src/acp/      the host process (`ideae --acp-host`, `ideae-server --acp-host` on another machine): speaks ACP with the agent, keeps the
                   thread and its journal (carried over from a name of before), answers its file reads and writes; wire.rs the messages
                   with the window, thread.rs the conversation, checkpoint.rs the files before a prompt as a git tree, policy.rs who answers
                   its permission questions, terminal.rs commands it runs through the app (`terminal/*`)
src/mcp.rs         the window's side of ideae's MCP server (the server an agent runs is ideae-core's mcp.rs: `ideae --mcp`, `ideae-server --mcp`
                   on another machine, its socket reached there by ssh): tokens, the socket, the answers, a remote task's paths keyed each way
src/db/            databases, no UI: connections (connections.json), passwords in the system's keyring, bastions shared by connections, a tunnel by the system's ssh, a session on its own thread (docs/databases.md)
  backend.rs       what a session asks of a server; mysql_backend.rs postgres_backend.rs redis_backend.rs
  statement.rs value.rs schema.rs schema_cache.rs export.rs csv.rs format.rs history.rs import.rs scratch.rs tls.rs   statements and parameters, typed cells, completions, what a schema held, writers, CSV, layout, what ran, DataGrip's sources, files of no project, what an encrypted connection trusts
  fake.rs live.rs  a stand-in server and ssh for the check; real servers behind a real bastion, by hand
src/machine.rs     this machine's store, kept by the daemon for its windows (by the window where there is none): at the start
                   its projects and tasks are the store's (made of state.json the first time, again when an older build wrote
                   it since), the window's own from state.json; at each save what the window changed since it last took the
                   store in goes to it; state.json written whole as before, with the store's rev (IDEAE_NO_STORE=1: state.json
                   alone). workspace/elsewhere.rs: what another window changed, taken into the model field by field
src/secrets.rs     agents' keys out of the state: sealed for this machine in secrets.json, no keyring
src/agent.rs       agents' reports bridge: Claude Code hooks, Codex notify, OpenCode plugin (unix socket, `ideae --hook`)
src/agents/        agent profiles and adapters (docs/agents.md): how each kind starts, resumes, reports, is asked once
  mod.rs           profiles, resolution (task > project > default), the Adapter trait, Launch
  claude.rs codex.rs opencode.rs kimi.rs antigravity.rs glm.rs generic.rs   one adapter each (glm.rs: Claude Code on Z.ai, apart from claude.rs on purpose); Codex and OpenCode follow their source, Kimi and Antigravity their docs, not tried live
  own_config.rs    ideae's hooks put in an agent's own file when the user says so (Kimi's config.toml, Antigravity's hooks.json), and taken out
  registry.rs      the ACP registry: how its agents would run here, a download checked and unpacked
src/mac_notify.rs  macOS: notifications as the app's own (a click comes back), where the binary is in its bundle; else AppleScript's
src/image_diff.rs  a changed image in a diff: its two versions as pictures, read from git or the worktree
src/local_time.rs  the machine's calendar: weekdays and hours for Defer
src/crash.rs       what a panic leaves: crashes.log next to the state, told at the next start
core/src/awake.rs  the machine kept up while agents work: by the window for its own, by the daemon for every agent it runs
                   (a laptop's too, its own window closed), as its Machine layer says; core/src/battery.rs how full the
                   battery is and whether it is in use
src/stalls.rs      debug aid: tells when the UI thread was held (debug builds, `IDEAE_DEBUG`): stderr and stalls.log
src/git.rs         git CLI wrappers, worktrees
src/lsp.rs         language server client
src/lsp_columns.rs the editor's columns (characters) and those of a server that counts UTF-16, translated at the client's edge
src/lsp_install.rs language servers installed on request, ~/.local/share/ideae/lsp
src/lsp_share.rs   one server for the tasks of a project: a worktree is a folder of it, held by a Lease; a task's servers (TaskServers), found and started off the UI thread
src/trash.rs       files to the system's trash and back (Undo of Delete in the Files panel)
src/browser.rs     Web tabs: a headless Chrome per task (its profile kept, IDEAE_BROWSER_CDP), CDP screencast, pages it opens as tabs;
                   the user's own Chrome (`Owner::User`, one per space, `user_mark` of a project) with no port at all, its tabs in tasks
  pipe.rs          that Chrome over --remote-debugging-pipe: one connection, replies by id, events by session, the browser's to every channel
  seed.rs          a new task's Chrome signed in to the project's stands only: hosts served from this machine (noted by tabs), resolving here, named; cookies (SQLite) and local storage (LevelDB) filtered
src/procs.rs       starting helper programs (no console on Windows), ending child sessions on quit
src/daemon/        the window's side of daemons (`ideae --daemon`, unix): this machine's (client.rs) and other machines' over ssh (remote.rs: link, pings,
                   dialled again; install.rs: ideae-server put there, its hooks' files); Exit/Ctrl+Q ends them, the window's × leaves them
core/              ideae-core, no gpui: the daemon itself (daemon/: server, pipes, bridge for `--daemon --stdio`, calls: a program run, a file read or written
                   for a window elsewhere), hook reports (`--hook`), what an agent said, paths. `ideae-server` is built from it alone for servers
                   (scripts/server.sh: static Linux x86_64/aarch64, macOS); a window of another build makes the daemon hand its ptys to a new one (SCM_RIGHTS)
core/src/store/     a machine's store, kept by the daemon (host.rs; by the window where there is none, src/machine.rs): its
                   projects, tasks and their work, changed by ops that give
                   what must be one machine-wide (ids, ports' slots, variants, questions, crew, archive keys) and
                   refuse what breaks it (a path taken, a task another window holds); machine.json beside the state,
                   its lock; records.rs the records the window's state shares; migrate.rs from a state.json of before
src/hosts.rs       projects on another machine: their key `ssh:/<host>/<path there>` (`hub>mac` for one reached through another), never a
                   path here; ssh_for (IDEAE_SSH); what ssh said, told in words (explain_ssh)
src/askpass.rs     what ssh asks (a password, a passphrase, a host not met), asked in the window: ideae run as ssh's SSH_ASKPASS
src/hostfs.rs      a task's programs and files there or here by its path: git, reads and writes of a remote task go through it
src/workspace/hosts_ui.rs  a remote project: added (checked there), its host connected before its tasks open, a task's worktree made there
src/deep_link.rs   `ideae ticket KEY` and `ideae://ticket/KEY`: handed to the running window (a socket per state), or taken at start
src/desktop.rs     launcher entry, dash pin, restart, `--install` / `--uninstall` (run by scripts/install.sh, install.ps1)
src/predict/       edit prediction: llama-server and a model downloaded on consent, run for the app, ghost text at the cursor;
                   tests and UI checks use local files: IDEAE_PREDICT_SERVER, IDEAE_PREDICT_MODEL (IDEAE_DEBUG_PREDICT logs misses; IDEAE_PREDICT_GHOST="text" shows that as the suggestion, for screenshots)
src/naming.rs      branch names from task titles
src/http_client.rs `.http` files (JetBrains' open format): requests parsed, env files and variables, a request run with its scripts, sent with ureq; no gpui
src/http_script.rs their JavaScript (`< {% %}`, `> {% %}`) in Boa: client, request, response, crypto; no files, no network
src/http_port.rs   requests in and out: a curl command or a Postman collection as `.http` text, a file as a Postman collection
src/digest.rs      what of a long output (a check's, a CI log) an agent reads: the errors, each once, and the end
src/services.rs    `.ideae.toml` services (Run ▾): per-task port slots, IDEAE_PORT_<NAME>
src/project_config.rs  `.ideae.toml` [task] (setup, teardown, check, copy), your own over the file, suggestions, runs
vendor/            patched gpui-component, gpui-base, gpui-cn, gpui-fast-linux, gpui-fast-macos; changes marked `ideae patch`;
                   cn-pty: gpui-cn's pty, shell setup and process table without a window (gpui-cn re-exports it), for the daemon
assets/side-icons/ the marks of the side's strip (Lucide's drawings, served by `task::Assets` over the kit's)
assets/icon/       the app icon: ideae.svg, and the .icns and .ico made from it by scripts/icons.py
scripts/           Xvfb driver for UI checks, wayland.sh (the same in a headless GNOME Shell), check.sh (the tests to run before a push), remote-live.sh (projects on another machine against real sshd, in Docker), icons.py, macos-app.sh (a .app around a binary)
```

## Build and test

- Zig 0.17.x must be on PATH (libghostty-vt). System -dev libraries: see docs/building.md.
- `cargo build`, `cargo test`. LSP tests are `#[ignore]` and need gopls / rust-analyzer.
- Variants have an end-to-end test on a real repository with real terminals, `#[ignore]` too: `cargo test -- --ignored two_variants` after changing them.
- So do agent profiles (two agents, session ids, guessed status, carrying a task on): `cargo test -- --ignored two_agents` after changing agents/ or how an agent is started.
- And questions asked aside (their agent, its reports apart from the task's, the main thread's session kept): `cargo test -- --ignored a_question_beside` after changing questions or how a task's agents are told apart.
- And agents started by agents (a question, a task the user allows, waiting for their answers): `cargo test -- --ignored agents_start_agents` after changing crew.rs or the MCP server's tools.
- And a task made from a ticket's key alone, named after its agent's first turn (its title, not its branch): `cargo test -- --ignored a_ticket_task_is_named` after changing how a new task's title comes about.
- Saving a file and following it on disk have one too: `cargo test -- --ignored an_open_file` after changing how an open file is written, reloaded or refreshed.
- So has the start: `cargo test -- --ignored the_tasks_of_the_last_run` after changing how the tasks of the last run are restored.
- So has archiving and restoring: `cargo test -- --ignored an_archived_task` after changing either.
- And chat agents (ACP): `cargo build && cargo test -- --ignored an_acp_agent` after changing the host, the chat's link or the daemon's pipes (a stand-in agent, the real daemon, an upgrade of it).
- And the daemon's terminals: `cargo test -- --ignored a_window_that_does_not_read` after changing how output goes from a session to a window (it floods one that reads nothing); `a_window_that_quits_leaves` after changing what Exit or a prune ends (two windows on one daemon); `two_windows_share_the_store` after changing the store's calls or how a window's changes are counted (core/src/store/sync.rs).
- And projects on another machine: `cargo build && cargo test -- --ignored a_remote_daemon a_remote_project` after changing the bridge, the remote link, calls, hostfs or hosts_ui (a stand-in ssh to this machine, ideae-server from target/debug). Against real sshd: `scripts/remote-live.sh` (Docker: two throwaway containers, each reached directly and one through the other, a password asked once; `IDEAE_LIVE_PLATFORM=linux/amd64` for the other architecture) after changing the remote link, ssh's master connections, forwards or the install. Against a host of your own: `scripts/server.sh && IDEAE_LIVE_REMOTE="ssh -i key -p 2222 me@host" cargo test -- --ignored a_real_server`.
- And a project's setup, check and teardown: `cargo test -- --ignored a_task_is_set_up_checked_and_torn_down` after changing them.
- And Web tabs (needs Chrome): `cargo test -- --ignored a_page_tells_the_tab a_task_s_browser_wakes_when_called a_user_browser_has_no_port a_new_task_is_signed_in_to_its_stands_only a_page_opened_by_a_user_tab_goes_to_its_task devtools_without_a_port` after changing browser.rs: a page's dialog, select, pickers, cursor, errors, title, user agent, DevTools and the pages it opens; a task's Chrome started again by a call at its port; the user's Chrome over its pipe, two pages on one connection, no port listening; a new task's Chrome given a stand's logins, not a company's; a page a user's tab opened going to that tab's task; DevTools of a user's page carried over the pipe, kept from its other pages.
- And tasks sharing a language server: `cargo test -- --ignored two_tasks_share_one_rust_analyzer` after changing lsp_share.rs or how a task holds its servers; `an_agent_asks_the_language_server` after changing the MCP server's code questions (tell.rs). Both need rust-analyzer.
- And databases: `cargo test -- --ignored a_sql_file_runs` after changing `src/db` (a stand-in MySQL and ssh). Against real servers: `a_real_` with `IDEAE_LIVE_SSH_PORT`, `IDEAE_LIVE_SSH_KEY`, `IDEAE_LIVE_SSH_PROGRAM` (see `src/db/live.rs`).
- `scripts/check.sh` runs all of these and the Windows check: before every push, and again after merging main. A new end-to-end test goes into it; none of them waits on real time except for a timeout it is about, and together they stay under two minutes (an agent runs it before every push).
- UI can be tested without a display: `#[gpui_kit::test]` with `gpui_kit::test` (find, click, scroll by element id; mark elements with `.test_support()`), as `settings_ui.rs` does.
- A debug build is the separate app `ideae-dev`: own state, sockets, window class, launcher. Keep that split for anything new that has an identity on the system.

## Checking UI changes

Run the real app headless and look at screenshots. Other sessions use the
driver too: pick your own display with `IDEAE_DISPLAY=<n>` (default 77).

```bash
IDEAE_STATE=/tmp/x/state.json scripts/drive.sh /tmp/x/run.log 7 \
  "move:73,57 click wait:0.7 shot:/tmp/x/menu.png" -- --no-agent /path/to/test/repo
```

- Always set `IDEAE_STATE` to a scratch file; the real state belongs to the user's running app. It also gives the run its own daemon (socket named after the state file), so a test never attaches to or prunes the user's agents. `IDEAE_NO_DAEMON=1` keeps terminals in the window.
- Stop test processes by PID, never `pkill -f <pattern>`: patterns match other sessions' shells and the user's own app.
- The driver gives the app no D-Bus session. Never run the app with the user's session bus in tests: it can change their GNOME dash or send notifications.
- Use a throwaway git repo for task create/delete tests, never a real project.
- Debug aids: `IDEAE_LSP_DIR` (where servers get installed), `IDEAE_LSP_IDLE_SECS` (how long a task out of view keeps its language servers, default 900), `IDEAE_OPEN=path:line`, `IDEAE_LOOK=split|preview` (a Markdown, SVG or CSV file opens drawn so), `IDEAE_URL=url`, `IDEAE_TAB=n`, `IDEAE_BEHIND` (the window opens without taking the keyboard or coming to the front: always set it on a machine someone is working at; a window that is covered is not redrawn, so what it shows may be its first frame), `IDEAE_KEYS="wait:8 ctrl-n g r e"` (presses those keys in the window, a pause where one is asked, `space` for a space: for runs nobody can type in, as on macOS, where `cmd-` stands for `ctrl-`), `IDEAE_DEBUG`, `hooks.log` next to the state (debug builds: every agent hook with its session id and the processes that sent it), `IDEAE_NO_SHELL_ENV` (started outside a terminal, the app takes no variables from the login shell), `IDEAE_NEW_TASK="title\nprompt"` with `IDEAE_VARIANTS=n` (starts a task in the first project, in n variants; several parted by a line `---`, `project: title` for another project; their prompts are sent, not left in the agent's input), `IDEAE_CHROME` (the browser tabs' Chrome, when none is installed where it is looked for), `IDEAE_NOTIFY="title\nbody"` (sends a notification at the start and prints how the system took it and when it is clicked), `IDEAE_WHOLE_TREES=1` (files watched as on macOS: whole trees, one stream, events filtered).
- Wayland-only bugs (placement, maximize, resize, decorations) need a compositor: `scripts/wayland.sh` takes the same arguments and runs the app in a private headless GNOME Shell (own D-Bus session, runtime dir, home, settings; nothing on the screen). A small extension in `scripts/wayland/` drives the pointer and writes every frame change of the app's windows to `LOG.windows`; `windows` prints them, `@X,Y` is from the window's corner, `-- --run PROGRAM` runs another app (a GTK one shows what the compositor does on its own). It stops all it started when it ends.

  ```bash
  IDEAE_STATE=/tmp/x/state.json IDEAE_NO_DAEMON=1 scripts/wayland.sh /tmp/x/run.log 8 \
    "dclick:@700,15 wait:1.5 windows dclick:@700,15 wait:1.5 windows shot:/tmp/x/w.png" -- --no-agent /path/to/test/repo
  ```
- Before finishing, make sure no Xvfb, test app, language server or test agent you started is still running. The user's own `ideae` instance (DISPLAY=:0) is not yours.

## Rules

- Windows: start helper programs with `procs::command` (it hides the console
  window and finds `npm.cmd` and the like), and programs that must die with
  the app with `procs::spawn_tied`. Check the Windows side compiles with
  `cargo check --target x86_64-pc-windows-gnu --target-dir target/win`.
- macOS: `scripts/check-macos.sh` type-checks it from Linux (cargo-zigbuild).
  Keys go through `keys::bind`/`keys::label` (Cmd on macOS); Linux-only
  process code (`/proc`, `prctl`) needs a macOS branch (`libproc`) or a cfg.
- macOS permissions (Screen Recording, Accessibility) are given to a
  signature, and the linker's ad-hoc one changes with every build: run
  `scripts/macos-sign.sh` after `cargo build --release` on a Mac where the
  app has them, or they are gone at the next restart.
- The repository is public: no personal paths, hostnames, emails or private project names in code, tests or commits.
- Agents and terminals belong to tasks; whatever a task starts must end on Pause, Archive and quit.
- No comments unless the why is non-obvious.
- Text is Unicode, in any language. A byte offset is valid only if it came from that same text (a `find`, `char_indices`, a match). Never cut `str`, `String` or `Rope` at a computed one (`n`, `len - n`, `start - 64`): use `chars().take(n)`, `str::get`, or step to `is_char_boundary`. An editor column counts characters, not bytes. Whatever cuts or measures text gets a case in `task/unicode_tests.rs`. `scripts/string-slices.sh` (in `check.sh`) fails on a new raw slice of a string; one whose offset came from that same text is recorded with `--update`.

## Speed and memory

The app is meant to be fast and light. Before a change is ready to commit or
push, read it once more for what it costs:

- Nothing slow on the UI thread: no file or network I/O, no `git` or other
  process, no waiting on a channel, lock or `sleep`. That is everything
  outside `background_spawn` or a thread of its own: event handlers, entity
  updates, the code after an `.await` in `cx.spawn`. Read, run and parse in
  the background; bring the result to the UI thread to apply.
- `render` runs on every frame: no disk access (`exists`, `is_file`), no
  parsing, no collection rebuilt from all files or tasks. Compute when the
  data changes and keep the result.
- Per keystroke, nothing that grows with the file: no copy of the whole text
  (`value()`, `text().to_string()`), no parse. Compare in place, or wait for a
  pause in typing.
- What repeats (timers, watchers, polls) skips work when nothing changed, and
  does less for tasks out of view; every open task pays for it.
- Memory: no second copy of large texts (file contents, patches) kept around;
  share with `Rc`/`Arc`/`SharedString`. Caches have a bound and are dropped
  with what they belong to.
- A write moved off the UI thread must still be on disk when the task closes
  or the app quits (as `FileView::save_now` does).
