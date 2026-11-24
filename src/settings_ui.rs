//! Settings (Ctrl+,): a dialog over the state's settings, applied on Save.
//! Pages on the left with a filter over them, the page's settings on the
//! right.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    setting::{NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
};
use gpui_kit::{TestSupportExt as _, prelude::FluentBuilder as _, *};

use crate::{lsp, state, theme::Appearance, workspace::Workspace};

type Draft = Rc<RefCell<state::Settings>>;

fn switch(
    draft: &Draft,
    title: &'static str,
    hint: &'static str,
    get: fn(&state::Settings) -> bool,
    set: fn(&mut state::Settings, bool),
) -> SettingItem {
    let (read, write) = (draft.clone(), draft.clone());
    let field = SettingField::switch(move |_| get(&read.borrow()), move |value, cx| {
        set(&mut write.borrow_mut(), value);
        cx.refresh_windows();
    })
    .default_value(get(&state::Settings::default()));
    SettingItem::new(title, field).description(hint)
}

/// A language with more than one server: which one runs, or none; and
/// for PHP, intelephense's licence key.
fn language_choice(
    draft: &Draft,
    key: &'static str,
    language: &'static str,
    programs: &'static [&'static str],
    def: &lsp::ServerDef,
    there: &dyn Fn(&'static str) -> bool,
) -> Vec<SettingItem> {
    const OFF: &str = "off";
    let (read, write) = (draft.clone(), draft.clone());
    let options = programs
        .iter()
        .map(|p| (SharedString::from(*p), SharedString::from(if there(p) { p.to_string() } else { format!("{p} (not installed)") })))
        .chain([(SharedString::from(OFF), SharedString::from("Off"))])
        .collect();
    let field = SettingField::dropdown(
        options,
        move |_| {
            let d = read.borrow();
            let off = programs.iter().all(|p| d.lsp_off.iter().any(|o| o == p));
            SharedString::from(if off { OFF } else { lsp::chosen(key, &d) })
        },
        move |value, cx| {
            let mut d = write.borrow_mut();
            d.lsp_off.retain(|o| !programs.contains(&o.as_str()));
            if value.as_ref() == OFF {
                d.lsp_off.extend(programs.iter().map(|p| p.to_string()));
            } else {
                d.lsp_choice.insert(key.to_string(), value.to_string());
            }
            cx.refresh_windows();
        },
    )
    .default_value(SharedString::from(programs[0]));
    let mut items = vec![SettingItem::new(language, field).description(match key {
        "php" => format!(
            "{}. intelephense is quick, also on large projects; rename, go to implementation, inlay hints and code actions need its licence. \
             phpactor has them all for free, but indexes a large project slowly and needs PHP 8.1 or newer.",
            def.extensions()
        ),
        _ => def.extensions(),
    })];
    if key == "php" {
        let (read, write) = (draft.clone(), draft.clone());
        let field = SettingField::input(
            move |_| {
                let typed = read.borrow().lsp_licence.clone();
                let kept = || if crate::secrets::get(lsp::INTELEPHENSE_LICENCE).is_some() { crate::agents::MASK } else { "" };
                SharedString::from(typed.unwrap_or_else(|| kept().to_string()))
            },
            move |value, _| write.borrow_mut().lsp_licence = Some(value.to_string()),
        );
        items.push(SettingItem::new("intelephense licence key", field).description(
            "Kept sealed on this machine, not in the settings file. Without one, intelephense also reads licence.txt from its own folder.",
        ));
    }
    items
}

