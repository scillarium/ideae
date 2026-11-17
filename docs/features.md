# What ideae does

## Projects and tasks

Add a repository, start a task from a title. The title becomes a branch
(`DEV-123 Fix login` gives `DEV-123_fixLogin`, any other text a slug with a
random suffix). The task gets a worktree in `<repo>.worktrees/<branch>`,
branched from the remote's default branch, with the main checkout's ignored
`.env*` files copied in. A project's [settings](services.md) can install
what it needs there, check the agent's work after each turn and clean up
when the task goes.

Worktrees made outside the app show up as tasks too. Those an agent made for
itself (Claude Code's subagents in `.claude/worktrees/`, a try in a temporary
folder) are folded under the task they came from, with the commits and
changes they hold, and open with a shell, not an agent.

Projects, tasks, open tabs, cursor lines and open folders survive a restart
(`~/.local/share/ideae/state.json`). A task that is done is archived, not
deleted: see below.

## Pause, archive, restore

A task's menu ends with **Pause** and **Archive**. Pause stops its agent,
terminals and servers and runs the project's teardown; the task stays in
the list, and a click starts it again (the main checkout's Pause takes it
out of the list, as before).

Archive is how a task ends when it is done, merged or not: one click, since
nothing is lost. The task is paused, then one file is written for it,
`archive/<project>/<branch>-<time>.tar.gz` next to the state
(`~/.local/share/ideae/archive`; `IDEAE_ARCHIVE_DIR` puts it elsewhere):

- `task.json`: title, prompt, project, branch, base and last commit, the
  PR, review comments not sent, what the agent said last, the agent's
  sessions with their profiles, the branch's commits, dates, the version;
- the agent's session files (Claude Code's transcript and its subagents',
  Codex's rollout; OpenCode keeps its own in its database);
- work that exists nowhere else: commits on no remote and no other branch
  (a git bundle) and changes not committed, new files too (a binary patch).

The file is read back before anything goes; only then the worktree and the
local branch are removed. Remote branches are never touched. If the archive
cannot be written, nothing is removed and you are told. A project's menu
archives all of its tasks whose PR was merged at once.

**Delete remote branches of merged PRs…** (a project's menu, shown when
there are some) lists the branches on origin left after their PR was merged,
each with a box, all on: a branch goes when the latest merged PR of its name
has its head in this repository (not a fork) and its tip is the PR's head as
merged (squash and rebase merges) or is in the default branch and no newer
than the merge. Not listed: the default branch, a branch a task has checked
out, one with an open PR, one with commits since. One push deletes them, a
`git fetch --prune` follows; a branch origin refuses (protected) is said and
not offered again. The merged PRs stay on GitHub. Looked for in the
background with `gh` every half hour, and when a task's PR is merged.

**From an archived task…** (a project's + and menu; "Archived tasks…" in the
command palette for all projects in the window) lists them, newest first,
searched by title, prompt, branch, PR and date. Restore takes the goal up
again: a new worktree from origin, as a new task is made, on a branch named
from the title that you can change, or, when its PR is
still open, its branch fetched from origin. The commits on no remote are
cherry-picked onto it and the changes not committed applied, if you like;
what does not apply is kept in the worktree's git folder, and you and the
agent are told where. The agent's session files go back where it looks for
them for the new worktree, and it resumes that same session, first told
when the task was archived, what became of its PR, that main has moved on,
its commits then, and what came back. With Settings → Tasks → "Restore
with: A summary only", or when its agent can no longer resume it, a new
session starts, told the task, what was said last and the same news.

The archive stays after a restore: archived again, the task replaces it,
unless it holds work the restored task did not get back. Delete one for
good from the list. Settings → Tasks deletes archives after a while (3
months by default; never, if you like), with their conversation and any
work in them that was never pushed.

A worktree an agent made for itself has **Remove** instead: its branch goes
too when everything on it is on a remote or another branch, else it stays
and you are told.

## Agent status

Claude Code runs with hooks passed through `--settings`; nothing is written
to `~/.claude` or the repository. Other agents (Codex, OpenCode, any program)
report what they can, and the rest is guessed from their terminal: see
[agents](agents.md) for profiles, sessions and what each kind of agent can
do. The sidebar shows working / needs input / done, and a desktop notification fires when a task out of view wants you.
A question Claude Code asks (AskUserQuestion) or a plan it waits to have
approved counts as needing you. Notifications carry the agent's last answer
or its question; Settings → General → "Show what the agent said in
notifications" off, they say only Finished or Needs your answer.
The Changes and Files panels mark the file the agent is writing to and those
it wrote to since your last prompt.

