# What ideae does

## Projects and tasks

Add a repository, start a task from a title. The title becomes a branch
(`DEV-123 Fix login` gives `DEV-123_fixLogin`, any other text a slug with a
random suffix). The task gets a worktree in `<repo>.worktrees/<branch>`,
branched from the remote's default branch, with the main checkout's ignored
`.env*` files copied in. A project's [settings](services.md) can install
what it needs there, check the agent's work after each turn and clean up
when the task goes.

A prompt written with the title waits in the agent's input for you to finish
and send; Settings → Tasks → "Send the prompt at once" sends it once the agent
is up.

Worktrees made outside the app show up as tasks too. Those an agent made for
itself (Claude Code's subagents in `.claude/worktrees/`, a try in a temporary
folder, or one its Claude Code made anywhere with `git worktree add`, which
its hook tells) are folded under the task they came from, with the commits and
changes they hold, and open with a shell, not an agent.

The main thread is the repository's own checkout, meant to be on its default
branch. When it is on another one (left there weeks ago, or switched in a
terminal), a line above its tabs says so and offers the fix: **Switch to
main** when the remote's main already has all of the branch (merged,
squashed or rebased; the branch is deleted, main brought up to date), or
**Move … to a task** when it holds work of its own: a worktree for it, with
what was not committed yet, and the main checkout back on main. **Keep … here**
stops it for that branch.

Projects, tasks, open tabs, cursor lines and open folders survive a restart
(`~/.local/share/ideae/state.json`). A task that is done is archived, not
deleted: see below.

A Jira link pasted as the title (`…/browse/DEV-123`) becomes its key, and
the agent is told to read the issue (the link with it); a prompt the dialog
filled in and you left as it was is sent at once. After the agent's first
turn, the task gets a title from what it said (`DEV-123 Fix login`). The
branch keeps the key: ideae renames no branch under an agent at work, and
how a project names its branches is for its agent (its CLAUDE.md) or you. A
Linear link has the issue's title in it: the task and its branch are named
from it at once (`ENG-123_fixTheLoginForm`).

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
command palette for all projects in the window; Recent tasks, Ctrl+E, once
something is typed, after the tasks there are) lists them, newest first,
searched by title, prompt, branch, PR and date. Restore takes the goal up
again: a new worktree from origin, as a new task is made, on the branch it
had (renamed by its agent or you, as the project names them; from its title
only when it had none), which you can change, or, when its PR is
still open, its branch fetched from origin. The commits on no remote are
cherry-picked onto it and the changes not committed applied, if you like;
what does not apply is kept in the worktree's git folder, and you and the
agent are told where. The agent's session files go back where it looks for
them for the new worktree, and it resumes that same session, first told
when the task was archived, what became of its PR, that main has moved on,
its commits then, and what came back. With Settings → Tasks → "Restore
with: A summary only", or when its agent can no longer resume it, a new
session starts, told the task, what was said last and the same news.

