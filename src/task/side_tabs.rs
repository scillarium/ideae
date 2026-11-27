//! Tabs in the side (Settings → Tabs): the task's tabs one under another in
//! a panel of the side, by pane when split; and, while that panel is in view,
//! panes without a row of tabs, a small label at the top of each for what
//! it shows, over the end of a file's own header.

use super::*;

/// How tall a pane's label is; a file's header is as tall under it.
pub(super) const LABEL_HEIGHT: Pixels = px(22.);
/// How far from the pane's right edge: clear of a scrollbar.
const LABEL_RIGHT: Pixels = px(14.);

impl TaskView {
    /// The tabs as the Tabs panel lists them, with their pane: pane by pane,
    /// each pane's in its order.
    pub(super) fn listed_tabs(&self) -> Vec<(usize, Pane)> {
        Pane::ALL.into_iter().flat_map(|pane| self.in_pane(pane).into_iter().map(move |ix| (ix, pane))).collect()
    }

    /// The panes go without their rows of tabs: asked for, and the list is
    /// there to be seen. In focus mode, a narrow window, or with another
    /// panel in the Tabs panel's half, the rows are back.
    pub(super) fn rowless(&self) -> bool {
        self.zoomed.is_none()
            && self.panel_open(RightPane::Tabs)
            && self.side_in_view()
            && crate::state::with_settings(|s| s.tabs_in_side && s.no_tab_row)
    }

    /// The Tabs panel takes its half of the side when that half is empty.
    pub(super) fn tabs_to_side(&mut self, cx: &App) {
        if !crate::state::with_settings(|s| s.tabs_in_side) || self.panel_open(RightPane::Tabs) {
            return;
        }
        let half = usize::from(Self::below(RightPane::Tabs, cx));
        if self.side[half].is_none() {
            self.side[half] = Some(RightPane::Tabs);
        }
    }

    /// The setting turned on or off: the panel comes, or goes.
    pub fn tabs_panel_setting(&mut self, cx: &mut Context<Self>) {
        if crate::state::with_settings(|s| s.tabs_in_side) {
            self.tabs_to_side(cx);
            cx.notify();
        } else if self.panel_open(RightPane::Tabs) {
            self.close_panel(RightPane::Tabs, cx);
        }
    }

    /// A tab's icon and name, as the list and a pane's label show it.
    fn tab_face(&self, tab: &TabItem, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let drawn = |path: &'static str| svg().path(path).size(px(14.)).flex_none().text_color(muted).into_any_element();
        let label = self.tab_label(tab);
        let (icon, name): (AnyElement, StyledText) = match &tab.kind {
            TabKind::File(file) => {
                let mark = file.read(cx).path.strip_prefix(&self.root).ok().and_then(|path| self.tree_marks.get(path.to_string_lossy().as_ref()).copied());
                let icon = crate::file_icons::icon(crate::file_icons::file_kept(&tab.title), cx).into_any_element();
                (icon, marked_label(label, mark, false, cx))
            }
            TabKind::Binary(_) => (crate::file_icons::icon(crate::file_icons::file_kept(&tab.title), cx).into_any_element(), StyledText::new(label)),
            _ => {
                let path = match (&tab.kind, tab.agent) {
                    (_, Some(Who::Question(_))) => "side-icons/message-circle-question-mark.svg",
                    (_, Some(_)) => "side-icons/bot.svg",
                    (TabKind::Terminal(_), None) => "side-icons/square-terminal.svg",
                    (TabKind::Browser(_), _) => "side-icons/globe.svg",
                    (TabKind::Changes, _) => "side-icons/diff.svg",
                    (TabKind::History, _) => "side-icons/history.svg",
                    (TabKind::Diff(_), _) => "side-icons/git-compare.svg",
                    _ => "side-icons/square-terminal.svg",
                };
                (drawn(path), StyledText::new(label))
            }
        };
        let asleep = matches!(tab.kind, TabKind::Asleep);
        h_flex()
            .min_w_0()
            .gap_1p5()
            .child(icon)
            .child(div().min_w_0().truncate().when(tab.preview, |d| d.italic()).when(asleep, |d| d.text_color(muted)).child(name))
    }