fn pages(
    draft: &Draft,
    look: &Rc<Cell<Appearance>>,
    dark_now: bool,
    recording: &Rc<Cell<Option<&'static str>>>,
    catcher: &Rc<RefCell<Option<Subscription>>>,
) -> Vec<SettingPage> {
    let default = state::Settings::default();
    let idle = {
        let (read, write) = (draft.clone(), draft.clone());
        SettingField::number_input(
            NumberFieldOptions { min: 1., max: 24. * 60., step: 1. },
            move |_| f64::from(read.borrow().lsp_idle_minutes),
            move |value, _| write.borrow_mut().lsp_idle_minutes = value as u32,
        )
        .default_value(f64::from(default.lsp_idle_minutes))
    };
    let fetch = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = [(1, "Every minute"), (2, "Every 2 minutes"), (5, "Every 5 minutes"), (10, "Every 10 minutes"), (15, "Every 15 minutes"), (30, "Every 30 minutes"), (0, "Never")]
            .into_iter()
            .map(|(m, label)| (SharedString::from(m.to_string()), SharedString::from(label)))
            .collect();
        SettingField::dropdown(options, move |_| read.borrow().fetch_minutes.to_string().into(), move |value, _| {
            write.borrow_mut().fetch_minutes = value.parse().unwrap_or(0)
        })
        .default_value(SharedString::from(default.fetch_minutes.to_string()))
    };
    let tab_limit = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = [(8, "8"), (10, "10"), (12, "12"), (15, "15"), (20, "20"), (30, "30"), (0, "No limit")]
            .into_iter()
            .map(|(n, label)| (SharedString::from(n.to_string()), SharedString::from(label)))
            .collect();
        SettingField::dropdown(options, move |_| read.borrow().tab_limit.to_string().into(), move |value, _| {
            write.borrow_mut().tab_limit = value.parse().unwrap_or(0)
        })
        .default_value(SharedString::from(default.tab_limit.to_string()))
    };
    let token_warn = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = [(0, "Never"), (500, "500k tokens"), (1000, "1M tokens"), (2000, "2M tokens"), (5000, "5M tokens"), (10000, "10M tokens")]
            .into_iter()
            .map(|(k, label)| (SharedString::from(k.to_string()), SharedString::from(label)))
            .collect();
        SettingField::dropdown(options, move |_| read.borrow().token_warn_k.to_string().into(), move |value, _| {
            write.borrow_mut().token_warn_k = value.parse().unwrap_or(0)
        })
        .default_value(SharedString::from(default.token_warn_k.to_string()))
    };
    let blame_place = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = vec![(SharedString::from("line"), SharedString::from("At the end of the line")), (SharedString::from("header"), SharedString::from("In the file's header"))];
        SettingField::dropdown(options, move |_| SharedString::from(if read.borrow().blame_at_line_end { "line" } else { "header" }), move |value, _| {
            write.borrow_mut().blame_at_line_end = value.as_ref() == "line"
        })
        .default_value(SharedString::from("line"))
    };
    let notices_at = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = [("bottom-right", "Bottom right"), ("bottom-left", "Bottom left"), ("top-right", "Top right"), ("top-left", "Top left")]
            .into_iter()
            .map(|(at, label)| (SharedString::from(at), SharedString::from(label)))
            .collect();
        SettingField::dropdown(options, move |_| read.borrow().notices_at.clone().into(), move |value, _| {
            write.borrow_mut().notices_at = value.to_string()
        })
        .default_value(SharedString::from(default.notices_at.clone()))
    };
    let restore_with = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = vec![(SharedString::from("full"), SharedString::from("The full conversation")), (SharedString::from("summary"), SharedString::from("A summary only"))];
        SettingField::dropdown(options, move |_| SharedString::from(if read.borrow().restore_summary { "summary" } else { "full" }), move |value, _| {
            write.borrow_mut().restore_summary = value.as_ref() == "summary"
        })
        .default_value(SharedString::from("full"))
    };
    let archive_keep = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = [(30, "A month"), (90, "3 months"), (180, "6 months"), (365, "A year"), (0, "Never")]
            .into_iter()
            .map(|(d, label)| (SharedString::from(d.to_string()), SharedString::from(label)))
            .collect();
        SettingField::dropdown(options, move |_| read.borrow().archive_keep_days.to_string().into(), move |value, _| {
            write.borrow_mut().archive_keep_days = value.parse().unwrap_or(0)
        })
        .default_value(SharedString::from(default.archive_keep_days.to_string()))
    };
    let present_zoom = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = [100, 110, 125, 150, 175, 200].into_iter().map(|p| (SharedString::from(p.to_string()), SharedString::from(format!("{p}%")))).collect();
        SettingField::dropdown(options, move |_| read.borrow().present_zoom.to_string().into(), move |value, _| {
            write.borrow_mut().present_zoom = value.parse().unwrap_or(100)
        })
        .default_value(SharedString::from(default.present_zoom.to_string()))
    };
    let rail_open = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = [(250, "A quarter of a second"), (500, "Half a second"), (750, "750 ms"), (1000, "A second"), (1500, "A second and a half"), (0, "Never")]
            .into_iter()
            .map(|(ms, label)| (SharedString::from(ms.to_string()), SharedString::from(label)))
            .collect();
        SettingField::dropdown(options, move |_| read.borrow().rail_open_ms.to_string().into(), move |value, _| {
            write.borrow_mut().rail_open_ms = value.parse().unwrap_or(750)
        })
        .default_value(SharedString::from(default.rail_open_ms.to_string()))
    };
    let side = |dark: bool| {
        let (read, write) = (draft.clone(), draft.clone());
        let options = crate::theme::FAMILIES
            .iter()
            .map(|family| (SharedString::from(family.name), SharedString::from(if dark { family.dark } else { family.light })))
            .collect();
        SettingField::dropdown(
            options,
            move |_| SharedString::from(if dark { read.borrow().dark_theme().to_string() } else { read.borrow().theme.clone() }),
            move |value, _| {
                let mut d = write.borrow_mut();
                if dark {
                    d.dark_theme = value.to_string();
                } else {
                    d.theme = value.to_string();
                }
            },
        )
        .default_value(SharedString::from(default.theme.clone()))
    };
    let (light_theme, dark_theme) = (side(false), side(true));
    let appearance = {
        let (read, write) = (look.clone(), look.clone());
        let options = Appearance::ALL.into_iter().map(|a| (SharedString::from(a.label()), SharedString::from(a.label()))).collect();
        SettingField::dropdown(options, move |_| read.get().label().into(), move |value, _| {
            if let Some(a) = Appearance::parse(&value) {
                write.set(a)
            }
        })
        .default_value(SharedString::from(Appearance::Auto.label()))
    };
    let now = if dark_now { "dark" } else { "light" };
    // Looked for on disk once while the dialog is open: this runs with every frame of it.
    let there = |program: &'static str| {
        INSTALLED.with(|known| {
            *known.borrow_mut().entry(program).or_insert_with(|| lsp::SERVERS.iter().any(|d| d.program == program && d.available()))
        })
    };
    let servers = lsp::SERVERS.iter().flat_map(|def| {
        let program = def.program;
        if let Some(&(key, language, programs)) = lsp::ALTERNATIVES.iter().find(|(_, _, programs)| programs.contains(&program)) {
            return if programs[0] == program { language_choice(draft, key, language, programs, def, &there) } else { vec![] };
        }
        let (read, write) = (draft.clone(), draft.clone());
        let field = SettingField::checkbox(move |_| !read.borrow().lsp_off.iter().any(|p| p == program), move |on, cx| {
            let mut d = write.borrow_mut();
            d.lsp_off.retain(|p| p != program);
            if !on {
                d.lsp_off.push(program.to_string());
            }
            cx.refresh_windows();
        })
        .default_value(true);
        let missing = if there(program) { "" } else { ", not installed" };
        vec![SettingItem::new(program, field).description(format!("{}{missing}", def.extensions()))]
    });
    vec![
        SettingPage::new("General").default_open(true).group(
            SettingGroup::new().title("Look")
                .item(SettingItem::new("Light theme", light_theme).description(
                    "Theme… in the menu previews each theme as you move through the list.",
                ))
                .item(SettingItem::new("Dark theme", dark_theme))
                .item(SettingItem::new("Light or dark", appearance).description(format!(
                    "Auto follows the system's setting, which is {now} now. Light and Dark stay fixed.",
                ))),
        ).group(
            SettingGroup::new().title("Notices")
                .item(switch(
                    draft,
                    "Notifications",
                    "A desktop notification when an agent finishes or needs your answer while ideae is not the window in front; in front, the sidebar's Needs you has it.",
                    |s| s.notifications,
                    |s, v| s.notifications = v,
                ))
                .item(switch(
                    draft,
                    "What the agent said, in notifications",
                    "The notification includes the agent's last answer or its question. When off, it says only Finished or Needs your answer, with the task's name. Never while presenting.",
                    |s| s.notify_said,
                    |s, v| s.notify_said = v,
                ))
                .item(switch(
                    draft,
                    "What the agent said, in Needs you",
                    "In Needs you, a finished task shows the start of its agent's last answer, and a waiting one shows its question. The task's tooltip shows it too. Never while presenting.",
                    |s| s.show_said,
                    |s, v| s.show_said = v,
                ))
                .item(SettingItem::new("Where notices show", notices_at).description(
                    "The corner of the window for the app's own notices, such as a finished task, a failed check or an update.",
                )),
        ).group(
            SettingGroup::new().title("Sidebar")
                .item(switch(
                    draft,
                    "Worktrees made by agents",
                    "Worktrees an agent creates for itself (Claude Code's subagents, a try in a temporary folder) are listed under the task they came from and open without an agent. When off, they are hidden.",
                    |s| s.show_side_worktrees,
                    |s, v| s.show_side_worktrees = v,
                ))
                .item(
                    SettingItem::new("The narrow sidebar opens after", rail_open)
                        .description("Dragged to its narrowest, the sidebar becomes a rail of tasks. Rest the pointer on the rail this long and the full sidebar opens over the task."),
                ),
        ).group(
            SettingGroup::new().title("Title bar")
                .item(switch(
                    draft,
                    "Load",
                    "CPU and memory, and how much of it the app and its tasks use. Hover it to see every process.",
                    |s| s.show_load,
                    |s, v| s.show_load = v,
                ))
                .item(switch(
                    draft,
                    "Tokens",
                    "Tokens used in the last 24 hours by the agents of the tasks in this window.",
                    |s| s.show_tokens,
                    |s, v| s.show_tokens = v,
                ))
                // macOS has its own at the top of the screen.
                .items((!cfg!(target_os = "macos")).then(|| switch(
                    draft,
                    "Menu bar always shown",
                    "File, Edit, View and the rest always show in the title bar. When off, they are under ☰, or appear when you press Alt.",
                    |s| s.menu_bar_shown,
                    |s, v| s.menu_bar_shown = v,
                ))),
        ).group(
            SettingGroup::new().title("Power")
                .item(switch(
                    draft,
                    "Keep the system awake while agents work",
                    "While an agent works, or a setup or a check runs, the system does not go to sleep for being left alone; the screen still dims and locks. A closed lid sleeps all the same.",
                    |s| s.keep_awake,
                    |s, v| s.keep_awake = v,
                ))
                .item(switch(
                    draft,
                    "Also on the battery",
                    "Off: only while plugged in.",
                    |s| s.keep_awake_on_battery,
                    |s, v| s.keep_awake_on_battery = v,
                ))
                .item(switch(
                    draft,
                    "Say when the battery runs low",
                    "Once, when the battery drops to 30% while unplugged: a notice suggesting Power Save Mode, with a button to turn it on.",
                    |s| s.battery_hint,
                    |s, v| s.battery_hint = v,
                )),
        ).group(
            SettingGroup::new().title("Updates").item(switch(
                draft,
                "Check for updates",
                "From time to time the app checks GitHub for a newer release; a build of a checkout, for new commits of its main branch, which one click pulls and builds.",
                |s| s.check_updates,
                |s, v| s.check_updates = v,
            )),
        ),
        agents_page(
            draft,
            switch(
                draft,
                "Agents' language servers",
                "Language servers agents run for themselves, such as Claude Code's LSP plugins: one rust-analyzer or the like per agent. They let an agent jump to definitions, but each takes a lot of memory. Applies to agents started after Save.",
                |s| s.agent_lsp,
                |s, v| s.agent_lsp = v,
            ),
            [
                switch(
                    draft,
                    "Context dial on tasks",
                    "A dial next to each task in the sidebar showing how full its agent's context is.",
                    |s| s.context_dial,
                    |s, v| s.context_dial = v,
                ),
                switch(
                    draft,
                    "Context dial on the agent's tab",
                    "A dial on the agent tab of the open task showing how full its context is.",
                    |s| s.context_on_tab,
                    |s, v| s.context_on_tab = v,
                ),
            ],
            token_warn,
        ),
        SettingPage::new("Tasks").default_open(true).group(
            SettingGroup::new().title("New tasks")
                .item(switch(
                    draft,
                    "Copy .env files",
                    "New worktrees get copies of the main checkout's ignored .env* files, unless the project's settings list other files to copy.",
                    |s| s.copy_env,
                    |s, v| s.copy_env = v,
                ))
                .item({
                    let (read, write) = (draft.clone(), draft.clone());
                    SettingItem::new(
                        "Worktrees in",
                        SettingField::input(
                            move |_| read.borrow().worktrees_dir.clone().into(),
                            move |value: SharedString, _| write.borrow_mut().worktrees_dir = value.trim().to_string(),
                        ),
                    )
                    .description("Empty: beside each repository, in <repository>.worktrees, where the tools of the folders above it still apply. A folder (~/worktrees): one of each project's name in it, as for a repository in a synced folder. New tasks only; a project's settings can name its own.")
                }),
        ).group(
            SettingGroup::new().title("Questions aside")
                .item(SettingItem::new("Stop an idle one's agent after", {
                    let (read, write) = (draft.clone(), draft.clone());
                    SettingField::number_input(NumberFieldOptions { min: 0., max: 24. * 60., step: 5. }, move |_| f64::from(read.borrow().question_sleep_minutes), move |value, _| {
                        write.borrow_mut().question_sleep_minutes = value as u32
                    })
                    .default_value(f64::from(state::Settings::default().question_sleep_minutes))
                }).description(
                    "Minutes; 0 means never. A question whose agent is neither working nor waiting for an answer, and whose tab is not in front, stops its agent to free its memory. The question stays open, and its agent resumes its conversation when you open it.",
                ))
                .item(SettingItem::new("Close an open one after", {
                    let (read, write) = (draft.clone(), draft.clone());
                    SettingField::number_input(NumberFieldOptions { min: 0., max: 365., step: 1. }, move |_| f64::from(read.borrow().question_close_days), move |value, _| {
                        write.borrow_mut().question_close_days = value as u32
                    })
                    .default_value(f64::from(state::Settings::default().question_close_days))
                }).description(
                    "Days; 0 means never. An open question with no message for this long, neither yours nor its agent's, goes to the closed ones, where it can be picked up again.",
                )),
        ).group(
            SettingGroup::new().title("Archived tasks")
                .item(SettingItem::new("Restore with", restore_with).description(
                    "A restored task's agent either continues its whole conversation and is told what changed since, or starts a new session with only the task, the last messages and what changed: a shorter context.",
                ))
                .item(SettingItem::new("Delete archived tasks after", archive_keep).description(
                    "Archives older than this are deleted, along with their agent's conversation and any commits or changes in them that were never pushed.",
                )),
        ).group(
            SettingGroup::new().title("Web tabs").item(switch(
                draft,
                "Reload the task's site after the agent's turn",
                "A Web tab showing the task's own site (a port of its services, or one its processes listen on) reloads when the agent finishes a turn. Other pages are left alone. Turn it off if the site reloads by itself and should keep its state.",
                |s| s.web_reload_after_turn,
                |s, v| s.web_reload_after_turn = v,
            )),
        ),
        SettingPage::new("Editor").default_open(true).group(
            SettingGroup::new().title("Typing")
                .item(switch(
                    draft,
                    "CamelHumps words",
                    "Moving, selecting and deleting by word stop at camelCase humps and snake_case underscores.",
                    |s| s.hump_words,
                    |s, v| s.hump_words = v,
                ))
                .item(switch(
                    draft,
                    "Completions as you type",
                    "The language server's suggestions appear as you type. When off, ask for them with Ctrl+Space.",
                    |s| s.complete_as_typed,
                    |s, v| s.complete_as_typed = v,
                ))
                .item(switch(
                    draft,
                    "Reformat on save",
                    "Ctrl+S asks the language server to reformat the file before saving. The save made when you leave a file does not reformat.",
                    |s| s.format_on_save,
                    |s, v| s.format_on_save = v,
                )),
        ).group(crate::predict::settings_group(draft)).group(
            SettingGroup::new().title("Tabs")
                .item(switch(
                    draft,
                    "Preview tab",
                    "Files opened just to look at (a jump to a definition or usage, a search result, a click in Files) share one tab in italics, which the next such file replaces. Edit the file or double-click the tab to keep it.",
                    |s| s.preview_tabs,
                    |s, v| s.preview_tabs = v,
                ))
                .item(SettingItem::new("File tabs open at most", tab_limit).description(
                    "Beyond this many file tabs, the ones unused longest close. Files with unsaved changes stay open.",
                )),
        ).group(
            SettingGroup::new().title("Shown in the code")
                .item(switch(
                    draft,
                    "Colors from the language server",
                    "Names are colored by what the server knows they are (a type, a parameter, a method), on top of the syntax colors.",
                    |s| s.semantic_colors,
                    |s, v| s.semantic_colors = v,
                ))
                .item(switch(
                    draft,
                    "Hints from the language server",
                    "Inferred types after names, and parameter names before arguments, shown inline in the code.",
                    |s| s.inlay_hints,
                    |s, v| s.inlay_hints = v,
                ))
                .item(SettingItem::new("Who changed the line", blame_place).description(
                    "Who last changed the line under the cursor, when and why, at the end of that line or in the file's header. Click it to open the commit.",
                ))
                .item(switch(
                    draft,
                    "Gutter marks against the base branch",
                    "The marks beside the line numbers show everything the task changed since it branched off, not only since the last commit.",
                    |s| s.gutter_since_base,
                    |s, v| s.gutter_since_base = v,
                )),
        ),
        SettingPage::new("Git").default_open(true).group(
            SettingGroup::new().title("Diffs")
                .item(switch(
                    draft,
                    "Unified diff",
                    "The Changes tab opens with old and new lines one above the other instead of side by side.",
                    |s| s.diff_unified,
                    |s, v| s.diff_unified = v,
                ))
                .item(switch(
                    draft,
                    "Ignore whitespace",
                    "Diffs hide changes to spaces, tabs, line endings and blank lines, like GitHub's \"Hide whitespace\". Staging or reverting a single change then applies only what you see.",
                    |s| s.diff_ignore_whitespace,
                    |s, v| s.diff_ignore_whitespace = v,
                )),
        ).group(
            SettingGroup::new().title("Conflicts").item(switch(
                draft,
                "Conflicts go to the agent",
                "A pull, rebase, cherry-pick or revert that would conflict is not started; the agent is asked to do it and resolve the conflicts instead. When off, you are only told.",
                |s| s.conflicts_to_agent,
                |s, v| s.conflicts_to_agent = v,
            )),
        ).group(
            SettingGroup::new().title("In the background")
                .item(SettingItem::new("Fetch the base branch", fetch).description(
                    "Keeps origin's main branch up to date in the background, so each task can show how far behind it is.",
                ))
                .item(switch(
                    draft,
                    "Pull request status",
                    "Asks gh about each task's pull request and shows its status on the task's row.",
                    |s| s.pr_status,
                    |s, v| s.pr_status = v,
                )),
        ),
        SettingPage::new("Language servers").default_open(true).groups([
            SettingGroup::new().title("Servers").items(servers),
            SettingGroup::new().title("Sharing").item(switch(
                draft,
                "A language server for each task",
                "Off, the tasks of a project share one server per language: a task's worktree is one more folder of it, and what they have alike (the standard library, the dependencies) is read once, a third of the memory for three tasks. rust-analyzer, clangd, gopls, basedpyright and the TypeScript server; the others run one per task anyway. Turn it on if a shared server gets something wrong.",
                |s| s.lsp_separate,
                |s, v| s.lsp_separate = v,
            )),
            SettingGroup::new().title("Sleep").item(
                SettingItem::new("Sleep after (minutes)", idle)
                    .description("A task you have not opened for this long stops its language servers. The task on screen stops them too when the app has been in the background this long and its agent is not working. They start again when you come back."),
            ),
        ]),
        SettingPage::new("Keys").default_open(true)
            .description("Click a shortcut and press the new keys: Esc leaves them, Backspace removes them.")
            .default_open(true)
            .groups(key_groups(draft, recording, catcher)),
        SettingPage::new("Spaces").default_open(true)
            .description("Projects sorted into sets (work, personal, a client); the window shows one set at a time. Switch with the mark at the right of the title bar.")
            .group(SettingGroup::new().item(
                SettingItem::new(
                    "The spaces",
                    SettingField::render(|_, _, _| {
                        Button::new("edit-spaces").small().outline().label("Edit spaces…").on_click(|_, window, cx| {
                            // The list is the workspace's and kept as it is typed: it has a window of its own.
                            if let Some(edit) = EDIT_SPACES.with(|edit| edit.borrow().clone()) {
                                edit(window, cx);
                            }
                        })
                    }),
                )
                .description("Add, rename, mark or remove spaces. To put a project in a space, use the project's menu in the sidebar."),
            )),
        SettingPage::new("Presentation").default_open(true)
            .description(if cfg!(target_os = "macos") {
                "For showing your screen to others: View → Presentation Mode… keeps only the tasks you tick in the window."
            } else {
                "For showing your screen to others: ☰ → Enter presentation mode keeps only the tasks you tick in the window."
            })
            .group(
                SettingGroup::new().title("Shown larger").item(
                    SettingItem::new("Text size", present_zoom)
                        .description("Files, diffs and terminals are this much larger while presenting; your own sizes return when it ends."),
                ),
            )
            .group(
                SettingGroup::new().title("Kept out of sight")
                    .item(switch(
                        draft,
                        "Sidebar as a rail",
                        "The remaining tasks show as a narrow rail with the start of their names; rest the pointer on it to see the whole list.",
                        |s| s.present_rail,
                        |s, v| s.present_rail = v,
                    ))
                    .item(switch(
                        draft,
                        "Hide files git ignores",
                        "Files git ignores (keys, local settings) are left out of the Files tree, and their contents are hidden behind a notice until you ask to see them.",
                        |s| s.present_veil,
                        |s, v| s.present_veil = v,
                    ))
                    .item(switch(
                        draft,
                        "Hide load and tokens",
                        "The title bar's load and token readouts are hidden while presenting.",
                        |s| s.present_hide_readouts,
                        |s, v| s.present_hide_readouts = v,
                    )),
            ),
        databases_page(),
    ]
}

