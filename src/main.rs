// A release build on Windows opens no console window next to its own.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod agent;
mod agents;
mod archive;
mod browser;
mod crash;
mod build_watch;
#[cfg(unix)]
mod daemon;
mod desktop;
mod edit;
mod file_icons;
mod finder;
mod git;
mod graph;
mod indent;
mod grammars;
mod history;
mod ide;
mod keymap;
mod keys;
mod lsp;
mod lsp_install;
mod merged_branches;
mod commands;
mod menu;
mod naming;
mod palette;
mod pr;
mod procs;
mod project_config;
mod replace;
mod said;
mod search;
mod services;
mod stalls;
mod settings_ui;
mod sidebar;
mod sysmon;
mod state;
mod task;
mod trash;
mod theme;
mod update;
mod usage;
mod workspace;
mod zoom;

use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    resizable::{h_resizable, resizable_panel},
    h_flex, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use theme::Appearance;
use workspace::Workspace;

/// A window narrower than this has the sidebar as a rail, whatever it was
/// dragged to: the task keeps the room it needs.
const RAIL_WINDOW: Pixels = px(900.);

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus mode: the task's zoomed pane floats over the whole window, a
        // few pixels in from its edges so the rest still shows around it.
        let zoomed_pane = self.active_view().and_then(|task| task.update(cx, |t, cx| t.render_zoomed(cx)));
        let presenting = self.is_presenting();
        // Go to file and Find look in the task in view: none, nothing to show.
        let has_task = self.active_view().is_some();
        let shown = state::settings();
        // What the machine and the agents do is not for an audience.
        let quiet = presenting && shown.present_hide_readouts;
        let rail = self.sidebar_is_rail() || window.viewport_size().width < RAIL_WINDOW;
        let content = match (self.active_group.clone(), self.active_view()) {
            (Some(group), _) => self.render_group(&group, cx),
            // A variant has its siblings above it.
            (None, Some(view)) => v_flex().size_full().children(self.render_variant_strip(cx)).child(div().flex_1().min_h_0().child(view)).into_any_element(),
            (None, None) => self.render_empty(cx).into_any_element(),
        };
        v_flex()
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .child(
                TitleBar::new()
                    .on_close_window({
                        let this = cx.weak_entity();
                        move |_, _, cx| {
                            let _ = this.update(cx, |w, cx| w.close_window(cx));
                        }
                    })
                    .child(
                    h_flex()
                        .w_full()
                        .pr_4()
                        .justify_between()
                        .child(
                            h_flex()
                                .id("title-menu")
                                .gap_2()
                                .min_w_0()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(|_, _, cx| cx.stop_propagation())
                                // On macOS the menu bar takes its place; the rest is in the command palette.
                                .when(!cfg!(target_os = "macos"), |row| row.child(self.render_menu(cx))),
                        )
                        .child(self.render_title(cx))
                        .child(
                            // Clicks on these buttons are not the title bar's: a quick
                            // second one would zoom the window as a double click, and a
                            // slight move while pressed would start dragging it.
                            h_flex()
                                .id("title-actions")
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(|_, _, cx| cx.stop_propagation())
                                .gap_3()
                                .children(self.render_presenting(cx))
                                // Not read while saving power, so not shown: its numbers would be old.
                                // Nor while presenting: what the machine and the agents do is not for the audience.
                                .when(shown.show_load && !quiet && !lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed), |row| {
                                    row.child(div().mr_2().child(self.sysmon.clone()))
                                })
                                .when(shown.show_tokens && !quiet && self.tokens_of_the_day() > 0, |row| {
                                    row.child(
                                        div()
                                            .id("tokens-day")
                                            .test_support()
                                            .mr_2()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("tokens 24h {}", usage::tokens(self.tokens_of_the_day())))
                                            .tooltip({
                                                let mut told = "What the agents of the tasks open here took in the last 24 hours: input not read from the cache, and output".to_string();
                                                for (name, n) in self.tokens_by_agent() {
                                                    told.push_str(&format!("\n{name}: {}", usage::tokens(n)));
                                                }
                                                move |window, cx| gpui_kit::component::tooltip::Tooltip::new(told.clone()).build(window, cx)
                                            }),
                                    )
                                })
                                .children(self.render_update(cx).filter(|_| !presenting))
                                .children(self.render_new_build(cx).filter(|_| !presenting))
                                .child(self.render_power_saving(cx))
                                .when(has_task, |bar| bar.child(
                                    Button::new("go-to-file")
                                        .xsmall()
                                        .ghost()
                                        .label("Go to file")
                                        .tooltip(format!("Go to file  {}", keymap::label("go_to_file")))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.finder(finder::FinderKind::Files, window, cx)
                                        })),
                                ))
                                .when(has_task, |bar| bar.child(
                                    Button::new("find-in-files")
                                        .xsmall()
                                        .ghost()
                                        .label("Find")
                                        .tooltip(format!("Find in files  {}", keymap::label("find_in_files")))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.finder(finder::FinderKind::Grep, window, cx)
                                        })),
                                ))
                                .child(self.render_space_switch(cx))
                                .child(self.render_appearance(cx)),
                        ),
                ),
            )
            .child(
                div().relative().flex_1().min_h_0().child(
                    if rail {
                        // The tasks as a rail, the task next to it; the whole sidebar opens over the task for the pointer.
                        h_flex().size_full().child(self.render_rail(cx)).child(div().flex_1().min_w_0().h_full().child(content)).into_any_element()
                    } else {
                        h_resizable("workspace")
                            .on_resize({
                                let this = cx.weak_entity();
                                move |state, _, cx| {
                                    if let Some(width) = state.read(cx).sizes().first().copied() {
                                        let _ = this.update(cx, |w, cx| {
                                            // Dragged as narrow as it goes: it is a rail from here, and the width it had is kept for when it opens.
                                            if width < sidebar::RAIL_BELOW {
                                                w.sidebar_rail = true;
                                                cx.notify();
                                            } else {
                                                w.sidebar_width = width;
                                            }
                                        });
                                    }
                                }
                            })
                            .child(
                                resizable_panel()
                                    .size(self.sidebar_width)
                                    .size_range(px(100.)..px(520.))
                                    // Keeps its width when the window resizes.
                                    .fixed()
                                    .child(self.render_sidebar(cx)),
                            )
                            .child(resizable_panel().child(content))
                            .into_any_element()
                    },
                )
                // The whole sidebar over the task, while the pointer is on the rail or on it.
                .when(rail && self.sidebar_peek, |body| {
                    body.child(
                        div()
                            .id("sidebar-peek")
                            .absolute()
                            .top_0()
                            .left_0()
                            .bottom_0()
                            .w(self.sidebar_width.max(px(220.)))
                            .bg(cx.theme().background)
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .shadow_lg()
                            .occlude()
                            .test_support()
                            .on_hover(cx.listener(|this, hovered: &bool, window, cx| this.peek_sidebar(*hovered, window, cx)))
                            .child(
                                v_flex()
                                    .size_full()
                                    // The way back to a sidebar that stays; not while presenting, where the rail is the point.
                                    .when(!presenting, |peek| {
                                        peek.child(h_flex().flex_none().justify_end().px_2().pt_1().child(
                                            Button::new("sidebar-keep")
                                                .xsmall()
                                                .ghost()
                                                .label("keep open")
                                                .tooltip("The sidebar stays, as before it was dragged narrow")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    (this.sidebar_rail, this.sidebar_peek) = (false, false);
                                                    cx.notify();
                                                })),
                                        ))
                                    })
                                    .child(div().flex_1().min_h_0().child(self.render_sidebar(cx))),
                            ),
                    )
                })
            .when_some(zoomed_pane, |body, pane| {
                    body.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .p(px(10.))
                            .bg(gpui_kit::black().opacity(0.35))
                            // What is under it takes no hover and no click.
                            .occlude()
                            .child(
                                div()
                                    .size_full()
                                    .rounded_md()
                                    .overflow_hidden()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .shadow_lg()
                                    .bg(cx.theme().background)
                                    .child(pane),
                            ),
                    )
                }),
            )

            // Files dropped from a file manager anywhere but a terminal.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.open_dropped(paths.paths(), window, cx));
                }
            }))
            // Tab and panel keys, for the task in view.
            .on_action(cx.listener(|this, a: &task::ShowView, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.show_view(a.0, window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &task::NextTab, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.cycle_tab(true, window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &task::PrevTab, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.cycle_tab(false, window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &task::CloseTab, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.close_active(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &task::ReopenClosedTab, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.reopen_closed(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &task::NavigateBack, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.navigate(true, window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &task::NavigateForward, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.navigate(false, window, cx));
                }
            }))
            .on_action(cx.listener(|this, a: &task::ShowSidePanel, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.show_side_panel(a.0, window, cx));
                }
            }))
            // Reached from the sidebar and a zoomed pane, where the task's own is not.
            .on_action(cx.listener(|this, _: &task::ShowBranches, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.branches_dialog(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &GoToFile, window, cx| this.finder(finder::FinderKind::Files, window, cx)))
            .on_action(cx.listener(|this, _: &GoToSymbol, window, cx| this.finder(finder::FinderKind::Symbols, window, cx)))
            .on_action(cx.listener(|this, _: &SwitchTheme, window, cx| this.theme_picker(window, cx)))
            // Reached when nothing inside the task holds focus (a placeholder, a list).
            .on_action(cx.listener(|this, _: &task::ToggleZoom, window, cx| {
                if let Some(task) = this.active_view() {
                    task.update(cx, |t, cx| t.toggle_zoom_focused(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &FindInFiles, window, cx| this.finder(finder::FinderKind::Grep, window, cx)))
            .on_action(cx.listener(|this, _: &ReplaceInFiles, window, cx| this.replace_in_files(window, cx)))
            .on_action(cx.listener(|this, _: &Quit, window, cx| this.request_quit(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .on_action(cx.listener(|this, _: &palette::CommandPalette, window, cx| this.command_palette(window, cx)))
            .on_action(cx.listener(|this, _: &palette::RecentPlaces, window, cx| this.recent_tasks(window, cx)))
            .on_action(cx.listener(|this, _: &palette::NextWaiting, window, cx| this.next_waiting(window, cx)))
            .on_action(cx.listener(|this, _: &NewTask, window, cx| this.new_task(window, cx)))
    }
}

actions!(ideae, [GoToFile, GoToSymbol, FindInFiles, ReplaceInFiles, NewTask, Quit, OpenSettings, SwitchTheme]);

impl Workspace {
    pub fn new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(project) = self.project_in_hand().or(self.first_project()) {
            self.new_task_dialog(project, window, cx);
        }
    }

    /// Not while presenting: nothing is added to the window then.
    pub fn ask_for_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_presenting() {
            self.add_project_dialog(window, cx);
        }
    }

    pub fn toggle_presentation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_presenting() { self.end_presentation(window, cx) } else { self.presentation_dialog(window, cx) }
    }

    fn finder(&mut self, kind: finder::FinderKind, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(task) = self.active_view() {
            task.update(cx, |t, cx| t.open_finder(kind, window, cx));
        }
    }

    fn replace_in_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(task) = self.active_view() {
            task.update(cx, |t, cx| t.open_replace(window, cx));
        }
    }

    /// A newer release: one click downloads it and restarts.
    fn render_update(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let release = self.update.as_ref()?;
        let label = if self.updating {
            "↑ updating…".to_string()
        } else {
            format!("↑ update {}", update::when(&release.tag))
        };
        Some(
            Button::new("update")
                .xsmall()
                .ghost()
                .disabled(self.updating)
                .label(label)
                .text_color(cx.theme().primary)
                .tooltip(format!(
                    "Release {} is out. Click to download it ({} MB) and restart; the agents go on.",
                    release.tag,
                    release.size / (1 << 20)
                ))
                .on_click(cx.listener(|this, _, window, cx| this.install_update(window, cx))),
        )
    }

    /// The binary was rebuilt since the app started: a restart runs it.
    fn render_new_build(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let new = self.build.new.as_ref()?;
        let running = match self.build.running_built() {
            Some(built) => format!("{}, built {}", build_watch::version(), build_watch::clock(built)),
            None => build_watch::version(),
        };
        let tip = format!(
            "Running: {running}\nOn disk: {}, built {}\nClick to restart into it. Agents resume their conversations.",
            new.version,
            build_watch::clock(new.built)
        );
        Some(
            Button::new("new-build")
                .xsmall()
                .ghost()
                .label(format!("↻ new build {}", build_watch::clock(new.built)))
                .text_color(cx.theme().primary)
                .tooltip(tip)
                .on_click(cx.listener(|this, _, window, cx| this.restart(window, cx))),
        )
    }

    fn render_power_saving(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let saving = lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed);
        Button::new("power-saving")
            .xsmall()
            .ghost()
            .icon(gpui_kit::component::IconName::Battery)
            .when(saving, |b| b.text_color(hsla(0.11, 0.9, 0.55, 1.)))
            .when(!saving, |b| b.text_color(cx.theme().muted_foreground))
            .tooltip(if saving {
                "Power saving is on: no language servers, no background fetch, no pull request status, no load readout. Click to turn it off."
            } else {
                "Power saving: click to stop the language servers, the background fetch, the pull request status and the load readout."
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_power_saving(cx)))
    }

    /// One button: a click always changes what is seen, light to dark or
    /// back; a right click follows the system again.
    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.appearance;
        let dark = cx.theme().mode.is_dark();
        let other = if dark { Appearance::Light } else { Appearance::Dark };
        let shown = if dark { "☾" } else { "☀" };
        let tip = match current {
            Appearance::Auto => format!("{}, as the system is. Click: {}.", if dark { "Dark" } else { "Light" }, other.label().to_lowercase()),
            _ => format!("{}. Click: {}; right click: as the system is.", current.label(), other.label().to_lowercase()),
        };
        Button::new("appearance")
            .xsmall()
            .ghost()
            .label(shown)
            .tooltip(tip)
            .on_click(cx.listener(move |this, _, window, cx| this.set_appearance(other, window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, _, window, cx| this.set_appearance(Appearance::Auto, window, cx)))
    }

    /// The app's menu, in the order one comes to it: what to start, where
    /// to go, how the window is, the app itself.
    fn render_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
        let this = cx.weak_entity();
        let project = self.active_project();
        let pinned = self.pinned;
        let presenting = self.is_presenting();
        let saving = lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed);
        let zoomed = zoom::any_set(cx);
        Button::new("app-menu").xsmall().ghost().label("☰").dropdown_menu(move |menu, _, _| {
            // One line of the menu: its words, its keys at the right and dimmer
            // as in the command list, what it does.
            let entry = |menu: PopupMenu, label: &str, key: Option<&str>, act: fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
                let this = this.clone();
                let label = SharedString::from(label.to_string());
                let key = key.map(|key| SharedString::from(keymap::label(key))).filter(|k| !k.is_empty());
                let item = match key {
                    Some(key) => PopupMenuItem::element(move |_, cx| {
                        h_flex()
                            .w_full()
                            .gap_6()
                            .child(div().flex_1().child(label.clone()))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(key.clone()))
                    }),
                    None => PopupMenuItem::new(label),
                };
                menu.item(item.on_click(move |_, window, cx| {
                    let _ = this.update(cx, |w, cx| act(w, window, cx));
                }))
            };
            let mut menu = menu;
            // Not while presenting: nothing is added to the window by a stray click.
            if !presenting {
                if project.is_some() {
                    menu = entry(menu, "New task…", Some("new_task"), |w, window, cx| {
                        if let Some(project) = w.active_project() {
                            w.new_task_dialog(project, window, cx)
                        }
                    });
                }
                menu = entry(menu, "Add project…", None, |w, window, cx| w.add_project_dialog(window, cx)).separator();
            }
            menu = entry(menu, "Go to file…", Some("go_to_file"), |w, window, cx| w.finder(finder::FinderKind::Files, window, cx));
            menu = entry(menu, "Go to class or symbol…", Some("go_to_symbol"), |w, window, cx| w.finder(finder::FinderKind::Symbols, window, cx));
            menu = entry(menu, "Find in files…", Some("find_in_files"), |w, window, cx| w.finder(finder::FinderKind::Grep, window, cx));
            menu = entry(menu, "Replace in files…", Some("replace_in_files"), |w, window, cx| w.replace_in_files(window, cx));
            menu = entry(menu, "Run a command…", Some("command_palette"), |w, window, cx| w.command_palette(window, cx)).separator();
            menu = entry(menu, if presenting { "End presentation" } else { "Enter presentation mode…" }, None, |w, window, cx| w.toggle_presentation(window, cx));
            menu = entry(menu, "Theme…", Some("theme"), |w, window, cx| w.theme_picker(window, cx));
            if zoomed {
                menu = entry(menu, "Text of its usual size", None, |_, _, cx| zoom::reset_all(cx));
            }
            let power = this.clone();
            menu = menu
                .item(PopupMenuItem::new("Power save mode").checked(saving).on_click(move |_, _, cx| {
                    let _ = power.update(cx, |w, cx| w.toggle_power_saving(cx));
                }))
                .separator();
            menu = entry(menu, "Settings…", Some("settings"), |w, window, cx| w.open_settings(window, cx));
            menu = entry(menu, "Keyboard shortcuts…", None, |w, window, cx| w.open_settings_at(settings_ui::KEYS_PAGE, window, cx)).separator();
            if desktop::can_pin() {
                let pin = this.clone();
                menu = menu.item(PopupMenuItem::new(if pinned { "Unpin from dash" } else { "Pin to dash" }).on_click(move |_, window, cx| {
                    let result = if pinned { Ok(desktop::unpin()) } else { desktop::pin() };
                    match result {
                        Ok(()) => {
                            let _ = pin.update(cx, |w, cx| {
                                w.pinned = !pinned;
                                cx.notify();
                            });
                        }
                        Err(err) => window.push_notification(gpui_kit::component::notification::Notification::error(err).title("Pin to dash"), cx),
                    }
                }));
            }
            menu = entry(menu, "Restart", None, |w, window, cx| w.restart(window, cx));
            entry(menu, "Exit", Some("quit"), |w, window, cx| w.request_quit(window, cx))
        })
    }

    /// The project and task in view.
    fn render_title(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .active
            .as_ref()
            .and_then(|a| self.find(a))
            .map(|(pi, ti)| {
                let p = &self.projects[pi];
                let t = &p.tasks[ti];
                if t.main { format!("{} · {}", p.name, t.branch) } else { format!("{} · {}", p.name, t.label()) }
            })
            .unwrap_or_else(|| state::APP_ID.into());
        div().flex_1().min_w_0().px_3().truncate().text_sm().text_color(cx.theme().muted_foreground).child(title)
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let project = self.active_project().or(self.first_project());
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .text_color(cx.theme().muted_foreground)
            .child(if self.projects.is_empty() { "Add a project to start." } else { "Pick a task on the left, or start one." })
            .child(
                h_flex()
                    .gap_2()
                    .when(self.projects.is_empty(), |s| {
                        s.child(
                            Button::new("empty-add-project")
                                .small()
                                .outline()
                                .label("Add project…")
                                .on_click(cx.listener(|this, _, window, cx| this.add_project_dialog(window, cx))),
                        )
                    })
                    .when_some(project, |s, project| {
                        s.child(
                            Button::new("empty-main")
                                .small()
                                .outline()
                                .label("Open main thread")
                                .on_click(cx.listener(move |this, _, window, cx| this.open_main(project, window, cx))),
                        )
                        .child(
                            Button::new("empty-new-task")
                                .small()
                                .outline()
                                .label("New task…")
                                .on_click(cx.listener(move |this, _, window, cx| this.new_task_dialog(project, window, cx))),
                        )
                    }),
            )
    }
}

