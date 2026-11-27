use super::*;
use gpui_kit::component::menu::PopupMenuItem;

impl TaskView {
    pub(super) fn render_tabs(&self, pane: Pane, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.shown_in(pane);
        let in_pane = self.in_pane(pane);
        let selected = in_pane.iter().position(|&ix| Some(self.tabs[ix].id) == shown).unwrap_or(0);
        let right = pane.right;
        // The tab in front to the right column, the one it opens when there is none.
        let to_right = shown.and_then(|id| self.ix_of(id)).and_then(|ix| {
            self.moves_of(ix).into_iter().find(|(_, to)| to.right && !to.below).map(|_| (ix, Pane { right: true, below: false }))
        });
        let map = in_pane.clone();
        let menu_tab = self.menu_tab.clone();
        let bar = TabBar::new(["tabs-left", "tabs-left-below", "tabs-right", "tabs-right-below"][pane.ix()])
            .selected_index(selected)
            .on_click(cx.listener(move |this, pos: &usize, window, cx| {
                let Some(&ix) = map.get(*pos) else { return };
                // The tab bar takes the clicks for itself, so a double click is
                // told apart here: the same tab again within 400 ms.
                let id = this.tabs[ix].id;
                let now = std::time::Instant::now();
                let double = this.last_tab_click.is_some_and(|(last, at)| last == id && now - at < Duration::from_millis(400));
                this.last_tab_click = Some((id, now));
                this.activate(ix, window, cx);
                if double {
                    this.last_tab_click = None;
                    // A preview tab double-clicked is kept, as in other editors.
                    if std::mem::take(&mut this.tabs[ix].preview) {
                        cx.notify();
                    } else {
                        this.toggle_zoom(pane, window, cx);
                    }
                }
            }))
            .suffix(
                h_flex()
                    .child(
                        Button::new("zoom")
                            .small()
                            .ghost()
                            .label(if self.zoomed.is_some() { "⤡" } else { "⤢" })
                            .selected(self.zoomed.is_some())
                            .tooltip(match crate::keymap::label("focus_mode") {
                                keys if keys.is_empty() => "Focus mode: this pane over everything (double-click a tab)".to_string(),
                                keys => format!("Focus mode: this pane over everything ({keys}, or double-click a tab)"),
                            })
                            .on_click(crate::learn::clicked("focus_mode", cx.listener(move |this, _, window, cx| this.toggle_zoom(pane, window, cx)))),
                    )
                    .child(
                        Button::new("new-terminal")
                            .small()
                            .ghost()
                            .label("+")
                            .tooltip("New terminal here")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.current = pane;
                                this.new_terminal(window, cx)
                            })),
                    )
                    .child({
                        let this = cx.entity().downgrade();
                        let root = self.root.clone();
                        Button::new("new-browser")
                            .small()
                            .ghost()
                            .child(svg().path("side-icons/globe.svg").size(px(14.)).text_color(cx.theme().muted_foreground))
                            .tooltip("New browser tab here")
                            .dropdown_menu(move |menu, _, _| {
                                let open = |user: bool| {
                                    let this = this.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        let _ = this.update(cx, |t, cx| {
                                            t.current = pane;
                                            if user { t.new_user_browser(window, cx) } else { t.new_browser(window, cx) }
                                        });
                                    }
                                };
                                let whose = crate::browser::user_mark(&root).name;
                                menu.item(PopupMenuItem::new("The task's browser: for its stands, driven by its agent too").on_click(open(false)))
                                    .item(PopupMenuItem::new(format!("Your own browser ({whose}): your logins, out of agents' reach")).on_click(open(true)))
                            })
                    })
                    .when(!right && !pane.below, |bar| {
                        bar.child(
                            Button::new("split")
                                .small()
                                .ghost()
                                .label("◫")
                                .disabled(to_right.is_none())
                                .tooltip("Move this tab to the right (or down, by dragging it to a pane's lower edge or from its right-click menu)")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some((ix, to)) = to_right {
                                        this.move_to(ix, to, window, cx)
                                    }
                                })),
                        )
                    })
                    .when(right || pane.below, |bar| {
                        let tip = if pane.below { "Join: move these tabs to the pane above" } else { "Join: move these tabs back to the left" };
                        bar.child(
                            Button::new("join")
                                .small()
                                .ghost()
                                .label("▣")
                                .tooltip(tip)
                                .on_click(cx.listener(move |this, _, window, cx| this.join_pane(pane, window, cx))),
                        )
                    }),
            )
            .children(in_pane.into_iter().map(|ix| (ix, &self.tabs[ix])).map(|(ix, tab)| {
                let label = self.tab_label(tab);
                let closable = closable(tab);
                let dragged = DraggedTab { tab_id: tab.id, title: label.clone() };
                let item = if tab.agent == Some(Who::Main) {
                    let root = self.root.clone();
                    let who = self.agent.session_ref();
                    let fill = self.context_fill;
                    let dialed = fill > 0. && crate::state::with_settings(|s| s.context_on_tab);
                    Tab::new().child(
                        h_flex()
                            .id("agent-usage")
                            .gap_1p5()
                            .child(label)
                            .when(dialed, |tab| tab.child(crate::sidebar::context_dial(fill, px(12.), cx)))
                            .tooltip(move |_, cx| cx.new(|cx| crate::usage::UsageTooltip::new(root.clone(), who.clone(), cx)).into()),
                    )
                } else if let TabKind::File(file) = &tab.kind {
                    // As in the Files panel: the icon of its kind, and its name in the color of what git knows of it.
                    let mark = file.read(cx).path.strip_prefix(&self.root).ok().and_then(|path| self.tree_marks.get(path.to_string_lossy().as_ref()).copied());
                    let name = match mark {
                        Some(_) => marked_label(label, mark, false, cx),
                        None => StyledText::new(label),
                    };
                    Tab::new().child(h_flex().gap_1p5().child(crate::file_icons::icon(crate::file_icons::file_kept(&tab.title), cx)).child(name)).when(tab.preview, |tab| tab.italic())
                } else if matches!(tab.agent, Some(Who::Question(_))) {
                    // Its own name, not what its terminal calls itself; what it does by a mark of its own.
                    let name: SharedString = tab.title.trim_start_matches("? ").to_string().into();
                    let asleep = matches!(tab.kind, TabKind::Asleep);
                    let color = cx.theme().muted_foreground;
                    let status = tab.agent.and_then(|who| self.agent_of(who)).and_then(|a| status_glyph(&a.status)).filter(|_| !asleep);
                    // As on the agent's tab, for whoever keeps the dial there: a question started
                    // from a long conversation is full from its first message.
                    let fill = match tab.agent {
                        Some(Who::Question(n)) => self.questions.iter().find(|q| q.id == n).map_or(0., |q| q.context_fill),
                        _ => 0.,
                    };
                    let dialed = fill > 0. && crate::state::with_settings(|s| s.context_on_tab);
                    let (root, who) = (self.root.clone(), tab.agent.and_then(|who| self.agent_of(who)).and_then(|a| a.session_ref()));
                    Tab::new().child(
                        h_flex()
                            .id(("question-usage", tab.id))
                            .tooltip(move |_, cx| cx.new(|cx| crate::usage::UsageTooltip::new(root.clone(), who.clone(), cx)).into())
                            .gap_1p5()
                            .child(svg().path("side-icons/message-circle-question-mark.svg").size(px(14.)).flex_none().text_color(color))
                            .children(status.map(|(glyph, mark)| div().text_color(mark).child(glyph)))
                            .child(div().when(asleep, |d| d.text_color(color)).child(name))
                            .when(dialed, |tab| tab.child(crate::sidebar::context_dial(fill, px(12.), cx))),
                    )
                } else if matches!(tab.kind, TabKind::Asleep) {
                    Tab::new().child(div().text_color(cx.theme().muted_foreground).child(label))
                } else {
                    Tab::new().label(label).when(tab.preview, |tab| tab.italic())
                };
                let id = tab.id;
                let menu_tab = self.menu_tab.clone();
                let item = item
                    .on_mouse_down(MouseButton::Right, move |_, _, _| menu_tab.set(Some(id)))
                    .when(closable, |tab| {
                        tab.on_mouse_down(
                            MouseButton::Middle,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(ix, window, cx)
                            }),
                        )
                    })
                    .on_drag(dragged, |dragged, _, _, cx| {
                        start_drag(Drag::Tab);
                        cx.new(|_| dragged.clone())
                    })
                    .drag_over::<DraggedTab>(|style, _, _, cx| style.bg(cx.theme().accent))
                    .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| this.reorder_tab(dragged.tab_id, id, window, cx)));
                if closable {
                    // The cross is there under the pointer only, over the tab's end: it keeps no room
                    // of its own, so the tabs are as wide as their names and the row is quieter.
                    // A tab is closed without it too: the middle button, its menu, the key.
                    let group = SharedString::from(format!("tab-{}", tab.id));
                    let ground = if self.is_shown(tab.id) { cx.theme().tab_active } else { cx.theme().tab_bar };
                    item.group(group.clone()).suffix(
                        h_flex()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .pl_1()
                            .pr_0p5()
                            .bg(ground)
                            .invisible()
                            .group_hover(group, |cross| cross.visible())
                            .child(Button::new(("close", ix)).xsmall().ghost().label("×").on_click(crate::learn::clicked("close_tab", cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(ix, window, cx)
                            })))),
                    )
                } else {
                    item
                }
            }));
        div().id(["tab-bar-left", "tab-bar-left-below", "tab-bar-right", "tab-bar-right-below"][pane.ix()]).w_full().child(bar).context_menu(tab_menu(cx.weak_entity(), menu_tab, pane))
    }

    /// What a tab is called in its row: the agent's status and letter, what a
    /// terminal or a page shows, a file's unsaved edits, a pin.
    pub(super) fn tab_label(&self, tab: &TabItem) -> SharedString {
        let mut label = tab.title.clone();
        if matches!(tab.kind, TabKind::Terminal(_)) && !tab.shown.is_empty() {
            label = format!("{}: {}", tab.title, short_title(&tab.shown)).into();
        }
        if tab.agent == Some(Who::Main) {
            // Which agent, when there are several.
            if let Some(letter) = self.agent_letter() {
                label = format!("[{letter}] {label}").into();
            }
        }
        if let Some((glyph, _)) = tab.agent.and_then(|who| self.agent_of(who)).and_then(|a| status_glyph(&a.status)) {
            label = format!("{glyph} {label}").into();
        }
        if matches!(tab.kind, TabKind::Browser(_)) && !tab.shown.is_empty() && tab.shown != "about:blank" {
            label = short_title(&tab.shown).into();
        }
        if matches!(tab.kind, TabKind::File(_)) {
            if tab.dirty {
                label = format!("● {label}").into();
            }
        }
        if tab.pinned {
            label = format!("⚑ {label}").into();
        }
        label
    }

    pub(super) fn render_structure(&self, cx: &mut Context<Self>) -> AnyElement {
        let file = self.structure_file.and_then(|id| {
            self.tabs.iter().find_map(|t| match &t.kind {
                TabKind::File(f) if t.id == id => Some((id, f.clone())),
                _ => None,
            })
        });
        let Some((id, file)) = file else {
            return div()
                .p_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Open a file to see its structure")
                .into_any_element();
        };
        let (path, cursor) = {
            let f = file.read(cx);
            (f.path.clone(), f.cursor_line)
        };
        self.refresh_structure(cx);
        let items = self.structure_items(cx);
        let keyboard = self.side_has_focus.get();
        // The innermost declaration the cursor is in: the last one starting at or above it.
        let current = at_cursor(&items, cursor);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        v_flex()
            .id("structure")
            .flex_1()
            .min_h_0()
            .child(div().px_3().py_1().text_xs().text_color(cx.theme().muted_foreground).child(name))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // Only the declarations in view are laid out: a file can have thousands.
                    .child(
                        uniform_list(
                            "declarations",
                            items.len(),
                            cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .map(|ix| {
                                        let item = &items[ix];
                                        let (file, line) = (file.clone(), item.line);
                                        // "function open" read slowly down a list: a colored
                                        // letter for the kind, the word in the tooltip.
                                        let name = item.name.as_str();
                                        let (mark, color) = crate::finder::kind_mark(&item.kind);
                                        let kind = SharedString::from(item.kind.clone());
                                        div()
                                            .id(("decl", ix))
                                            .w_full()
                                            .pl(px(12. + 14. * item.depth as f32))
                                            .pr_2()
                                            .py_0p5()
                                            .text_sm()
                                            .truncate()
                                            .cursor_pointer()
                                            .when(Some(ix) == current, |d| d.bg(cx.theme().accent))
                                            .when(keyboard && ix == this.side_selected, |d| d.border_l_2().border_color(cx.theme().primary))
                                            .hover(|d| d.bg(cx.theme().accent))
                                            .when(!kind.is_empty(), |d| {
                                                let kind = kind.clone();
                                                d.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(kind.clone()).build(window, cx))
                                            })
                                            .child(
                                                h_flex()
                                                    .gap_1p5()
                                                    .min_w_0()
                                                    .child(div().w(px(12.)).flex_none().text_xs().font_semibold().text_color(color).child(mark))
                                                    .child(div().min_w_0().truncate().child(name.to_string())),
                                            )
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                if let Some(ix) = this.tabs.iter().position(|t| t.id == id) {
                                                    this.activate(ix, window, cx);
                                                }
                                                file.update(cx, |f, cx| f.go_to_line(line, 0, window, cx));
                                            }))
                                            .context_menu({
                                                let name = name.to_string();
                                                move |menu, _, _| menu.item(crate::copying::item("Copy name", name.clone()))
                                            })
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .size_full()
                        .track_scroll(&self.structure_scroll),
                    )
                    .child(gpui_kit::component::scroll::Scrollbar::vertical(&self.structure_scroll)),
            )
            .into_any_element()
    }
}