/// The connections `.sql` files run against. Kept apart from the settings
/// (`db::config`), so a change here does not wait for Save.
fn databases_page() -> SettingPage {
    let list = SettingItem::render(|_, _, cx| {
        let muted = cx.theme().muted_foreground;
        let connections = crate::db::config::list();
        let empty = connections.is_empty();
        let rows = connections.into_iter().enumerate().map(|(ix, c)| {
            let through = c.ssh.as_ref().map(|hop| format!(" · through {}", hop.host)).unwrap_or_default();
            let about = format!("{} · {}@{}:{}{through}", c.driver.name(), c.user, c.host, c.port);
            let (edit, remove) = (c.id.clone(), c.id.clone());
            h_flex()
                .id(SharedString::from(format!("db-connection-{ix}")))
                .test_support()
                .gap_2()
                .py_1()
                .items_center()
                .child(gpui_kit::component::v_flex().flex_1().min_w_0().child(div().text_sm().child(c.name)).child(div().text_xs().text_color(muted).truncate().child(about)))
                .child(Button::new(SharedString::from(format!("db-edit-{ix}"))).xsmall().ghost().label("Edit…").on_click(move |_, window, cx| {
                    crate::task::connection_dialog(Some(edit.clone()), Some(Rc::new(|_, window, _| window.refresh())), window, cx);
                }))
                .child(Button::new(SharedString::from(format!("db-remove-{ix}"))).xsmall().ghost().label("Remove").on_click(move |_, window, cx| {
                    crate::db::config::remove(&remove);
                    crate::db::password::delete(&remove, cx).detach();
                    crate::db::history::clear(Some(remove.clone()));
                    crate::db::schema_cache::forget(&remove);
                    window.refresh();
                }))
        });
        gpui_kit::component::v_flex()
            .gap_1()
            .children(rows)
            .when(empty, |s| s.child(div().text_xs().text_color(muted).child("No connection yet. A .sql file runs against one, chosen in the line over its text.")))
            .child(
                h_flex()
                    .pt_1()
                    .gap_2()
                    .child(Button::new("db-add").small().outline().label("Add connection…").on_click(|_, window, cx| {
                        crate::task::connection_dialog(None, Some(Rc::new(|_, window, _| window.refresh())), window, cx);
                    }))
                    .child(Button::new("db-import").small().outline().label("Import from DataGrip…").on_click(|_, window, cx| crate::task::import_dialog(window, cx))),
            )
    });
    let bastions = SettingItem::render(|_, _, cx| {
        let muted = cx.theme().muted_foreground;
        let list = crate::db::config::bastions();
        let empty = list.is_empty();
        let rows = list.into_iter().enumerate().map(|(ix, b)| {
            let uses = crate::db::config::bastion_uses(&b.id);
            let about = format!("{}@{}:{} · {}", b.hop.user, b.hop.host, b.hop.port, if uses == 1 { "1 connection".to_string() } else { format!("{uses} connections") });
            let (edit, remove) = (b.id.clone(), b.id.clone());
            h_flex()
                .id(SharedString::from(format!("db-bastion-{ix}")))
                .gap_2()
                .py_1()
                .items_center()
                .child(gpui_kit::component::v_flex().flex_1().min_w_0().child(div().text_sm().child(b.name)).child(div().text_xs().text_color(muted).truncate().child(about)))
                .child(Button::new(SharedString::from(format!("db-bastion-edit-{ix}"))).xsmall().ghost().label("Edit…").on_click(move |_, window, cx| {
                    crate::task::bastion_dialog(Some(edit.clone()), Some(Rc::new(|_, window, _| window.refresh())), window, cx);
                }))
                .child(Button::new(SharedString::from(format!("db-bastion-remove-{ix}"))).xsmall().ghost().label("Remove").disabled(uses > 0).tooltip("Only when no connection goes through it").on_click(move |_, window, _| {
                    crate::db::config::remove_bastion(&remove);
                    window.refresh();
                }))
        });
        gpui_kit::component::v_flex()
            .gap_1()
            .children(rows)
            .when(empty, |s| s.child(div().text_xs().text_color(muted).child("None needed for a database you can reach directly.")))
            .child(h_flex().pt_1().child(Button::new("db-bastion-add").small().outline().label("Add bastion…").on_click(|_, window, cx| {
                crate::task::bastion_dialog(None, Some(Rc::new(|_, window, _| window.refresh())), window, cx);
            })))
    });
    SettingPage::new("Databases").default_open(true).groups([SettingGroup::new().title("Connections").item(list), SettingGroup::new().title("SSH bastions").item(bastions)])
}