/// gpui-cn puts a macOS-style overlay scrollbar on the shared base theme
/// (hidden between scrolls, faint thumb), and every theme rebuild, such as the
/// one for the OS appearance after the window opens, puts it back.
fn visible_scrollbars(cx: &mut App) {
    use gpui_kit::base::{ScrollbarMode, ScrollbarStyles, ScrollbarTheme, Theme};
    if cx.global::<Theme>().scrollbar.mode() == ScrollbarMode::Always {
        return;
    }
    let fg = gpui_kit::component::Theme::global(cx).muted_foreground;
    cx.global_mut::<Theme>().scrollbar = ScrollbarTheme::new()
        .with_mode(ScrollbarMode::Always)
        .with_styles(
            ScrollbarStyles::default()
                .thumb(|s| s.bg(fg.opacity(0.5)))
                .thumb_hover(|s| s.bg(fg.opacity(0.8)))
                .thumb_active(|s| s.bg(fg.opacity(0.8))),
        );
}

fn main() {
    // Run by Claude Code hooks: forward the event and exit before any UI setup.
    let argv: Vec<String> = std::env::args().collect();
    // Before the file can change under a running process.
    let _ = build_watch::build_id();
    #[cfg(unix)]
    if argv.get(1).map(String::as_str) == Some("--daemon") {
        crash::install("daemon");
        daemon::server::run(argv.get(2).map(String::as_str) == Some("--adopt"));
        return;
    }
    if argv.get(1).map(String::as_str) == Some("--hook") {
        agent::run_hook(argv.get(2..).unwrap_or_default());
        return;
    }
    if let Some(what) = argv.get(1).filter(|a| *a == "--install" || *a == "--uninstall") {
        let done = if what == "--install" { desktop::install() } else { desktop::uninstall() };
        match done {
            Ok(told) => println!("{told}"),
            Err(e) => {
                eprintln!("ideae {what}: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let mut repos = Vec::new();
    // None: as Settings say; Some(None): --no-agent.
    let mut agent: Option<Option<String>> = None;
    let mut appearance: Option<Appearance> = None;
    let mut open_task: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--agent" => agent = Some(args.next()),
            "--no-agent" => agent = Some(None),
            "--task" => open_task = args.next().map(PathBuf::from),
            "--theme" => {
                appearance = args.next().and_then(|v| Appearance::parse(&v))
            }
            "--version" | "-V" => {
                println!("ideae {}", build_watch::version());
                return;
            }
            "--help" | "-h" => {
                println!("usage: ideae [--agent CMD | --no-agent] [--theme auto|light|dark] [--task PATH] [--version] [REPO...]\n       ideae --install | --uninstall   (list the app with the system's apps, or remove it)\nRepos join the project list, kept in {}.", state::data_dir().join("state.json").display());
                return;
            }
            _ => repos.push(PathBuf::from(arg)),
        }
    }
    crash::install("ideae");
    procs::adopt_shell_env();
    std::thread::spawn(desktop::refresh_entry);
    procs::catch_quit_signals();

    menu::before_launch();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_cn::init(cx);
            theme::init(cx);
            grammars::register();
            cx.bind_keys(gpui_cn::terminal::default_key_bindings());
            visible_scrollbars(cx);
            cx.observe_global::<gpui_kit::base::Theme>(visible_scrollbars).detach();
            menu::init(cx);
            keymap::bind_all(cx);
            menu::install(cx);
            stalls::watch(cx);

            // A saved window larger than the screen it opens on (another
            // monitor, another scale) shrinks to fit.
            let screen = cx.primary_display().map(|d| d.bounds());
            let saved = state::load();
            let window_bounds = saved
                .window
                .clone()
                .filter(|w| w.width >= 400. && w.height >= 300.)
                .map(|w| {
                    let (mut width, mut height) = (px(w.width), px(w.height));
                    let mut origin = point(px(w.x), px(w.y));
                    if let Some(screen) = screen {
                        width = width.min(screen.size.width);
                        height = height.min(screen.size.height);
                        origin.x = origin.x.max(screen.origin.x).min(screen.origin.x + screen.size.width - width);
                        origin.y = origin.y.max(screen.origin.y).min(screen.origin.y + screen.size.height - height);
                    }
                    let bounds = Bounds::new(origin, size(width, height));
                    if w.fullscreen {
                        WindowBounds::Fullscreen(bounds)
                    } else if w.maximized {
                        WindowBounds::Maximized(bounds)
                    } else {
                        WindowBounds::Windowed(bounds)
                    }
                })
                .unwrap_or_else(|| WindowBounds::centered(size(px(1500.), px(920.)), cx));
            let requested = match window_bounds {
                WindowBounds::Windowed(b) => Some(b.origin),
                _ => None,
            };
            // Debug aid for runs that are only looked at: IDEAE_BEHIND opens
            // the window without taking the keyboard or coming to the front.
            let behind = std::env::var_os("IDEAE_BEHIND").is_some();
            let options = WindowOptions {
                window_bounds: Some(window_bounds),
                app_id: Some(state::APP_ID.into()),
                focus: !behind,
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    let mut workspace = Workspace::with_state(saved, repos, agent, appearance, window, cx);
                    workspace.placed_at(requested);
                    cx.set_global(menu::Target { window: window.window_handle(), workspace: cx.weak_entity() });
                    // Again with the window's spaces and state.
                    cx.defer(menu::install);
                    if let Some(task) = &open_task {
                        workspace.focus_task(task, window, cx);
                    }
                    workspace
                })
            })
            .expect("failed to open window");
            if !behind {
                cx.activate(true);
            }
            // A quit signal (logout, kill) goes the same way as Exit.
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
                    if procs::quit_requested() {
                        let _ = cx.update(|cx| cx.quit());
                        break;
                    }
                }
            })
            .detach();
        });
}
