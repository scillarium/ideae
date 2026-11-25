# ideae

**An IDE for running AI coding agents side by side.** Each task gets its own
git worktree and its own agent, Claude Code by default. You see at a glance
which agent is working, which one is waiting for you and which one is done.
When you are ready, review its diff and commit, push or open a pull request
without leaving the window.

![Five tasks in three projects: one agent done, one working, one asking](docs/images/tasks.png)

Native (Rust and [GPUI](https://www.gpui.rs)), for Linux, macOS (Apple
Silicon and Intel) and Windows. Early, and moving fast.

## Why

- **Agents don't trip over each other.** A task title becomes a branch and a
  worktree. Five agents can work on one repository at once, each in its own
  checkout, with its own terminals and services on its own ports.
- **You know who needs you.** The sidebar puts the tasks that are waiting or
  done at the top, with the agent's question or the start of its answer.
  An agent that finishes while you look elsewhere sends a notification.
- **Nothing is lost when you switch.** Each task keeps its agent, terminals,
  open files, diffs and browser tabs. On Linux and macOS, terminals and
  agents keep running when the window closes, and through an update of the
  app.
- **Review is part of the loop.** The task's changes sit beside the agent:
  stage or revert single changes, comment, commit, push, open a PR. A pull
  that would conflict can go to the agent to resolve.
- **It stays light.** Language servers and browsers of tasks you are not
  looking at go to sleep; a task you are done with is archived with its
  conversation and comes back weeks later where it was.

![The task's changes, side by side](docs/images/changes.png)

## Install

Linux (x86_64) and macOS:

```bash
curl -fsSL https://github.com/scillarium/ideae/releases/latest/download/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/scillarium/ideae/releases/latest/download/install.ps1 | iex
```

It lands in `~/.local/bin`, `~/Applications` or `%LOCALAPPDATA%\Programs`,
listed with your applications so it can be pinned, and updates itself from
then on. `ideae --uninstall` removes it and keeps your settings. The builds are
also on the [releases page](https://github.com/scillarium/ideae/releases).
Claude Code and git should be installed.

## What it does

- **Tasks.** A title becomes a branch and a worktree, with the agent started
  in it. Star the ones you care about; pause one to free what it holds.
- **Variants.** One prompt to up to four agents at once, compared side by
  side, with a judge to say which to keep.
- **[Agents](docs/agents.md).** Claude Code by default; Codex, OpenCode or any
  program as profiles (two accounts of one agent too), chosen per project,
  task or variant. A task can be handed over to another agent.
- **[Chat agents](docs/agents.md#chat-agents-acp).** Any agent that speaks the
  Agent Client Protocol (Gemini, Kimi, Qwen, OpenCode, the adapters for Codex
  and Claude Code, the agents of the ACP registry) runs as a chat: tool calls
  with their diffs, questions answered with a key or by your rules, the
  files before each prompt to go back to, a daily cost limit, keys kept
  sealed. The agent can ask ideae for its task's open files, problems and
  check results.
- **Editor.** Tree-sitter highlighting, language servers installed on
  request, go to definition and usages, rename, fixes, inline hints, sticky
  scroll, replace in files, blame, bookmarks, and edit prediction from a local
  model if you want it.
- **Git.** Changes as a tree or a list, by file and by hunk, commit, amend,
  undo the last commit, push, pull requests and their checks, branches,
  history, stashes.
- **Web tabs.** A Chrome per task shown in a tab, with DevTools; its own site
  reloads after the agent's turn, and an element can be picked for the
  agent.
- **[Project settings](docs/services.md).** A setup for each new worktree, a
  check after each turn of the agent, a teardown, and services each task runs
  on ports of its own.
- Spaces to sort projects into (work, personal), a presentation mode, power
  save mode, themes.

![A file being edited: types from rust-analyzer in the code, a suggestion from the local model in gray, the Files panel](docs/images/editor.png)

![An agent waits for permission; its question is in the sidebar too](docs/images/asking.png)

More in [docs/features.md](docs/features.md); notes for each system in
[docs/platforms.md](docs/platforms.md).

## Keys

| Key | Action |
|---|---|
| Ctrl+Shift+T | New task |
| Ctrl+1 / 2 / 3 / \` | Agent / Changes / History / shell |
| Ctrl+Shift+N, Ctrl+Shift+F, Ctrl+Shift+R | Go to file, find in files, replace in files |
| Ctrl+B / Ctrl+click | Go to definition; on a declaration, its usages |
| Ctrl+Alt+B | Implementations, then usages |
| Ctrl+I | Ask a question aside |
| Ctrl+F12 | File structure |
| F2 / Shift+F2 | Next / previous problem |
| F11 / Shift+F11 | Bookmark the line / the task's bookmarks |
| Alt+1 / 2 / 3 / 4 / 5 | Changes / Files / Structure / Run / Database panel |
| F4 | In Changes, the diff or the list: open the file (at the line) |
| F1 / Ctrl+Alt+O | Quick documentation / optimize imports |
| Alt+F1 | Show the file in Files |
| Ctrl+Shift+Backspace | Last edit location, again for the one before |
| Ctrl+K / Ctrl+Shift+K | Commit / push |
| Ctrl+Alt+K | Every key (Settings → Keys); there, Learn the keys tells the keys of a command chosen with the mouse |
| Ctrl+F4 / Ctrl+Shift+F4 | Close tab / reopen the last closed |
| Shift+Esc | Focus mode |

On macOS, Cmd where this says Ctrl. Every key can be changed (Settings →
Keys, also Help → Keyboard Shortcuts…), and Ctrl+Shift+A lists every command.

## Building

`cargo build --release` with Rust and Zig 0.17; the details, and the system
libraries Linux needs, are in [docs/building.md](docs/building.md).

## License

[Mozilla Public License 2.0](LICENSE). Use it, ship it, build on it, in closed
products too; changes to its files stay open under the same license.

## Third-party code

`vendor/` holds patched copies of `gpui-component`, `gpui-base`, `gpui-cn` and
`gpui-fast-macos` (Apache-2.0, licenses kept in each directory). Changed places are marked
`ideae patch`.

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