/// Where the shortcuts are among the pages.
pub const KEYS_PAGE: usize = 6;
pub const AGENTS_PAGE: usize = 1;
pub const LSP_PAGE: usize = 5;

/// The Agents page: the profiles, the default, the one for quick questions.
fn agents_page(draft: &Draft, lsp_switch: SettingItem, dials: [SettingItem; 2], token_warn: SettingField<SharedString>) -> SettingPage {
    let list = {
        let draft = draft.clone();
        SettingItem::render(move |_, _, cx| {
            let muted = cx.theme().muted_foreground;
            let d = draft.borrow();
            let rows = d.agents.iter().enumerate().map(|(ix, p)| {
                let default = p.id == d.default_agent;
                let how = if p.acp { "chat" } else { "terminal" };
                let mut about = format!("{} · {how} · {}", p.kind.label(), if p.command.trim().is_empty() { p.kind.program() } else { p.command.trim() });
                if !p.env.is_empty() || !p.secrets.is_empty() {
                    about.push_str(&format!(" · {}", p.env.keys().cloned().chain(p.secrets.iter().map(|k| format!("{k} (sealed)"))).collect::<Vec<_>>().join(", ")));
                }
                let (edit, remove) = (draft.clone(), draft.clone());
                h_flex()
                    .id(SharedString::from(format!("agent-profile-{ix}")))
                    .test_support()
                    .gap_2()
                    .py_1()
                    .items_center()
                    .child(div().w(px(28.)).flex().justify_center().child(crate::workspace::spaces::agent_chip(&p.letter(), p.color, cx.theme().border, muted)))
                    .child(
                        gpui_kit::component::v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().child(if default { format!("{} (default)", p.name) } else { p.name.clone() }))
                            .child(div().text_xs().text_color(muted).truncate().child(about))
                            .when(!p.acp && !p.adapter().tried_live(), |s| s.child(div().text_xs().text_color(muted).child("Not tried live: its adapter follows the agent's source code."))),
                    )
                    .child(Button::new(SharedString::from(format!("agent-edit-{ix}"))).xsmall().ghost().label("Edit…").on_click(move |_, window, cx| profile_dialog(edit.clone(), Some(ix), None, window, cx)))
                    .child(Button::new(SharedString::from(format!("agent-remove-{ix}"))).xsmall().ghost().label("Remove").on_click(move |_, window, _| {
                        let mut d = remove.borrow_mut();
                        if ix < d.agents.len() {
                            d.agents.remove(ix);
                            d.normalize();
                        }
                        window.refresh();
                    }))
            });
            let (add, find, registry_draft) = (draft.clone(), draft.clone(), draft.clone());
            gpui_kit::component::v_flex()
                .gap_1()
                .children(rows)
                .when(d.agents.is_empty(), |s| s.child(div().text_xs().text_color(muted).child("No agent: tasks open with a plain shell.")))
                .child(
                    h_flex()
                        .pt_1()
                        .gap_2()
                        .child(Button::new("agent-add").small().outline().label("Add agent…").on_click(move |_, window, cx| profile_dialog(add.clone(), None, None, window, cx)))
                        .child({
                            let browse = registry_draft.clone();
                            Button::new("agent-registry")
                                .small()
                                .outline()
                                .label("Browse ACP agents…")
                                .tooltip("Agents that speak the Agent Client Protocol, from its registry: run as a chat")
                                .on_click(move |_, window, cx| registry_dialog(browse.clone(), window, cx))
                        })
                        .child(Button::new("agent-find").small().outline().label("Find installed agents").on_click(move |_, window, cx| crate::workspace::welcome::find_into(find.clone(), window, cx))),
                )
                .when_some(crate::workspace::welcome::find_said(), |s, said| s.child(div().text_xs().text_color(muted).child(said)))
        })
    };
    let choices = |with_default: bool| -> Vec<(SharedString, SharedString)> {
        let d = draft.borrow();
        let mut options: Vec<(SharedString, SharedString)> = Vec::new();
        if with_default {
            options.push((SharedString::default(), SharedString::from("The default agent")));
        }
        options.extend(d.agents.iter().map(|p| (SharedString::from(p.id.clone()), SharedString::from(p.name.clone()))));
        options
    };
    let default_agent = {
        let (read, write) = (draft.clone(), draft.clone());
        SettingField::dropdown(choices(false), move |_| read.borrow().default_agent.clone().into(), move |value: SharedString, _| {
            let mut d = write.borrow_mut();
            d.default_agent = value.to_string();
            d.normalize();
        })
    };
    let quick_agent = {
        let (read, write) = (draft.clone(), draft.clone());
        SettingField::dropdown(choices(true), move |_| read.borrow().quick_agent.clone().into(), move |value: SharedString, _| write.borrow_mut().quick_agent = value.to_string())
    };
    let several = draft.borrow().agents.len() > 1;
    let mut choose = SettingGroup::new().title("Which one");
    if !draft.borrow().agents.is_empty() {
        choose = choose.item(SettingItem::new("Default agent", default_agent).description("New tasks run it, unless their project or the new-task dialog chooses another."));
        choose = choose.item(
            SettingItem::new("Agent for quick questions", quick_agent)
                .description("Writes commit messages and compares variants. It gets one question at a time, answers and exits."),
        );
    }
    SettingPage::new("Agents")
        .default_open(true)
        .description(if several {
            "Each task runs one of these in its agent tab. To hand a task to another agent, use Continue with another agent… in its menu; a project's menu sets the agent for its new tasks."
        } else {
            "The agent each task runs in its agent tab. With more than one, you choose the agent when making a task, and can hand a task over to another later."
        })
        .group(SettingGroup::new().title("Profiles").item(list))
        .group(choose)
        .group(
            SettingGroup::new().title("Chat agents").item(
                SettingItem::new("Stop an idle one out of view after", {
                    let (read, write) = (draft.clone(), draft.clone());
                    SettingField::number_input(NumberFieldOptions { min: 0., max: 24. * 60., step: 5. }, move |_| f64::from(read.borrow().agent_idle_minutes), move |value, _| {
                        write.borrow_mut().agent_idle_minutes = value as u32
                    })
                    .default_value(f64::from(state::Settings::default().agent_idle_minutes))
                })
                .description("Minutes; 0 means never. A chat agent that is neither working nor waiting for an answer, in a task you have not opened for this long, is stopped to free its memory (hundreds of MB each). Its conversation is kept, and it resumes when you open the task. Terminal agents keep running."),
            )
            .item(
                SettingItem::new("Decline an unanswered question after", {
                    let (read, write) = (draft.clone(), draft.clone());
                    SettingField::number_input(NumberFieldOptions { min: 0., max: 24. * 60., step: 5. }, move |_| f64::from(read.borrow().agent_unanswered_minutes), move |value, _| {
                        write.borrow_mut().agent_unanswered_minutes = value as u32
                    })
                    .default_value(f64::from(state::Settings::default().agent_unanswered_minutes))
                })
                .description("Minutes; 0 means never. A chat agent keeps working after the window closes; its questions come as desktop notifications and wait for you. With a limit, a question is declined after this long and the agent moves on."),
            ),
        )
        .group(SettingGroup::new().title("Claude Code").item(lsp_switch))
        .group(
            SettingGroup::new().title("Context and tokens").items(dials).item(
                SettingItem::new("Tell me when a session has used", token_warn)
                    .description("A notification, once per session, when its agent has used this many tokens: input not read from the cache, plus output. The agent is not stopped."),
            ),
        )
}