By a task in the sidebar, a small dial shows how full its agent's context is
(yellow from three quarters, red from nine tenths; the numbers when the
pointer rests on the task). The title bar shows what all the agents took in
the last 24 hours. A session that has used more than a limit you set is told
of once (Settings → Agents).

## Variants

A hard task can be started as up to four variants: the same prompt to as
many agents, each in a worktree of its own from the same commit, with a line
for each one alone if you like (the smallest patch, a failing test first…).
Their agents run in Claude Code's auto mode (or the like for other agents),
so the same question is not asked in each. With more than one agent profile,
each variant can run a different one.

They show as one group. Its screen has, for each variant, what its agent
says it did, what it changed and what it cost, and which variant changed
which file. Any two can be compared file by file (A ↔ B); a message goes to
all of them at once. A command typed there (tests, a benchmark) runs in every
variant, one after another so that their times compare; with Auto it runs by
itself when all of them have finished. A judge, one more agent run that
can only read (the agent for quick questions), says which to keep and what to take from the others, and that
can be sent on to the agent of the one it picks.

For a site, "Start servers" runs every variant's [services](services.md) on
ports of its own, and the judge gets a picture of each. Keep one, or discard
all: what is removed stays under `refs/ideae/variants` for two weeks.
"Another variant…" on any task gives it a second try.

## Editor

- Tree-sitter highlighting; go to definition, usages and implementations
  (with a language server, else a search-based fallback). A missing server
  can be installed from the editor header.
- With a language server: completion as you type, problems underlined (F2
  steps through them), fixes (Alt+Enter), rename everywhere (Shift+F6),
  reformat (Ctrl+Alt+L, or on save), parameter info (Ctrl+P), names colored
  by what they are, Ctrl+N for a class or function anywhere in the project.
- The declarations you are inside of stay at the top as you scroll.
- File structure (Ctrl+F12), syntax-aware selection, folding, the next
  occurrence of a selection (Alt+J), lines deleted, duplicated, moved and
  joined on keys of their own, CamelHumps words (Settings → Editor).
- The gutter marks what differs from the last commit. A right click on the
  line numbers annotates the file with git blame. Bookmarks (F11) follow
  their text and are listed for the whole task (Shift+F11).
- A file opened to look at (a jump, a search result) takes the preview tab;
  past a limit the file tabs left longest close. Tabs can be pinned.
- Replace in files (Ctrl+Shift+R) shows every match with what it becomes.
- Autosave when you leave a file; Markdown has a Preview.

## Files panel

File and folder icons as JetBrains' new UI draws them. A name has the color
of what git says of it: blue changed, green added, red-brown not added, olive
ignored; a folder, of what is in it. An ignored folder (`target/`,
`node_modules/`) is read from disk when it is opened.

## Git

- Changes with stage, unstage and discard, single hunks reverted or staged;
  commit, push, a pull request (`gh`), the branch against its base.
- Safe pull: Pull is marked when what comes would conflict with your work,
  and is then not tried. Where a pull, rebase, pick or revert would conflict,
  nothing changes and the agent is asked to do it and resolve the conflicts.
- The working tree, or one file of it, compares with any branch, tag or
  commit.
- Branches (a click on the branch name, Ctrl+Shift+\`): open one as a task,
  compare it, delete it here or on the remote, the merged ones all at once.
- History of any branch, narrowed by words, `@author` and `file:path`, with
  the whole commit message; a commit is cherry-picked, reverted, or made the
  start of a new task.
- ⑂ Graph in History: every branch, local and on the remote, newest first,
  with lanes for where each left and joined, as JetBrains and Zed draw it.
  Branches can be hidden by pattern (`tenant/*`), and those of the remote
  with no commits of their own (only merges into them) left out; both are
  kept in the repository's git config.
- Stash, and the stashes listed to look into, bring back or drop.

## Terminals and browser tabs

Terminals are built on libghostty-vt; Ctrl+click on paths, `file:line`, class
and function names opens them in the editor. They live in a daemon, so the
agents keep running when the window restarts.

Browser tabs are headless Chrome streamed into a tab. ⌖ in the toolbar picks
an element for the agent: its selector, markup, text, React components and a
picture of it go into the prompt.

## Window

- Themes: Ayu, Catppuccin, Gruvbox, Solarized, Everforest and Flexoki, each
  light and dark (Theme… in the menu shows each as you go through the list).
- Spaces sort projects into sets (work, personal, a client); the window shows
  one at a time, and the agents of the others keep working unseen.
- Presentation mode (☰ → Enter presentation mode; on macOS, View →
  Presentation Mode…) leaves only the tasks you pick in the window, draws code
  larger, and hides what git ignores (keys, local settings) until asked for.
- Focus mode (Shift+Esc) puts one pane over the whole window.
- Dragged narrow, the sidebar becomes a rail of tasks that opens on hover.