/// The menu of a tab: the one the pointer pressed last (`menu_tab`), in `pane`.
pub(super) fn tab_menu(
    task: WeakEntity<TaskView>,
    menu_tab: std::rc::Rc<std::cell::Cell<Option<usize>>>,
    pane: Pane,
) -> impl Fn(gpui_kit::component::menu::PopupMenu, &mut Window, &mut Context<gpui_kit::component::menu::PopupMenu>) -> gpui_kit::component::menu::PopupMenu + 'static {
    move |menu, _, cx| {
        let Some(view) = task.upgrade() else { return menu };
        let clicked = menu_tab.take();
        let t = view.read(cx);
        let tab = clicked.and_then(|id| t.tabs.iter().find(|t| t.id == id));
        let reopen = !t.closed.is_empty();
        let moves = tab.and_then(|tab| t.ix_of(tab.id)).map(|ix| t.moves_of(ix)).unwrap_or_default();
        let (pinned, id) = (tab.is_some_and(|t| t.pinned), tab.map(|t| t.id));
        // Each shown only when it would close something.
        let from = tab.and_then(|tab| t.ix_of(tab.id)).unwrap_or(usize::MAX);
        let others = !t.closed_together(pane, |_, other| Some(other.id) != id, cx).is_empty();
        let to_right = !t.closed_together(pane, |ix, _| ix > from, cx).is_empty();
        let all = !t.closed_together(pane, |_, _| true, cx).is_empty();
        let close = tab.is_some_and(closable);
        // The task's own agent's tab stands for the task: it is renamed from there too.
        let renames = tab.is_some_and(|tab| tab.agent == Some(Who::Main));
        let paths = tab.and_then(|tab| match &tab.kind {
            TabKind::File(file) => {
                let path = file.read(cx).path.clone();
                let relative = path.strip_prefix(&t.root).unwrap_or(&path).display().to_string();
                Some((path, relative))
            }
            _ => None,
        });
        let act = |label: &'static str, f: fn(&mut TaskView, Option<usize>, Pane, &mut Window, &mut Context<TaskView>)| {
            let view = view.downgrade();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let _ = view.update(cx, |t, cx| f(t, id, pane, window, cx));
            })
        };
        let icon = crate::sidebar::menu_icon;
        let menu = match id {
            Some(_) => menu
                .when(renames, |menu| {
                    let view = view.downgrade();
                    menu.item(PopupMenuItem::new("Rename task…").icon(icon("pencil")).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |_, cx| cx.emit(TaskEvent::Rename));
                    }))
                    .separator()
                })
                .when(close, |menu| {
                    menu.item(act("Close", |t, id, _, window, cx| {
                        if let Some(ix) = id.and_then(|id| t.ix_of(id)) {
                            t.close_tab(ix, window, cx);
                            crate::learn::used("close_tab", window, cx);
                        }
                    }).icon(icon("x")))
                })
                .when(others, |menu| menu.item(act("Close others", |t, id, pane, window, cx| t.close_many(pane, |_, tab| Some(tab.id) != id, window, cx))))
                .when(to_right, |menu| {
                    menu.item(act("Close to the right", |t, id, pane, window, cx| {
                        let from = id.and_then(|id| t.ix_of(id)).unwrap_or(usize::MAX);
                        t.close_many(pane, |ix, _| ix > from, window, cx)
                    }))
                })
                .when(all, |menu| menu.item(act("Close all", |t, _, pane, window, cx| t.close_many(pane, |_, _| true, window, cx))))
                .when(close || others || to_right || all, |menu| menu.separator())
                .when_some(paths, |menu, (path, relative)| {
                    let (shown, revealed) = (path.clone(), path.clone());
                    let view = view.downgrade();
                    menu.item(crate::copying::item("Copy path", path.display().to_string()).icon(icon("copy")))
                        .item(crate::copying::item("Copy relative path", relative))
                        .item(PopupMenuItem::new("Show in Files").icon(icon("locate-fixed")).on_click(move |_, _, cx| {
                            let _ = view.update(cx, |t, cx| t.show_in_files(&shown, cx));
                        }))
                        .item(PopupMenuItem::new("Reveal in file manager").icon(icon("folder-open")).on_click(move |_, _, cx| {
                            super::file_tree::reveal(revealed.clone(), false, cx)
                        }))
                        .separator()
                })
                .item(act(if pinned { "Unpin" } else { "Pin" }, |t, id, _, _, cx| {
                    if let Some(id) = id {
                        t.toggle_pin(id, cx)
                    }
                }).icon(icon(if pinned { "pin-off" } else { "pin" })))
                .map(|mut menu| {
                    for (label, to) in moves {
                        let view = view.downgrade();
                        let mark = match label.as_ref() {
                            "Split down" => Some(icon("rows-2")),
                            "Split right" => Some(icon("columns-2")),
                            _ => None,
                        };
                        menu = menu.item(PopupMenuItem::new(label).when_some(mark, |item, mark| item.icon(mark)).on_click(move |_, window, cx| {
                            let _ = view.update(cx, |t, cx| {
                                if let Some(ix) = id.and_then(|id| t.ix_of(id)) {
                                    t.move_to(ix, to, window, cx)
                                }
                            });
                        }));
                    }
                    menu
                }),
            None => menu.when(all, |menu| menu.item(act("Close all", |t, _, pane, window, cx| t.close_many(pane, |_, _| true, window, cx)))),
        };
        menu.when(reopen, |menu| menu.separator().item(act("Reopen closed tab", |t, _, _, window, cx| t.reopen_closed(window, cx)).icon(icon("rotate-ccw"))))
    }
}
