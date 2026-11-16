//! Settings (Ctrl+,): a dialog over the state's settings, applied on Save.
//! Pages on the left with a filter over them, the page's settings on the
//! right.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    setting::{NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
};
use gpui_kit::{TestSupportExt as _, prelude::FluentBuilder as _, *};

use crate::{lsp, state, workspace::Workspace};

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

fn pages(draft: &Draft, recording: &Rc<Cell<Option<&'static str>>>, catcher: &Rc<RefCell<Option<Subscription>>>) -> Vec<SettingPage> {
    let default = state::Settings::default();
    let agent = {
        let (read, write) = (draft.clone(), draft.clone());
        SettingField::input(move |_| SharedString::from(read.borrow().agent.clone()), move |value: SharedString, _| {
            write.borrow_mut().agent = value.to_string()
        })
        .default_value(default.agent.clone())
    };
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
    let theme = {
        let (read, write) = (draft.clone(), draft.clone());
        let options = crate::theme::FAMILIES.iter().map(|family| (SharedString::from(family.name), SharedString::from(family.name))).collect();
        SettingField::dropdown(options, move |_| read.borrow().theme.clone().into(), move |value, _| write.borrow_mut().theme = value.to_string())
            .default_value(SharedString::from(default.theme.clone()))
    };
    let servers = lsp::SERVERS.iter().map(|def| {
        let program = def.program;
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
        // Looked for on disk once while the dialog is open: this runs with every frame of it.
        let there = INSTALLED.with(|known| *known.borrow_mut().entry(program).or_insert_with(|| def.available()));
        let missing = if there { "" } else { ", not installed" };
        SettingItem::new(program, field).description(format!("{}{missing}", def.extensions()))
    });
    vec![
        SettingPage::new("General").group(
            SettingGroup::new().title("Notices")
                .item(switch(
                    draft,
                    "Notifications",
                    "A desktop notification when an agent out of view needs you or finishes.",
                    |s| s.notifications,
                    |s, v| s.notifications = v,
                ))
                .item(switch(
                    draft,
                    "Check for updates",
                    "Release builds ask GitHub for a newer release now and then.",
                    |s| s.check_updates,
                    |s, v| s.check_updates = v,
                )),
        ).group(
            SettingGroup::new().title("Look").item(
                SettingItem::new("Theme", theme)
                    .description("Each has a light and a dark side: which of the two is drawn is the switch in the title bar. Theme… in the menu shows them one after another as you go through the list."),
            ),
        ).group(
            SettingGroup::new().title("Title bar")
                .item(switch(
                    draft,
                    "Load",
                    "CPU, memory, and what the app and its tasks hold; the tooltip has every process.",
                    |s| s.show_load,
                    |s, v| s.show_load = v,
                ))
                .item(switch(
                    draft,
                    "Tokens",
                    "What the agents of the tasks open here took in the last 24 hours.",
                    |s| s.show_tokens,
                    |s, v| s.show_tokens = v,
                )),
        ).group(
            SettingGroup::new().title("Sidebar")
                .item(switch(
                    draft,
                    "Context dial on tasks",
                    "By a task in the sidebar, how full its agent's context is.",
                    |s| s.context_dial,
                    |s, v| s.context_dial = v,
                ))
                .item(
                    SettingItem::new("The narrow sidebar opens after", rail_open)
                        .description("Dragged as narrow as it goes, the sidebar is a rail of tasks to click. The whole of it opens over the task when the pointer rests on the rail this long."),
                ),
        ),
        SettingPage::new("Agent").group(
            SettingGroup::new().title("Command")
                .item(
                    SettingItem::new("Agent command", agent)
                        .description("Runs in each task's agent tab; empty for a plain shell. New tasks use it."),
                )
                .item(switch(
                    draft,
                    "Agents' language servers",
                    "Claude Code's LSP plugins, a rust-analyzer and the like per agent: much memory for every agent that runs, for go-to-definition in what it reads. For agents started after Save.",
                    |s| s.agent_lsp,
                    |s, v| s.agent_lsp = v,
                )),
        ).group(
            SettingGroup::new().title("Tokens")
                .item(
                    SettingItem::new("Tell me when a session has used", token_warn)
                        .description("A notification, once for a session, when its agent has used this much: input not read from the cache, and output. The agent is not stopped."),
                ),
        ),
        SettingPage::new("Editor").group(
            SettingGroup::new().title("Typing")
                .item(switch(
                    draft,
                    "CamelHumps words",
                    "Moving, selecting and deleting by word stops at the humps of camelCase and at the underscores of snake_case.",
                    |s| s.hump_words,
                    |s, v| s.hump_words = v,
                ))
                .item(switch(
                    draft,
                    "Completions as you type",
                    "The language server's suggestions come up while you type. Off, they come when asked for (Ctrl+Space).",
                    |s| s.complete_as_typed,
                    |s, v| s.complete_as_typed = v,
                ))
                .item(switch(
                    draft,
                    "Reformat on save",
                    "Ctrl+S has the language server reformat the file first. Not the save made when you leave a file.",
                    |s| s.format_on_save,
                    |s, v| s.format_on_save = v,
                )),
        ).group(
            SettingGroup::new().title("Tabs")
                .item(switch(
                    draft,
                    "Preview tab",
                    "A file opened to look at (a jump to a definition or a usage, a search result, a click in Files) opens in one tab, shown in italics, that the next one takes over. Editing it or double-clicking it keeps it.",
                    |s| s.preview_tabs,
                    |s, v| s.preview_tabs = v,
                ))
                .item(SettingItem::new("File tabs open at most", tab_limit).description(
                    "Past this many, the file tabs left longest close. Files with unsaved edits stay.",
                )),
        ).group(
            SettingGroup::new().title("Shown in the code")
                .item(switch(
                    draft,
                    "Colors from the language server",
                    "Names are colored by what the server knows them to be (a type, a parameter, a method), over the grammar's colors.",
                    |s| s.semantic_colors,
                    |s, v| s.semantic_colors = v,
                ))
                .item(switch(
                    draft,
                    "Gutter marks against the base branch",
                    "The marks beside the line numbers show what the task changed since it left its base branch, not only since the last commit.",
                    |s| s.gutter_since_base,
                    |s, v| s.gutter_since_base = v,
                )),
        ),
        SettingPage::new("Tasks").group(
            SettingGroup::new().title("New tasks")
                .item(switch(
                    draft,
                    "Copy .env files",
                    "New worktrees get the main checkout's ignored .env* files.",
                    |s| s.copy_env,
                    |s, v| s.copy_env = v,
                )),
        ).group(
            SettingGroup::new().title("Git in the background")
                .item(SettingItem::new("Fetch the base branch", fetch).description(
                    "Brings origin's main branch up to date in the background, so tasks show how far behind they are and what they share.",
                ))
                .item(switch(
                    draft,
                    "Pull request status",
                    "Ask gh about each task's PR and show it on the row.",
                    |s| s.pr_status,
                    |s, v| s.pr_status = v,
                )),
        ).group(
            SettingGroup::new().title("Changes")
                .item(switch(
                    draft,
                    "Unified diff",
                    "The Changes tab starts with the old and the new one under the other, not side by side.",
                    |s| s.diff_unified,
                    |s, v| s.diff_unified = v,
                ))
                .item(switch(
                    draft,
                    "Conflicts go to the agent",
                    "A pull, rebase, cherry-pick or revert that would conflict changes nothing, and the agent's prompt gets the request to do it and resolve the conflicts. Off, you are only told.",
                    |s| s.conflicts_to_agent,
                    |s, v| s.conflicts_to_agent = v,
                )),
        ),
        SettingPage::new("Language servers").default_open(true).groups([
            SettingGroup::new().title("Servers").items(servers),
            SettingGroup::new().title("Sleep").item(
                SettingItem::new("Sleep after (minutes)", idle)
                    .description("A task out of view this long stops its language servers."),
            ),
        ]),
        SettingPage::new("Keys")
            .description("Click a shortcut and press the new keys: Esc leaves them, Backspace removes them.")
            .default_open(true)
            .groups(key_groups(draft, recording, catcher)),
        SettingPage::new("Spaces")
            .description("Projects sorted into sets (work, personal, a client), one of them in the window at a time. The mark at the right of the title bar goes from one to another.")
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
                .description("Add, name, mark and remove them. Which space a project is of is set from its menu in the sidebar."),
            )),
        SettingPage::new("Presentation")
            .description("For showing the window to others: ☰ → Enter presentation mode leaves in it only the tasks you tick.")
            .group(
                SettingGroup::new().title("Shown larger").item(
                    SettingItem::new("Text size", present_zoom)
                        .description("Files, diffs and terminals are drawn this much larger while presenting; the sizes you chose come back when it ends."),
                ),
            )
            .group(
                SettingGroup::new().title("Kept out of sight")
                    .item(switch(
                        draft,
                        "Sidebar as a rail",
                        "The tasks left in the window show as a narrow rail: the start of their names, the whole list when the pointer rests on it.",
                        |s| s.present_rail,
                        |s, v| s.present_rail = v,
                    ))
                    .item(switch(
                        draft,
                        "Hide files git ignores",
                        "A file git ignores (keys, local settings) is left out of the Files tree, and shows a notice in place of what it holds until you ask for it.",
                        |s| s.present_veil,
                        |s, v| s.present_veil = v,
                    ))
                    .item(switch(
                        draft,
                        "Hide load and tokens",
                        "The title bar's readouts are not shown while presenting.",
                        |s| s.present_hide_readouts,
                        |s, v| s.present_hide_readouts = v,
                    )),
            ),
    ]
}