    /// The Tabs panel: every tab of the task, a row each. A click shows it in
    /// its pane, the middle button closes it, dragged it goes where it is
    /// dropped: on another row (its pane too), on a pane, to a pane's edge.
    pub(super) fn render_tab_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let split = self.tabs.iter().any(|t| t.pane != Pane::MAIN);
        let keyboard = self.side_has_focus.get() && self.keyed == RightPane::Tabs;
        let mut list = v_flex().id("tab-list").test_support().flex_1().min_h_0().overflow_y_scroll().py_1().gap_px();
        let mut last = None;
        for (n, (ix, pane)) in self.listed_tabs().into_iter().enumerate() {
            if split && last != Some(pane) {
                last = Some(pane);
                let name = match (pane.right, pane.below) {
                    (false, false) => "LEFT",
                    (false, true) => "LEFT, BELOW",
                    (true, false) => "RIGHT",
                    (true, true) => "RIGHT, BELOW",
                };
                let color = if self.current == pane { cx.theme().primary } else { cx.theme().muted_foreground };
                list = list.child(div().px_3().pt_1p5().pb_0p5().text_xs().text_color(color).child(name));
            }
            list = list.child(self.render_listed_tab(ix, pane, split, keyboard && n == self.side_selected, cx));
        }
        list.into_any_element()
    }

    fn render_listed_tab(&self, ix: usize, pane: Pane, split: bool, selected: bool, cx: &mut Context<Self>) -> AnyElement {
        let tab = &self.tabs[ix];
        let id = tab.id;
        let shown = self.shown_in(pane) == Some(id);
        // The tab the keyboard is in, apart from those other panes show.
        let front = shown && (!split || self.current == pane);
        let accent = cx.theme().accent;
        let group = SharedString::from(format!("side-tab-{id}"));
        let menu_tab = self.menu_tab.clone();
        let closes = closable(tab);
        let row = h_flex()
            .id(("side-tab", id))
            .test_support()
            .group(group.clone())
            .relative()
            .h(px(24.))
            .mx_1()
            .px_2()
            .gap_1()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .when(front, |row| row.bg(accent))
            .when(shown && !front, |row| row.bg(accent.opacity(0.5)))
            .hover(move |row| row.bg(accent))
            .when(selected, |row| row.border_l_2().border_color(cx.theme().primary))
            .child(div().flex_1().min_w_0().child(self.tab_face(tab, cx)))
            .when(closes, |row| {
                row.child(
                    div().flex_none().invisible().group_hover(group, |cross| cross.visible()).child(
                        Button::new(("side-tab-close", id)).xsmall().ghost().label("×").on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if let Some(ix) = this.ix_of(id) {
                                this.close_tab(ix, window, cx)
                            }
                        })),
                    ),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(ix) = this.ix_of(id) {
                    this.activate(ix, window, cx)
                }
            }))
            .on_mouse_down(MouseButton::Right, move |_, _, _| menu_tab.set(Some(id)))
            .when(closes, |row| {
                row.on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if let Some(ix) = this.ix_of(id) {
                            this.close_tab(ix, window, cx)
                        }
                    }),
                )
            })
            .on_drag(DraggedTab { tab_id: id, title: self.tab_label(tab) }, |dragged, _, _, cx| {
                start_drag(Drag::Tab);
                cx.new(|_| dragged.clone())
            })
            .drag_over::<DraggedTab>(|style, _, _, cx| style.bg(cx.theme().accent))
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| this.reorder_tab(dragged.tab_id, id, window, cx)));
        row.context_menu(super::panels::tab_menu(cx.weak_entity(), self.menu_tab.clone(), pane)).into_any_element()
    }

    /// Over a pane without its row of tabs, at its upper right: what it
    /// shows. Middle-clicked it closes, right-clicked it has the tab's menu,
    /// dragged it is the tab.
    pub(super) fn render_tab_label(&self, pane: Pane, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = self.shown_in(pane)?;
        let tab = self.tabs.get(self.ix_of(id)?)?;
        let focused = self.tabs.iter().any(|t| t.pane != Pane::MAIN) && self.current == pane;
        let menu_tab = self.menu_tab.clone();
        let closes = closable(tab);
        let width = label_width(&self.tab_label(tab));
        // A file's header makes room for it at its end.
        if let TabKind::File(file) = &tab.kind {
            file.read(cx).label_room.set(width + LABEL_RIGHT + px(8.));
        }
        let label = div()
            .id(("pane-label", pane.ix()))
            .test_support()
            .absolute()
            .top_0()
            .right(LABEL_RIGHT)
            .w(width)
            .h(LABEL_HEIGHT)
            .flex()
            .items_center()
            .px_1p5()
            .rounded_b_md()
            .bg(cx.theme().tab_bar)
            .border_1()
            .border_t_0()
            .border_color(if focused { cx.theme().primary } else { cx.theme().border })
            .text_xs()
            .opacity(if focused { 0.9 } else { 0.6 })
            .hover(|label| label.opacity(1.))
            .child(self.tab_face(tab, cx))
            .on_mouse_down(MouseButton::Right, move |_, _, _| menu_tab.set(Some(id)))
            .when(closes, |label| {
                label.on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if let Some(ix) = this.ix_of(id) {
                            this.close_tab(ix, window, cx)
                        }
                    }),
                )
            })
            .on_drag(DraggedTab { tab_id: id, title: self.tab_label(tab) }, |dragged, _, _, cx| {
                start_drag(Drag::Tab);
                cx.new(|_| dragged.clone())
            });
        Some(label.context_menu(super::panels::tab_menu(cx.weak_entity(), self.menu_tab.clone(), pane)).into_any_element())
    }
}

/// The width of a label for `name`: its icon, its characters, its padding;
/// a long name is cut, at most this wide.
fn label_width(name: &str) -> Pixels {
    let chars = name.chars().count() as f32;
    px((14. + 6. + chars * 6.5 + 14.).min(320.))
}