**Defer…** (a task's menu) is Archive for a task you will come back to,
as the week of crash reports after a release: it goes as Archive does,
and is kept, not cleaned up, until a day chosen then (a week to six
months, or not sure yet). On that day it comes back by itself, as Restore
does with what it offers first (from origin, or its PR's branch when that
is still open; the work on no remote applied), paused and out of view, marked "back from deferred" in the sidebar: its
project's setup runs and its agent is told what changed when you open it.
Archived tasks lists the deferred ones first, with their day; Day… changes
it, Archive makes one an archive like the others, Defer… defers an
archived one. A project removed from the list takes its deferred tasks
back to archives like the others.

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
or its question; Settings → General → "What the agent said, in
notifications" off, they say only Finished or Needs your answer.
The Changes and Files panels mark the file the agent is writing to and those
it wrote to since your last prompt.

An agent that goes round in circles (Claude Code making the same tool call
four times running, or two calls in turn three times each, within one turn)
is not stopped, since that may be meant: its row says `↻ round in circles: …`
with what it repeats, a notification says so, and a bar above its tab stops
its turn (as Esc) or lets it go on, not told again that turn.
Reading a background command's output again and a command with `sleep` in it
are waiting, not repeating.

Settings → Agents → "Agents out of view" stops a chat agent that has had
nothing to do in a task you have not opened for a while (30 minutes), to free
its memory; "Terminal agents too" does the same for Claude Code in a terminal.
Its tab and row say asleep, and opening the task resumes its session. It is
left running while it works or asks, while something of its own would end
with it (a command it runs in the background, a timer it set with
CronCreate or ScheduleWakeup, text typed into it since it last reported, a
message waiting for it), and when its session is not known or it does not
report its status.

By a task in the sidebar, a small dial shows how full its agent's context is
(yellow from three quarters, red from nine tenths; the numbers when the
pointer rests on the task). The title bar can show what all the agents took
in the last 24 hours (Settings → General → Title bar). A session that has used more than a limit you set is told
of once (Settings → Agents).

## Questions aside

**Ask a question…** (Ctrl+I, a project's +, the File menu and the command
palette, for the project in hand) is for something to find out, not to
change: it goes to an agent of its own in the main thread, beside the main
thread's agent, which goes on with what it was doing. No branch, no worktree,
no title to think of: its first line names it, and the agent for quick
questions gives it a shorter name when there is more. It runs where the main
thread is, told to change nothing unless asked (Claude Code asks before each
change, Codex has a read-only sandbox, OpenCode asks), which it may still do
with your leave: notes for thinking it over, say.

A question can start from what an agent of the project knows: **Context** in
its dialog is empty by default, or names any of them. An agent of the same kind
goes on from a copy of that conversation, which itself stays as it was; another
kind is pointed to its file to read.

Each open question is a row under the main thread, with what its agent does
(a double click renames it), and a tab with its mark, after the agent's, that
can be dragged among the others. One that asks you something or has answered
while you were elsewhere is in Needs you under its task, stays visible under a
folded project; the sidebar narrowed to a rail lists them too. Alt+Down goes to it, and its notification
opens it. The tab's × only hides it, the agent going on; its row
brings it back. **Close question** above the tab (or the row's ×) ends its
agent; **Closed questions…** in the project's + lists them, and one picked
goes on with its conversation. Recent tasks (Ctrl+E) lists the open ones
with the tasks, marked `?`, and the closed ones once something is typed, as
archived tasks. **Make a task** (above its tab) carries a question on in a
task of its own: a worktree and a branch named after it, its conversation
resumed there, and its agent told the files it wrote in the main thread, to
move them over. Asked while the main thread is not running,
only the question's agent starts, and the main thread's first tab waits with a
button (or Ctrl+1), through a restart too. Pausing the main thread stops its
questions' agents as well, saying first what that would cost one; their rows and
tabs stay, marked asleep, and opening one starts only that agent, its
conversation going on, or says so if it was not found. Questions come back after a restart, their agents running on
in the background. A question's agent with nothing to do for a while, its
tab not in front, stops (Settings → Tasks, 15 minutes): the row says asleep,
its tab keeps its place, and the agent resumes the conversation when it is
opened. An open one with no message for days (3), yours or its agent's, goes
to the closed ones, unless its answer was not read. Once a question was asked, every agent there is resumed by its own
session: the main thread's session, if its id was never told, is found when
the first question is asked.

## Agents that start agents

With Settings → Agents → "Agents may start agents" on (it is off at first; it
reaches agents started after), a task's agent can, through ideae's MCP server
(Claude Code gets it with the hooks), start other agents of the same project
and wait for what they say; off, it is not shown those tools at all:

- `ask_agent`: a question beside the main thread, as Ctrl+I asks one, from
  nothing or from a copy of the asking agent's conversation. It can only read,
  so it starts at once; its row and tab are where yours are.
- `start_task`: a task of its own, with a title and a prompt. You are asked
  first (Start it / Decline); allowed, its worktree is made out of view, its
  agent sent the prompt, and it is listed under the task whose agent asked.
- `wait_for`: waits, a few minutes a call, for that agent to end its turn or
  ask you something, and gives what it said; `tell_agent` sends it more;
  `agent_changes` gives a task's commits and diff since it left the base, to
  review or take over its work.

Only a task's own agent starts others, four at work at once at most: not a
question's agent, not the agent of a task another agent started. A proposed
task waits in Needs you (Start / Decline) and is told of, nothing comes over
what you do; a started one is listed under the task whose agent asked, and
Pause or Archive of that task asks whether its tasks go too. Questions an
agent asks are its to read: they are not unread for you, their row says
whose, and only one that asks you something notifies. The ids are kept in
the state (the latest hundred).

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
- Language servers: gopls (Go), rust-analyzer, typescript-language-server
  (TypeScript, JavaScript), intelephense or phpactor (PHP), kotlin-lsp, basedpyright
  (Python), clangd (C, C++), jdtls (Java), csharp-ls (C#), LuaLS, zls (Zig),
  bash-language-server, the JSON, CSS and HTML servers of VS Code,
  yaml-language-server, Taplo (TOML), Marksman (Markdown),
  dockerfile-language-server, terraform-ls. ruby-lsp, sourcekit-lsp (Swift),
  Dart's, elixir-ls and Metals (Scala) are used when on PATH: they come with
  their language, or need it to install. A server on PATH wins over one ideae
  installed; Settings → Language servers turns each off.
- PHP has intelephense or phpactor, chosen in Settings → Language servers,
  with intelephense's licence key kept sealed there. intelephense without one
  is marked "no licence" in the editor header: a click opens that page.
- The tasks of a project share one language server per language: a task's
  worktree is one more folder of it, and what they have alike (the standard
  library, the dependencies, the system's headers) is read once, about a
  third of the memory for three tasks. rust-analyzer, clangd, gopls,
  basedpyright and the TypeScript server; a branch on another toolchain or
  TypeScript gets its own. Jumps, usages, rename and Go to symbol stay in
  the task's own worktree. A server that ends by itself starts again.
  Settings → Language servers → A language server for each task turns it off.
- A task you have not opened for a while (Settings → Language servers) stops
  its language servers. The task on screen stops them too when the app has
  been in the background as long and its agent is not working. They start
  again when you come back.
- Types and parameter names from the server are shown in the code where it
  has them (most of the above; intelephense only with a licence).
- With a language server: completion as you type, problems underlined (F2
  steps through them), fixes (Alt+Enter), rename everywhere (Shift+F6),
  reformat (Ctrl+Alt+L, or on save), parameter info (Ctrl+P), names colored
  by what they are, Ctrl+N for a class or function anywhere in the project.
- Edit prediction by a small model on this computer (downloaded on
  consent): gray text at the cursor that Tab takes, or the next edit nearby,
  struck through and green, that Tab makes; one further away is reached
  with a first Tab.
- The declarations you are inside of stay at the top as you scroll.
- File structure (Ctrl+F12) in every language with a grammar: declarations,
  Markdown's headings, keys and sections of config files, tags, CSS selectors,
  a `.sql` file's statements. Syntax-aware selection, CamelHumps words
  (Settings → Editor).
- Folding by syntax, and in any language by `region`/`endregion` comments
  (`//region`, `#region`, `-- region`, `<editor-fold>`), which the structure
  shows as sections, and by runs of line comments such as a license. A file no
  grammar reads folds by its indentation. What is folded stays so after a
  restart.
- What is there to read can be copied: commits' messages and hashes, errors
  (whole, with a Copy), notices and dialogs are selected with the mouse; a row
  that is clicked (a commit, a task, a question, a tab, a stash, a declaration)
  has it in its right-click menu.
- The gutter marks what differs from the last commit. A right click on the
  line numbers annotates the file with git blame. Bookmarks (F11) follow
  their text and are listed for the whole task (Shift+F11).
- Find in files lists matches in code before those in comments and prose
  (Markdown, text), within a file and among files.
- Replace in files (Ctrl+Shift+R) shows every match with what it becomes.
- Autosave when you leave a file.
- Markdown (with Mermaid diagrams), SVG, Mermaid files and CSV/TSV (a
  sortable table) are shown as text, drawn, or side by side (Ctrl+Shift+V),
  the drawing following what you type; diagrams and `currentColor` icons
  take the theme's colors. An HTML file opens in a Web tab.

## The side

A strip at the window's right edge has a mark for each panel: Changes,
Files, Structure, Run, Database (Alt+1 to 5). A click shows or hides a panel. The
side has two halves, a panel in each: Changes, Files and Database are of the upper
one, Structure and Run of the lower, and a mark dragged to the other end of
the strip moves its panel there. With nothing shown the strip is all there
is of the side, and the pointer resting on a mark brings its panel over the
task until it leaves.
The Files panel and Go to file leave out what the system writes into folders
for itself (.DS_Store, Thumbs.db, desktop.ini and the like); Changes still
lists one git does not ignore.

## Git

- Changes with stage, unstage and discard, single hunks reverted or staged;
  commit, push, a pull request (`gh`), the branch against its base.
- Review comments on lines of the diff go to the agent together (Send to
  agent) and stay, marked sent. When its turn ends each is looked for in its
  file: its code gone or changed, it says "changed: look again"; still there
  as it was, "still open": it moves to where the code is now and goes again
  with the next Send. Remove changed takes those you have looked at; × is
  done with one.
- A task follows its branch: renamed (`git branch -m`) or grown into a new
  one by its agent, the new name is the task's. Left on another branch that
  was there before, or detached, the task says so once its agent's turn is
  over (not during it), and offers to keep that branch as the task's or to
  go back on its own; nothing is switched or renamed by itself.
- Safe pull: Pull is marked when what comes would conflict with your work,
  and is then not tried. So is the base branch, as last fetched: when it
  changed the same lines as the task, its Rebase button turns yellow with the
  files, and the task's row says so (`⚠ conflicts with main`), before a PR would. Where a pull, rebase, pick or revert would conflict,
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
- A changed image shows in diffs as it was and as it is, an SVG above its
  text; any other binary file with its size before and after.

## Terminals and browser tabs

Terminals are built on libghostty-vt; Ctrl+click on paths, `file:line`, class
and function names opens them in the editor. They live in a daemon, so the
agents keep running when the window restarts.

Browser tabs are headless Chrome streamed into a tab, with as many pixels
as the window's screen has. The dashed square in the toolbar picks for the
agent: click elements (each with its selector, markup, text, React
components, the styles it is made of and a picture), drag over an area, or
take the whole page; say what to do with them and send them all in one prompt.

- Each task has a Chrome of its own: its profile is kept between runs, so
  logins stay, and a new task starts with the cookies of the project's last
  one; tasks share no cookies, so variants on other ports of localhost do not
  log each other out.
- The page's errors (exceptions, `console.error`, requests that failed) are
  counted in the toolbar: ⚠ lists them, and Send to agent puts them into its
  prompt.
- The task's own site is a page of this machine at a port of the task: one
  of its services', or one its processes listen on. A tab showing it loads
  it again when the agent's turn ends (Settings → Tasks → Web tabs; off for a site that
  reloads by itself and should keep its state); a page of elsewhere is left
  alone. The address bar offers the task's own addresses.
- A page of this machine that could not be loaded (a dev server still
  starting) is loaded again once something listens at its port.
- The screens' mark in the toolbar shows the page at a phone's, tablet's or desktop's size
  (made smaller to fit, touches from the mouse on a phone), turned or not,
  and asks it for light or dark colors, or the IDE's.
- Addresses of this machine Ctrl+clicked in a terminal open in a Web tab of
  the task; others in the system's browser.
- A task out of view as long as its language servers stay (Settings) closes
  its pages, unless its agent is at work: its Chrome ends, and each tab opens
  its page again when shown.
- `</>` or F12 opens Chrome's DevTools in a panel of the tab, below the page
  or right of it; the page gives it room, and the divider moves.
- The task's terminals, the agent's included, have the task's DevTools
  endpoint as `IDEAE_BROWSER_CDP` (a port that stays the task's), so
  Playwright MCP drives the pages you see. In a project's `.mcp.json` for
  Claude Code:

  ```json
  { "mcpServers": { "playwright": { "command": "npx",
      "args": ["@playwright/mcp@latest", "--cdp-endpoint", "${IDEAE_BROWSER_CDP}"] } } }
  ```

  Chrome runs while the task has a Web tab, and a minute after the last
  closes; a call at the task's port starts it again, so the agent finds it.
  Playwright drives the tab you see; a page it opens shows as a tab behind
  the one you look at.

## Window

- One menu bar on every system: File, Edit, View, Navigate, Task, Git, Window
  and Help. macOS has it at the top of the screen; on Linux and Windows it is
  behind ☰ in the title bar, or Alt pressed alone (Settings → General → Title bar can
  keep it shown).
- Themes: Ayu, Catppuccin, Gruvbox, Solarized, Everforest, Flexoki, GitHub, One,
  Rosé Pine, Tokyo Night and JetBrains' Islands, each light and dark. Settings has
  the light one and the dark one apart, and whether the window is light, dark or
  follows the system.
- Text (Settings → General): the code font (Fira Code or JetBrains Mono, which
  come with ideae, or one installed), ligatures on or off, the size
  of code in files, Changes and terminals (Ctrl+wheel over a view sets it too),
  and a terminal's letter spacing and line height.
- Spaces sort projects into sets (work, personal, a client); the window shows
  one at a time, and the agents of the others keep working unseen.
- Presentation mode (View → Presentation Mode…) leaves only the tasks you pick in the window, draws code
  larger, and hides what git ignores (keys, local settings) until asked for.
- Focus mode (Shift+Esc) puts one pane over the whole window.
- Dragged narrow, the sidebar becomes a rail that still says what each task
  is doing and what it holds, and opens whole when the pointer rests on it.
- Power save mode stops the language servers, the background fetch and the
  load readout; on a battery running low the app suggests it once.
- While an agent works, or a setup or a check runs, the system does not go to
  sleep for being left alone (the screen still dims and locks); Settings can
  keep it to the times the machine is plugged in, or turn it off.
