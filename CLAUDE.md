# CLAUDE.md

Context for AI coding agents working on ideae. Human docs: [README.md](README.md) and [docs/](docs).

## Layout

One Rust binary on GPUI (`gpui-kit`), Linux first, Windows too.

```
src/main.rs        CLI, key bindings, title bar, window
src/workspace.rs   projects and tasks: load, switch, create/close/delete, state, hooks
src/workspace/variants.rs  several tries at one task as a group: create, compare, keep one
src/workspace/spaces.rs    spaces: projects sorted into sets, one in the window at a time; `Workspace::shows` is the one test of what is seen
src/workspace/side.rs      worktrees an agent made for itself: told apart, folded under their task, no agent of their own
src/workspace/automation.rs  a project's setup, teardown and check, the trust its .ideae.toml needs, hints, Project settings…
src/workspace/agents_ui.rs choosing agents: the picker, a project's agent, Continue with another agent
src/workspace/archive_ui.rs Archive (a task's everyday end), the archived list, Restore, an agent's worktree removed
src/workspace/present.rs   presenting: only the tasks chosen stay in the window, the rest out of the list and the notifications
src/sidebar.rs     left sidebar (inbox, projects, tasks, drag and drop)
src/task/          one task, a module: what is private to it is seen by all of its files (`use super::*`)
  mod.rs           the task itself: its state, the refresh loop, tabs and panes
  agent_tab.rs     what is sent to the agent, its restart, queued prompts
  activity.rs      the status of an agent that reports little, guessed from its terminal
  open.rs          opening a file and giving it its language server
  refresh.rs       what a refresh read, applied: diffs, files, the branch
  git_ui.rs        the commit box, the changed files, stashes, single changes, git actions
  branch_bar.rs    the branch line above the tabs
  branches.rs      the branches of the repository: open, compare, delete
  panels.rs        the tab bar and the side panels
  nav.rs           back and forward
  lookups.rs       outline, usages and places, installing a language server
  file.rs          an open file: editor, save and reload, blame, the language server's bridge
  changes.rs       the Changes tab: the diff, review notes, single changes, review by keys (J/K, N/P, V, S, R, C)
  change_popup.rs  a change of an open file, from its mark in the gutter: what was there, Revert, Stage
  term.rs          a terminal tab
  session.rs       session save/restore, language servers asleep
  services.rs      the Run panel
  auto.rs          the project's setup (a terminal) and check (background) in this task, CI failure to the agent
  pr.rs            pull request feedback
  annotate.rs      git blame by every line, in the gutter
  bookmarks.rs     bookmarked lines: toggle, next/previous, the task's list
  change_tree.rs   the changed files as a tree
  file_tree.rs     the Files panel: names colored by git status, ignored folders read when opened
  helpers.rs       patches parsed for the panels, outline, symbol search, definition providers
src/menu.rs        the macOS menu bar, kept short: tasks, git and the window, not the editor's commands
src/commands.rs    the commands on a task, its project, its variants, its git: names and what allows them, for the menu bar, the sidebar and the palette
src/file_icons.rs  file and folder icons (assets/file-icons, JetBrains' new UI), by name
src/state.rs       state.json; APP_ID (ideae vs ideae-dev)
src/archive.rs     an archived task's .tar.gz (task.json, sessions, unpushed bundle, patch): write, list, restore, clean up
src/agent.rs       agents' reports bridge: Claude Code hooks, Codex notify, OpenCode plugin (unix socket, `ideae --hook`)
src/agents/        agent profiles and adapters (docs/agents.md): how each kind starts, resumes, reports, is asked once
  mod.rs           profiles, resolution (task > project > default), the Adapter trait, Launch
  claude.rs codex.rs opencode.rs generic.rs   one adapter each; Codex and OpenCode follow their source, not tried live
src/crash.rs       what a panic leaves: crashes.log next to the state, told at the next start
src/stalls.rs      debug aid: tells when the UI thread was held (debug builds, `IDEAE_DEBUG`): stderr and stalls.log
src/git.rs         git CLI wrappers, worktrees
src/lsp.rs         language server client
src/lsp_install.rs language servers installed on request, ~/.local/share/ideae/lsp
src/trash.rs       files to the system's trash and back (Undo of Delete in the Files panel)
src/procs.rs       starting helper programs (no console on Windows), ending child sessions on quit
src/daemon/        terminals in a daemon (`ideae --daemon`, unix): they outlive the window (×, Restart); Exit/Ctrl+Q ends them;
                   a window of another build makes the daemon hand its ptys to a new one (SCM_RIGHTS), agents keep running
src/desktop.rs     launcher entry, dash pin, restart, `--install` / `--uninstall` (run by scripts/install.sh, install.ps1)
src/naming.rs      branch names from task titles
src/services.rs    `.ideae.toml` services (Run ▾): per-task port slots, IDEAE_PORT_<NAME>
src/project_config.rs  `.ideae.toml` [task] (setup, teardown, check, copy), your own over the file, suggestions, runs
vendor/            patched gpui-component, gpui-base, gpui-cn, gpui-fast-linux, gpui-fast-macos; changes marked `ideae patch`
assets/icon/       the app icon: ideae.svg, and the .icns and .ico made from it by scripts/icons.py
scripts/           Xvfb driver for UI checks, wayland.sh (the same in a headless GNOME Shell), check.sh (the tests to run before a push), icons.py, macos-app.sh (a .app around a binary)
```