/// What each kind of agent does with ideae, for the profile editor.
fn kind_hint(kind: crate::agents::Kind) -> &'static str {
    use crate::agents::Kind;
    match kind {
        Kind::Claude => "Reports through its hooks; each task resumes its own session by id. For a second account, set CLAUDE_CONFIG_DIR.",
        Kind::Codex => "Not tried live: its adapter follows Codex's source. Reports the end of each turn through notify and its approvals as terminal notifications; resumes a task's session by id. For a second account, set CODEX_HOME.",
        Kind::OpenCode => "Not tried live: its adapter follows OpenCode's source. Reports through a plugin ideae adds for the session (OPENCODE_CONFIG_CONTENT); resumes a task's session by id. Separate accounts: XDG_DATA_HOME.",
        Kind::Generic => "Any program that runs in a terminal. Its state is guessed from its output: working while it writes, done when it goes quiet or rings the bell.",
    }
}

/// The ACP registry's agents, searched: the one chosen opens in the
/// profile editor, ready to save; one to download is fetched first.
fn registry_dialog(draft: Draft, window: &mut Window, cx: &mut App) {
    use crate::agents::registry::{self, How};
    use gpui_kit::component::command::{Command, CommandItem, CommandState};
    enum Listed {
        Fetching,
        Failed(String),
        Ready(Vec<(registry::Entry, How)>),
    }
    let listed: Rc<RefCell<Listed>> = Rc::new(RefCell::new(Listed::Fetching));
    let command = cx.new(|cx| CommandState::new(window, cx));
    {
        let listed = listed.clone();
        window
            .spawn(cx, async move |cx| {
                let got = cx
                    .background_spawn(async move {
                        let entries = registry::fetch()?;
                        Ok::<_, String>(entries.into_iter().map(|e| {
                            let how = registry::how(&e, &|p| crate::agents::find::on_path(p));
                            (e, how)
                        }).collect::<Vec<_>>())
                    })
                    .await;
                *listed.borrow_mut() = match got {
                    Ok(entries) => Listed::Ready(entries),
                    Err(e) => Listed::Failed(e),
                };
                let _ = cx.update(|window, _| window.refresh());
            })
            .detach();
    }
    let focused = Rc::new(Cell::new(false));
    window.open_dialog(cx, move |dialog, _, _| {
        let (listed, command, focused, draft) = (listed.clone(), command.clone(), focused.clone(), draft.clone());
        dialog.title("ACP agents").w(px(640.)).p_0().content(move |content, window, cx| {
            let muted = cx.theme().muted_foreground;
            let note = |text: String| div().p_4().text_sm().text_color(muted).child(text);
            let entries = match &*listed.borrow() {
                Listed::Fetching => return content.child(note("Fetching the registry…".into())),
                Listed::Failed(why) => return content.child(note(why.clone())),
                Listed::Ready(entries) => entries.clone(),
            };
            if !focused.replace(true) {
                let command = command.clone();
                window.defer(cx, move |window, cx| command.read(cx).focus_handle(cx).focus(window, cx));
            }
            // Grouped by how each would run here: what is installed first.
            let mut groups: Vec<(&'static str, Vec<(registry::Entry, How)>)> = vec![("Installed", vec![]), ("Run with npx or uvx", vec![]), ("Downloaded when added", vec![]), ("Not available here", vec![])];
            for (e, how) in entries {
                let at = match how {
                    How::Installed(_) => 0,
                    How::Package(_) => 1,
                    How::Download => 2,
                    How::Not(_) => 3,
                };
                groups[at].1.push((e, how));
            }
            groups.retain(|(_, list)| !list.is_empty());
            let mut list = Command::new(&command).bordered(false).placeholder("Search agents…").max_h(px(460.));
            for (head, members) in &groups {
                let items = members.iter().map(|(e, how)| {
                    let tag = match how {
                        How::Installed(c) | How::Package(c) => c.clone(),
                        How::Download => e.version.clone(),
                        How::Not(why) => why.clone(),
                    };
                    let (name, about) = (e.name.clone(), short(&e.description, 110));
                    let words = vec![e.id.clone(), e.description.clone()];
                    CommandItem::new().label(name.clone()).keywords(words).child(move |_, cx| {
                        let muted = cx.theme().muted_foreground;
                        gpui_kit::component::v_flex()
                            .w_full()
                            .py_0p5()
                            .child(h_flex().gap_2().child(div().text_sm().child(name.clone())).child(div().flex_1()).child(div().text_xs().text_color(muted).truncate().max_w(px(260.)).child(tag.clone())))
                            .child(div().text_xs().text_color(muted).truncate().child(about.clone()))
                    })
                });
                list = list.group(gpui_kit::component::command::CommandGroup::new().label(*head).items(items));
            }
            let groups: Vec<Vec<(registry::Entry, How)>> = groups.into_iter().map(|(_, list)| list).collect();
            let draft = draft.clone();
            content
                .child(
                    list
                        .on_confirm(move |index, window, cx| {
                            let Some((entry, how)) = groups.get(index.section).and_then(|g| g.get(index.row)).cloned() else { return };
                            let made = entry.clone();
                            let template = move |command: String| crate::agents::Profile {
                                name: made.name.clone(),
                                kind: registry::kind_of(&made),
                                command: registry::adjust(&made, command),
                                acp: true,
                                env: made.distribution.npx.as_ref().map(|p| p.env.clone()).unwrap_or_default(),
                                ..Default::default()
                            };
                            match how {
                                How::Installed(command) | How::Package(command) => {
                                    window.close_dialog(cx);
                                    profile_dialog(draft.clone(), None, Some(template(command)), window, cx);
                                }
                                How::Download => {
                                    window.close_dialog(cx);
                                    let note = gpui_kit::component::notification::Notification::new().title(format!("Downloading {} {}…", entry.name, entry.version)).message("Checked against the registry's checksum, unpacked into the app's data folder.");
                                    window.push_notification(note, cx);
                                    let draft = draft.clone();
                                    window
                                        .spawn(cx, async move |cx| {
                                            let job = entry.clone();
                                            let installed = cx.background_spawn(async move { registry::install(&job) }).await;
                                            let _ = cx.update(|window, cx| match installed {
                                                Ok(command) => profile_dialog(draft, None, Some(template(command)), window, cx),
                                                Err(e) => window.push_notification(gpui_kit::component::notification::Notification::error(format!("{}: {e}", entry.name)), cx),
                                            });
                                        })
                                        .detach();
                                }
                                How::Not(why) => {
                                    let site = entry.website.clone().or(entry.repository.clone()).unwrap_or_default();
                                    window.push_notification(gpui_kit::component::notification::Notification::warning(format!("{} does not run here: {why}. {site}", entry.name)), cx);
                                }
                            }
                        }),
                )
                .child(div().px_4().pb_3().text_xs().text_color(muted).child("From agentclientprotocol.com's registry. Installed ones run as they are; others through npx or uvx, or downloaded. Each signs in its own way; Claude Code's adapter runs on an API key."))
        })
    });
}

fn short(text: &str, most: usize) -> String {
    let text = text.split(['.', '\n']).next().unwrap_or(text).trim();
    if text.chars().count() <= most {
        return text.to_string();
    }
    format!("{}…", text.chars().take(most).collect::<String>())
}

/// What each kind of agent does as a chat, for the profile editor.
fn chat_hint(kind: crate::agents::Kind) -> &'static str {
    use crate::agents::Kind;
    match kind {
        Kind::Claude => "Anthropic's ACP adapter for Claude Code, always on an API key (below): an app may not offer a claude.ai plan's login. For your plan, use a terminal profile.",
        Kind::Codex => "The ACP adapter for Codex: signs in with ChatGPT or an API key (OPENAI_API_KEY), as it asks in the chat.",
        Kind::OpenCode => "OpenCode's own ACP server: its providers and models are OpenCode's (opencode auth login).",
        Kind::Generic => "Any program that speaks ACP: what it does, asks and costs is told by it, not guessed. It signs in its own way when it asks.",
    }
}

