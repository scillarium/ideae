# ideae

An IDE in the agentic era: one window for running AI coding agents
(Claude Code by default), one task per git worktree.

Every task gets the agent in a terminal, more terminals, an editor, the diff
of its changes, git history and a browser tab, and keeps all of it while you
switch between tasks. Linux, macOS (Apple Silicon) and Windows; early, and
moving fast.

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
  in it. The sidebar shows which agents work, wait for you or are done, and
  tells you when one out of view wants you.
- **Archive.** A task that is done is archived with its agent's
  conversation and any work on no remote; restored weeks later, the agent
  goes on with the same conversation in a fresh worktree.
- **Variants.** One prompt to up to four agents at once, compared side by
  side, with a judge to say which to keep.
- **[Agents](docs/agents.md)**: Claude Code by default; Codex, OpenCode or any
  program as profiles (two accounts of one agent too), chosen per project,
  task or variant, and a task carried on by another agent.
- **Editor.** Tree-sitter highlighting, language servers (installed on
  request), go to definition and usages, rename, fixes, sticky scroll,
  replace in files, git blame in the gutter, bookmarks.
- **Git.** Changes by file and by hunk, commit, push, pull requests, a pull
  that warns before conflicting, branches, history, stashes.
- **Terminals** that outlive the window, and **browser tabs** whose elements
  can be picked for the agent.
- **[Project settings](docs/services.md)**: a setup for each new worktree, a
  check after each turn of its agent, a teardown, and services each task runs
  on ports of its own.
- Themes, spaces for sorting projects, a presentation mode.

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
| Ctrl+F12 | File structure |
| F2 / Shift+F2 | Next / previous problem |
| F11 / Shift+F11 | Bookmark the line / the task's bookmarks |
| Alt+1 / 2 / 3 | Changes / Files / Structure panel |
| Ctrl+F4 / Ctrl+Shift+F4 | Close tab / reopen the last closed |
| Shift+Esc | Focus mode |

On macOS, Cmd where this says Ctrl. Every key can be changed (Settings →
Keys), and Ctrl+Shift+A lists every command.

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