## Build and test

- Zig 0.17.x must be on PATH (libghostty-vt). System -dev libraries: see docs/building.md.
- `cargo build`, `cargo test`. LSP tests are `#[ignore]` and need gopls / rust-analyzer.
- Variants have an end-to-end test on a real repository with real terminals, `#[ignore]` too: `cargo test -- --ignored two_variants` after changing them.
- So do agent profiles (two agents, session ids, guessed status, carrying a task on): `cargo test -- --ignored two_agents` after changing agents/ or how an agent is started.
- Saving a file and following it on disk have one too: `cargo test -- --ignored an_open_file` after changing how an open file is written, reloaded or refreshed.
- So has the start: `cargo test -- --ignored the_tasks_of_the_last_run` after changing how the tasks of the last run are restored.
- So has archiving and restoring: `cargo test -- --ignored an_archived_task` after changing either.
- And the daemon's terminals: `cargo test -- --ignored a_window_that_does_not_read` after changing how output goes from a session to a window (it floods one that reads nothing).
- And a project's setup, check and teardown: `cargo test -- --ignored a_task_is_set_up_checked_and_torn_down` after changing them.
- `scripts/check.sh` runs all of these and the Windows check: before every push, and again after merging main. A new end-to-end test goes into it; none of them waits on real time except for a timeout it is about, and together they stay under a minute.
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
- Debug aids: `IDEAE_LSP_DIR` (where servers get installed), `IDEAE_LSP_IDLE_SECS` (how long a task out of view keeps its language servers, default 900), `IDEAE_OPEN=path:line`, `IDEAE_URL=url`, `IDEAE_TAB=n`, `IDEAE_BEHIND` (the window opens without taking the keyboard or coming to the front: always set it on a machine someone is working at; a window that is covered is not redrawn, so what it shows may be its first frame), `IDEAE_KEYS="wait:8 ctrl-n g r e"` (presses those keys in the window, a pause where one is asked: for runs nobody can type in, as on macOS, where `cmd-` stands for `ctrl-`), `IDEAE_DEBUG`, `IDEAE_NO_SHELL_ENV` (started outside a terminal, the app takes no variables from the login shell), `IDEAE_NEW_TASK="title\nprompt"` with `IDEAE_VARIANTS=n` (starts a task in the first project, in n variants), `IDEAE_CHROME` (the browser tabs' Chrome, when none is installed where it is looked for), `IDEAE_WHOLE_TREES=1` (files watched as on macOS: whole trees, one stream, events filtered).
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