/// The key a chat agent runs on, when it is one known: its variable, and
/// where to make one.
fn api_key_var(kind: crate::agents::Kind, command: &str) -> Option<(&'static str, &'static str)> {
    use crate::agents::Kind;
    match kind {
        Kind::Claude => Some(("ANTHROPIC_API_KEY", "console.anthropic.com → API keys")),
        Kind::Codex => Some(("OPENAI_API_KEY", "platform.openai.com → API keys")),
        Kind::Generic if command.contains("gemini") => Some(("GEMINI_API_KEY", "aistudio.google.com → Get API key")),
        _ => None,
    }
}

/// `text` (KEY=value lines) with `name` set to `value`, in place of what it had.
fn with_env_line(text: &str, name: &str, value: &str) -> String {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            line.split_once('=').is_none_or(|(k, _)| k.trim() != name)
        })
        .map(str::to_string)
        .collect();
    lines.push(format!("{name}={value}"));
    lines.join("\n").trim_start().to_string()
}

/// Adds a profile (`at` none, from `template` if given) or edits one, in the draft.
fn profile_dialog(draft: Draft, at: Option<usize>, template: Option<crate::agents::Profile>, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
    let editing = at.and_then(|ix| draft.borrow().agents.get(ix).cloned()).or(template).unwrap_or_else(|| crate::agents::Profile {
        kind: crate::agents::Kind::Codex,
        name: "Codex".into(),
        command: "codex".into(),
        ..Default::default()
    });
    let name = cx.new(|cx| InputState::new(window, cx).placeholder("Claude — work account"));
    let command = cx.new(|cx| InputState::new(window, cx).placeholder("claude --model opus"));
    let env = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("KEY=value, one a line: CLAUDE_CONFIG_DIR=~/.claude-work"));
    let key = cx.new(|cx| InputState::new(window, cx).placeholder("Paste a key"));
    let limit = cx.new(|cx| InputState::new(window, cx).placeholder("No limit"));
    let mark = cx.new(|cx| InputState::new(window, cx).placeholder("Its letter"));
    mark.update(cx, |i, cx| i.set_value(editing.mark.clone(), window, cx));
    let color = Rc::new(Cell::new(editing.color));
    limit.update(cx, |i, cx| i.set_value(editing.daily_limit.map(|l| format!("{l}")).unwrap_or_default(), window, cx));
    key.update(cx, |i, cx| i.set_masked(true, window, cx));
    name.update(cx, |i, cx| i.set_value(editing.name.clone(), window, cx));
    command.update(cx, |i, cx| i.set_value(editing.command.clone(), window, cx));
    env.update(cx, |i, cx| i.set_value(editing.env_text(), window, cx));
    let kind = Rc::new(Cell::new(editing.kind));
    let chat = Rc::new(Cell::new(editing.acp));
    // What its chat chose last, shown by name; cleared by Forget.
    let remembered: Vec<String> = editing
        .choices
        .iter()
        .map(|(id, value)| {
            let option = editing.offered.iter().find(|o| &o.id == id);
            let name = option.map_or(id.as_str(), |o| o.name.as_str());
            let shown = option.and_then(|o| o.values.iter().find(|v| &v.value == value)).map_or(value.as_str(), |v| v.name.as_str());
            format!("{name}: {shown}")
        })
        .collect();
    let forget = Rc::new(Cell::new(false));
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let (name, command, env, kind, chat, draft, editing, key, limit, mark, color) = (name.clone(), command.clone(), env.clone(), kind.clone(), chat.clone(), draft.clone(), editing.clone(), key.clone(), limit.clone(), mark.clone(), color.clone());
        let (save_mark, save_color) = (mark.clone(), color.clone());
        let spent = crate::acp::spend::spent_today(&editing.id);
        let (save_name, save_command, save_env, save_kind, save_chat, save_forget, save_key, save_limit) = (name.clone(), command.clone(), env.clone(), kind.clone(), chat.clone(), forget.clone(), key.clone(), limit.clone());
        let (remembered, forget) = (remembered.clone(), forget.clone());
        let label = |text: &'static str| div().text_sm().w(px(110.)).flex_none().child(text);
        let save = Rc::new(move |window: &mut Window, cx: &mut App| {
            let k = save_kind.get();
            let command = save_command.read(cx).value().trim().to_string();
            let acp = save_chat.get();
            let command = if command.is_empty() { if acp { k.acp_command() } else { k.program() }.to_string() } else { command };
            if command.is_empty() {
                return;
            }
            let mut name = save_name.read(cx).value().trim().to_string();
            if name.is_empty() {
                name = command.split_whitespace().next().unwrap_or("Agent").to_string();
            }
            let mut env_text = save_env.read(cx).value().to_string();
            let typed = save_key.read(cx).value().trim().to_string();
            if let (true, Some((var, _))) = (acp && !typed.is_empty(), api_key_var(k, &command)) {
                env_text = with_env_line(&env_text, var, &typed);
            }
            {
                let mut d = draft.borrow_mut();
                let id = match at {
                    Some(_) if !editing.id.is_empty() => editing.id.clone(),
                    _ => crate::agents::new_profile_id(&name, &d.agents),
                };
                let choices = if save_forget.get() { Default::default() } else { editing.choices.clone() };
                let daily_limit = save_limit.read(cx).value().trim().trim_start_matches('$').parse::<f64>().ok().filter(|l| *l > 0.0 && acp);
                let mark: String = save_mark.read(cx).value().trim().chars().take(3).collect();
                let mut profile = crate::agents::Profile { id, name, kind: k, command, acp, secrets: editing.secrets.clone(), choices, offered: editing.offered.clone(), daily_limit, mark, color: save_color.get(), ..Default::default() };
                profile.set_env_text(&env_text);
                match at.filter(|ix| *ix < d.agents.len()) {
                    Some(ix) => d.agents[ix] = profile,
                    None => d.agents.push(profile),
                }
                d.normalize();
            }
            window.close_dialog(cx);
            window.refresh();
        });
        dialog
            .title(if at.is_some() { "Edit agent" } else { "Add agent" })
            .w(px(620.))
            // Enter in a field reaches the dialog: it saves, as the button does.
            .on_ok({
                let save = save.clone();
                move |_, window, cx| {
                    save(window, cx);
                    false
                }
            })
            .content(move |content, _, cx| {
                let current = kind.get();
                let as_chat = chat.get();
                let key_var = as_chat.then(|| api_key_var(current, &command.read(cx).value())).flatten();
                // Set: in the environment below, sealed or not.
                let key_set = key_var.is_some_and(|(var, _)| crate::agents::Profile::parse_env(&env.read(cx).value()).get(var).is_some_and(|v| !v.is_empty()));
                let own = move |k: crate::agents::Kind, chat: bool| if chat { k.acp_command() } else { k.program() };
                content.child(
                    gpui_kit::component::v_flex()
                        .gap_2()
                        .child(h_flex().gap_2().items_center().child(label("Kind")).children(crate::agents::Kind::ALL.into_iter().map(|k| {
                            let (kind, command, name) = (kind.clone(), command.clone(), name.clone());
                            Button::new(("agent-kind", k as usize)).xsmall().ghost().label(k.label()).selected(current == k).on_click(move |_, window, cx| {
                                let before = kind.replace(k);
                                // A command or name that was the old kind's own follows the new kind.
                                if command.read(cx).value().trim() == own(before, as_chat) {
                                    command.update(cx, |i, cx| i.set_value(own(k, as_chat), window, cx));
                                }
                                if name.read(cx).value().trim().is_empty() || name.read(cx).value() == before.label() {
                                    name.update(cx, |i, cx| i.set_value(if k == crate::agents::Kind::Generic { "" } else { k.label() }, window, cx));
                                }
                                window.refresh();
                            })
                        })))
                        .child(div().text_xs().text_color(muted).child(if as_chat { chat_hint(current) } else { kind_hint(current) }))
                        .child(h_flex().gap_2().items_center().child(label("Runs as")).children([(false, "Terminal"), (true, "Chat (ACP)")].into_iter().map(|(c, text)| {
                            let (chat, command) = (chat.clone(), command.clone());
                            Button::new(("agent-chat", c as usize)).xsmall().ghost().label(text).selected(as_chat == c).on_click(move |_, window, cx| {
                                let before = chat.replace(c);
                                if command.read(cx).value().trim() == own(current, before) {
                                    command.update(cx, |i, cx| i.set_value(own(current, c), window, cx));
                                }
                                window.refresh();
                            })
                        })))
                        .when(as_chat && !remembered.is_empty() && !forget.get(), |this| {
                            let forget = forget.clone();
                            this.child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(label("Starts with"))
                                    .child(div().flex_1().text_sm().child(remembered.join(" · ")))
                                    .child(Button::new("agent-forget").xsmall().ghost().label("Forget").tooltip("New sessions start with the agent's own defaults again").on_click(move |_, window, _| {
                                        forget.set(true);
                                        window.refresh();
                                    })),
                            )
                            .child(div().text_xs().text_color(muted).child("The model, effort and mode you last picked in its chat; every new session starts with them."))
                        })
                        .when(as_chat, |this| {
                            this.child(div().text_xs().text_color(muted).child(
                                "Its conversation shows as a chat instead of a terminal: messages, tool calls with their changes, questions with buttons. It keeps working after the window closes.",
                            ))
                        })
                        .child(h_flex().gap_2().items_center().child(label("Name")).child(div().flex_1().child(Input::new(&name))))
                        .child({
                            // As it will show on the tasks: what is typed, else the agent's letter;
                            // a click gives it the next color, the last being none.
                            let shown = crate::agents::Profile { kind: current, name: name.read(cx).value().to_string(), mark: mark.read(cx).value().to_string(), ..Default::default() }.letter();
                            let next = color.clone();
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(label("Mark"))
                                .child(
                                    div()
                                        .id("agent-mark-color")
                                        .w(px(36.))
                                        .flex()
                                        .justify_center()
                                        .cursor_pointer()
                                        .child(crate::workspace::spaces::agent_chip(&shown, color.get(), cx.theme().border, muted))
                                        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Click for another color").build(window, cx))
                                        .on_click(move |_, window, _| {
                                            next.set((next.get() + 1) % (crate::workspace::spaces::COLORS.len() as u8 + 1));
                                            window.refresh();
                                        }),
                                )
                                .child(div().w(px(90.)).flex_none().child(Input::new(&mark)))
                                .child(div().flex_1().min_w_0().text_xs().text_color(muted).child("A letter or a few and a color (click the mark), shown on its tasks to tell two profiles of one agent apart, such as work and personal."))
                        })
                        .child(h_flex().gap_2().items_center().child(label("Command")).child(div().flex_1().child(Input::new(&command))))
                        .child(div().text_xs().text_color(muted).child(if as_chat {
                            "What starts its ACP server: opencode acp, gemini --acp, kimi acp, qwen --acp, npx -y <package>. ideae resumes its sessions itself."
                        } else {
                            "The program and its flags, as typed in a shell: a model, a mode. ideae adds what starts or resumes a session and what reports to it."
                        }))
                        .when_some(key_var, |this, (var, where_)| {
                            this.child(h_flex().gap_2().items_center().child(label("API key")).child(div().flex_1().child(Input::new(&key).mask_toggle())))
                                .child(div().text_xs().text_color(muted).child(if key_set {
                                    format!("{var} is set and kept sealed for this machine. Type a new one to replace it, or remove its line below.")
                                } else {
                                    format!("Goes in {var}, kept sealed for this machine, not in the settings. Make one at {where_}.")
                                }))
                        })
                        .when(as_chat, |this| {
                            this.child(h_flex().gap_2().items_center().child(label("Daily limit, $")).child(div().w(px(120.)).child(Input::new(&limit))).child(div().text_xs().text_color(muted).child(if spent > 0.0 { format!("${spent:.2} today") } else { String::new() })))
                                .child(div().text_xs().text_color(muted).child("Its turn is stopped once its sessions cost this much today (UTC), going by the cost the agent reports. Claude Code's adapter reports it; agents that report nothing are not limited."))
                        })
                        .child(h_flex().gap_2().items_start().child(label("Environment")).child(div().flex_1().child(Textarea::new(&env))))
                        .child(div().text_xs().text_color(muted).child("For this agent only. A second account of the same agent is another profile with its own config folder set here. Keys and tokens (…_KEY, …_TOKEN) are kept sealed on this machine, not in the settings; $(pass show anthropic) takes one from a password manager when the agent starts.")),
                )
            })
            .footer(h_flex().justify_end().gap_2().child(Button::new("agent-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                Button::new("agent-save").primary().label("Save").on_click({
                    let save = save.clone();
                    move |_, window, cx| save(window, cx)
                }),
            ))
    });
}