/// Where the shortcuts are among the pages.
pub const KEYS_PAGE: usize = 5;

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
        open(state::settings(), page, window, cx, move |settings, cx| {
            let _ = this.update(cx, |w, cx| w.apply_settings(settings, cx));
        });
    }
}

/// The dialog over `current`, at the page `first`; Save hands `save` the result.
fn open(current: state::Settings, first: usize, window: &mut Window, cx: &mut App, save: impl Fn(state::Settings, &mut App) + 'static) {
    let draft: Draft = Rc::new(RefCell::new(current));
    INSTALLED.with(|known| known.borrow_mut().clear());
    // The shortcut waiting for keys, and what catches them before any
    // binding does.
    let recording: Rc<Cell<Option<&'static str>>> = Rc::default();
    let catcher: Rc<RefCell<Option<Subscription>>> = Rc::default();
    let save = Rc::new(save);
    window.open_dialog(cx, move |dialog, _, _| {
        let (draft, recording, catcher) = (draft.clone(), recording.clone(), catcher.clone());
        let save = {
            let (draft, save) = (draft.clone(), save.clone());
            move |window: &mut Window, cx: &mut App| {
                let mut settings = draft.borrow().clone();
                settings.agent = settings.agent.trim().to_string();
                settings.lsp_idle_minutes = settings.lsp_idle_minutes.max(1);
                save(settings, cx);
                window.close_dialog(cx);
            }
        };
        dialog
            .title("Settings")
            .w(px(900.))
            .content(move |content, window, cx| {
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
                                .sidebar_width(px(200.))
                                .default_selected_index(SelectIndex { page_ix: first, group_ix: None })
                                .pages(pages(&draft, &recording, &catcher)),
                        ),
                )
            })
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
    let all: Vec<(&'static str, &'static str, String)> =
        SHORTCUTS.iter().map(|s| (s.id, s.name, effective(s.id, s.default_keys()))).collect();
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
                                if matches!(k.key.as_str(), "shift" | "control" | "alt" | "platform" | "function" | "") {
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
        ["Window", "Editor", "Terminal"]
            .into_iter()
            .map(|area| SettingGroup::new().title(area).items(SHORTCUTS.iter().filter(|s| s.area() == area).map(&item)))
            .collect()
    }

#[cfg(test)]
mod tests {
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
        for page in 0..8 {
            let handle = window(cx);
            cx.update_window(handle, |_, window, cx| {
                open(state::Settings::default(), page, window, cx, |_, _| {});
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
            current.agent = " mine ".into();
            let saved = saved.clone();
            open(current, 5, window, cx, move |settings, _| *saved.borrow_mut() = Some(settings));
            window.render_frame(cx);
            let before = window.find("key-row-recent").bounds();
            assert!(window.try_find("key-row-comment").is_none_or(|row| !row.visible()), "the last keys start below the fold");
            window.scroll("key-row-recent", ScrollDelta::Pixels(point(px(0.), px(-300.))), cx);
            let after = window.try_find("key-row-recent").map(|row| row.bounds());
            assert!(after.is_none_or(|after| after.top() < before.top()), "the page scrolled");
            window.click("settings-save", cx);
        })
        .unwrap();
        assert_eq!(saved.borrow().as_ref().map(|s| s.agent.as_str()), Some("mine"));
    }
}