thread_local! {
    /// Which language servers are there, as found when the dialog asked first.
    static INSTALLED: RefCell<std::collections::HashMap<&'static str, bool>> = RefCell::default();
    /// What opens the list of spaces, for the page that has a button for it.
    static EDIT_SPACES: RefCell<Option<Rc<dyn Fn(&mut Window, &mut App)>>> = const { RefCell::new(None) };
}

impl Workspace {
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_at(0, window, cx)
    }

    /// Settings, open at one of its pages.
    pub fn open_settings_at(&mut self, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let spaces = this.clone();
        EDIT_SPACES.with(|edit| {
            *edit.borrow_mut() = Some(Rc::new(move |window, cx| {
                let _ = spaces.update(cx, |w, cx| w.spaces_dialog(window, cx));
            }))
        });
        open(state::settings(), self.appearance, page, window, cx, move |settings, appearance, window, cx| {
            let _ = this.update(cx, |w, cx| {
                w.apply_settings(settings, cx);
                if appearance != w.appearance {
                    w.set_appearance(appearance, window, cx);
                }
            });
        });
    }
}

/// The dialog over `current`, at the page `first`; Save hands `save` the result.
fn open(
    current: state::Settings,
    appearance: Appearance,
    first: usize,
    window: &mut Window,
    cx: &mut App,
    save: impl Fn(state::Settings, Appearance, &mut Window, &mut App) + 'static,
) {
    let draft: Draft = Rc::new(RefCell::new(current));
    let look = Rc::new(Cell::new(appearance));
    INSTALLED.with(|known| known.borrow_mut().clear());
    crate::workspace::welcome::forget_find();
    // The shortcut waiting for keys, and what catches them before any
    // binding does.
    let recording: Rc<Cell<Option<&'static str>>> = Rc::default();
    let catcher: Rc<RefCell<Option<Subscription>>> = Rc::default();
    let save = Rc::new(save);
    window.open_dialog(cx, move |dialog, window, _| {
        // Room for the longer titles of the pages' list, as the window allows.
        let wide = (window.viewport_size().width - px(64.)).clamp(px(640.), px(1000.));
        let (draft, look, recording, catcher) = (draft.clone(), look.clone(), recording.clone(), catcher.clone());
        let save = {
            let (draft, look, save) = (draft.clone(), look.clone(), save.clone());
            move |window: &mut Window, cx: &mut App| {
                let mut settings = draft.borrow().clone();
                settings.normalize();
                settings.lsp_idle_minutes = settings.lsp_idle_minutes.max(1);
                save(settings, look.get(), window, cx);
                window.close_dialog(cx);
            }
        };
        dialog
            .title("Settings")
            .w(wide)
            .content(move |content, window, cx| {
                let dark_now = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
                // The dialog is as tall as the window lets it; the pages scroll inside.
                let height = (window.viewport_size().height - px(220.)).clamp(px(200.), px(620.));
                content.child(
                    div()
                        .h(height)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .overflow_hidden()
                        .child(
                            Settings::new("settings")
                                .sidebar_width(px(240.))
                                .default_selected_index(SelectIndex { page_ix: first, group_ix: None })
                                .pages(pages(&draft, &look, dark_now, &recording, &catcher)),
                        ),
                )
            })
            // Enter in a field or the search reaches the dialog: it stays, the edits with it.
            .on_ok(|_, _, _| false)
            .footer(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Button::new("settings-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("settings-save").primary().label("Save").on_click(move |_, window, cx| save(window, cx))),
            )
    });
}

/// The shortcuts by where they work, each with its keys: a click waits for
/// new ones (Esc leaves them, Backspace removes them), ↺ puts the default
/// back.
fn key_groups(
    draft: &Draft,
    recording: &Rc<Cell<Option<&'static str>>>,
    catcher: &Rc<RefCell<Option<Subscription>>>,
) -> Vec<SettingGroup> {
    use crate::keymap::{SHORTCUTS, display};
    let d = draft.borrow().clone();
    let effective = |id: &str, default: String| d.keys.get(id).cloned().unwrap_or(default);
    let mut all: Vec<(&'static str, &'static str, String)> =
        SHORTCUTS.iter().map(|s| (s.id, s.name, effective(s.id, s.default_keys()))).collect();
    // macOS's own: a shortcut given one of them is told so.
    if cfg!(target_os = "macos") {
        all.extend(crate::menu::SYSTEM_KEYS.iter().map(|&(keys, name)| (name, name, keys.to_string())));
    }
    let item = |shortcut: &'static crate::keymap::Shortcut| {
        let id = shortcut.id;
        let keys = effective(id, shortcut.default_keys());
        let waiting = recording.get() == Some(id);
        let clash: Vec<&'static str> =
            all.iter().filter(|(other, _, k)| *other != id && !keys.is_empty() && *k == keys).map(|(_, name, _)| *name).collect();
        let (draft, recording, catcher) = (draft.clone(), recording.clone(), catcher.clone());
        let changed = d.keys.contains_key(id);
        let shown = display(&keys);
        let (dirty_draft, reset_all_draft) = (draft.clone(), draft.clone());
        let field = SettingField::render(move |_, _, _| {
            let (draft, recording, catcher) = (draft.clone(), recording.clone(), catcher.clone());
            let reset_draft = draft.clone();
            let keys = keys.clone();
            h_flex()
                .id(SharedString::from(format!("key-row-{id}")))
                .test_support()
                .gap_2()
                .when(!clash.is_empty(), |row| {
                    row.child(div().text_xs().text_color(gpui_kit::red()).child(format!("also {}", clash.join(", "))))
                })
                .child(
                    Button::new(SharedString::from(format!("key-{id}")))
                        .small()
                        .outline()
                        .w(px(150.))
                        .label(if waiting {
                            "Press keys… Esc".to_string()
                        } else if keys.is_empty() {
                            "none".to_string()
                        } else {
                            display(&keys)
                        })
                        .selected(waiting)
                        .on_click(move |_, window, cx| {
                            recording.set(Some(id));
                            let (draft, recording, own) = (draft.clone(), recording.clone(), catcher.clone());
                            let subscription = cx.intercept_keystrokes(move |event, window, cx| {
                                let Some(id) = recording.get() else { return };
                                let k = &event.keystroke;
                                // A modifier alone waits for its key; it opens nothing meanwhile (Alt, the menu bar).
                                if matches!(k.key.as_str(), "shift" | "control" | "alt" | "platform" | "function" | "") {
                                    cx.stop_propagation();
                                    return;
                                }
                                cx.stop_propagation();
                                let plain = !k.modifiers.modified();
                                match k.key.as_str() {
                                    "escape" if plain => {}
                                    "backspace" | "delete" if plain => {
                                        draft.borrow_mut().keys.insert(id.to_string(), String::new());
                                    }
                                    _ => {
                                        let keys = k.unparse(cx.platform_info().os);
                                        let default = crate::keymap::find(id).map(|s| s.default_keys()).unwrap_or_default();
                                        let mut d = draft.borrow_mut();
                                        if keys == default {
                                            d.keys.remove(id);
                                        } else {
                                            d.keys.insert(id.to_string(), keys);
                                        }
                                    }
                                }
                                recording.set(None);
                                let own = own.clone();
                                window.defer(cx, move |_, _| {
                                    own.borrow_mut().take();
                                });
                                window.refresh();
                            });
                            *catcher.borrow_mut() = Some(subscription);
                            window.refresh();
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("key-reset-{id}")))
                        .small()
                        .ghost()
                        .label("↺")
                        .tooltip("Back to the default")
                        .when(!changed, |b| b.invisible())
                        .on_click(move |_, window, _| {
                            reset_draft.borrow_mut().keys.remove(id);
                            window.refresh();
                        }),
                )
            })
            .on_reset(move |_| dirty_draft.borrow().keys.contains_key(id), move |_, cx| {
                reset_all_draft.borrow_mut().keys.remove(id);
                cx.refresh_windows();
            });
            SettingItem::new(shortcut.name, field).keywords([shown])
        };
        ["Window", "Editor", "Picture", "Changes", "Terminal", "Web"]
            .into_iter()
            .map(|area| SettingGroup::new().title(area).items(SHORTCUTS.iter().filter(|s| s.area() == area).map(&item)))
            .collect()
    }

#[cfg(test)]
mod tests {
    #[test]
    fn a_key_typed_takes_its_line_in_the_environment() {
        let text = "CLAUDE_CONFIG_DIR=~/.c\nexport ANTHROPIC_API_KEY=••••••••";
        assert_eq!(super::with_env_line(text, "ANTHROPIC_API_KEY", "sk-new"), "CLAUDE_CONFIG_DIR=~/.c\nANTHROPIC_API_KEY=sk-new");
        assert_eq!(super::with_env_line("", "OPENAI_API_KEY", "x"), "OPENAI_API_KEY=x");
        assert_eq!(super::api_key_var(crate::agents::Kind::Generic, "gemini --acp").map(|k| k.0), Some("GEMINI_API_KEY"));
        assert_eq!(super::api_key_var(crate::agents::Kind::OpenCode, "opencode acp"), None);
    }

    use crate::theme::Appearance;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Bounds, Empty, Point, ScrollDelta, TestAppContext, WindowBounds, WindowOptions, point, px, size};

    use super::{Rc, RefCell, open, state};

    fn window(cx: &mut TestAppContext) -> gpui_kit::AnyWindowHandle {
        cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1200.), px(800.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| Empty)).expect("a test window").0
        })
    }

    #[gpui_kit::test]
    fn every_page_draws(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for page in 0..10 {
            let handle = window(cx);
            cx.update_window(handle, |_, window, cx| {
                open(state::Settings::default(), Appearance::Auto, page, window, cx, |_, _, _, _| {});
                window.render_frame(cx);
                assert_eq!(window.try_find("key-row-quit").is_some(), page == super::KEYS_PAGE, "page {page}");
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    fn keys_scroll_and_save(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = window(cx);
        let saved: Rc<RefCell<Option<state::Settings>>> = Rc::default();
        cx.update_window(handle, |_, window, cx| {
            let mut current = state::Settings::default();
            current.agents[0].command = "mine".into();
            let saved = saved.clone();
            open(current, Appearance::Auto, super::KEYS_PAGE, window, cx, move |settings, _, _, _| *saved.borrow_mut() = Some(settings));
            window.render_frame(cx);
            let before = window.find("key-row-recent").bounds();
            assert!(window.try_find("key-row-comment").is_none_or(|row| !row.visible()), "the last keys start below the fold");
            window.scroll("key-row-recent", ScrollDelta::Pixels(point(px(0.), px(-300.))), cx);
            let after = window.try_find("key-row-recent").map(|row| row.bounds());
            assert!(after.is_none_or(|after| after.top() < before.top()), "the page scrolled");
            window.click("settings-save", cx);
        })
        .unwrap();
        assert_eq!(saved.borrow().as_ref().map(|s| s.agent.as_str()), Some("mine"), "the older field follows the default profile");
    }

    /// The Agents page lists the profiles; one removed is gone on Save,
    /// and the default moves to one still there.
    #[gpui_kit::test]
    fn agents_are_listed_and_removed(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = window(cx);
        let saved: Rc<RefCell<Option<state::Settings>>> = Rc::default();
        cx.update_window(handle, |_, window, cx| {
            let mut current = state::Settings::default();
            current.agents.push(crate::agents::Profile { id: "codex".into(), name: "Codex".into(), kind: crate::agents::Kind::Codex, command: "codex".into(), ..Default::default() });
            let saved = saved.clone();
            open(current, Appearance::Auto, super::AGENTS_PAGE, window, cx, move |settings, _, _, _| *saved.borrow_mut() = Some(settings));
            window.render_frame(cx);
            assert!(window.try_find("agent-profile-1").is_some(), "both listed");
            window.click("agent-remove-0", cx);
            window.render_frame(cx);
            window.click("settings-save", cx);
        })
        .unwrap();
        let saved = saved.borrow();
        let saved = saved.as_ref().unwrap();
        assert_eq!(saved.agents.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["codex"]);
        assert_eq!((saved.default_agent.as_str(), saved.agent.as_str()), ("codex", "codex"));
    }
}
