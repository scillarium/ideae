//! Where tabs are: two columns, each of an upper pane and, when a tab is put
//! there, a lower one. The left column's upper pane always is; the others
//! come with their first tab and go with their last.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Pane {
    pub right: bool,
    pub below: bool,
}

impl Pane {
    pub const MAIN: Pane = Pane { right: false, below: false };
    pub const ALL: [Pane; 4] = [
        Pane::MAIN,
        Pane { right: false, below: true },
        Pane { right: true, below: false },
        Pane { right: true, below: true },
    ];

    pub fn ix(self) -> usize {
        usize::from(self.right) * 2 + usize::from(self.below)
    }

    fn id(self) -> &'static str {
        ["pane-left", "pane-left-below", "pane-right", "pane-right-below"][self.ix()]
    }

    fn name(self) -> &'static str {
        ["the upper left pane", "the lower left pane", "the upper right pane", "the lower right pane"][self.ix()]
    }
}

/// Where the tabs go when a pane is left empty: the left column takes the
/// right one's, an upper pane the one below it.
pub(super) fn settle(panes: &mut [Pane]) {
    if !panes.iter().any(|p| !p.right) {
        panes.iter_mut().for_each(|p| p.right = false);
    }
    for right in [false, true] {
        if !panes.iter().any(|p| p.right == right && !p.below) {
            panes.iter_mut().filter(|p| p.right == right).for_each(|p| p.below = false);
        }
    }
}

/// The height of a pane's row of tabs.
const TAB_ROW: Pixels = px(33.);

impl TaskView {
    pub(super) fn has(&self, pane: Pane) -> bool {
        self.tabs.iter().any(|t| t.pane == pane)
    }

    /// The tabs of a pane, by index, in their order.
    pub(super) fn in_pane(&self, pane: Pane) -> Vec<usize> {
        (0..self.tabs.len()).filter(|&ix| self.tabs[ix].pane == pane).collect()
    }

    /// The id of the tab a pane shows.
    pub(super) fn shown_in(&self, pane: Pane) -> Option<usize> {
        self.active[pane.ix()]
    }

    pub(super) fn is_shown(&self, id: usize) -> bool {
        self.active.contains(&Some(id))
    }

    /// Keeps the panes consistent after tabs move or close: see [`settle`];
    /// each pane shows one of its own tabs, and the pane worked in is one
    /// that is there.
    pub(super) fn normalize(&mut self) {
        let mut panes: Vec<Pane> = self.tabs.iter().map(|t| t.pane).collect();
        settle(&mut panes);
        for (tab, pane) in self.tabs.iter_mut().zip(panes) {
            tab.pane = pane;
        }
        for pane in Pane::ALL {
            let shown = self.active[pane.ix()];
            if !shown.is_some_and(|id| self.tabs.iter().any(|t| t.id == id && t.pane == pane)) {
                self.active[pane.ix()] = self.tabs.iter().find(|t| t.pane == pane).map(|t| t.id);
            }
        }
        if !self.has(self.current) {
            let above = Pane { below: false, ..self.current };
            self.current = if self.has(above) { above } else { Pane::MAIN };
        }
        if self.zoomed.is_some_and(|pane| !self.has(pane)) {
            self.zoomed = Some(self.current);
        }
    }

    /// Moves a tab to a pane and shows it there, the pane coming if it was not.
    pub(super) fn move_to(&mut self, ix: usize, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(ix) else { return };
        tab.pane = pane;
        tab.preview = false;
        let id = tab.id;
        self.normalize();
        // Settled, it may be in another pane than asked: a column or a floor that closed.
        let pane = self.tabs.iter().find(|t| t.id == id).map_or(pane, |t| t.pane);
        self.active[pane.ix()] = Some(id);
        self.current = pane;
        self.sync_visibility(cx);
        self.show_diff_if_seen(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Where the menu of tab `ix` can move it: the other panes there are,
    /// and those its move opens: a lower pane under an upper one, the right
    /// column. Not one its own pane would close for, coming to the same.
    pub(super) fn moves_of(&self, ix: usize) -> Vec<(SharedString, Pane)> {
        let Some(tab) = self.tabs.get(ix) else { return Vec::new() };
        let from = tab.pane;
        let stays = |pane: Pane| self.tabs.iter().any(|t| t.id != tab.id && t.pane == pane);
        let mut moves = Vec::new();
        for pane in Pane::ALL.into_iter().filter(|&pane| pane != from) {
            let label: SharedString = if self.has(pane) {
                format!("Move to {}", pane.name()).into()
            } else if pane.below && stays(Pane { below: false, ..pane }) {
                if pane.right == from.right { "Split down".into() } else { format!("Move to {}", pane.name()).into() }
            } else if pane.right && !pane.below && self.tabs.iter().any(|t| t.id != tab.id && !t.pane.right) {
                "Split right".into()
            } else {
                continue;
            };
            moves.push((label, pane));
        }
        moves
    }

    /// A pane's tabs all go to the pane above, or from the right column's top
    /// to the left one's.
    pub(super) fn join_pane(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        let into = if pane.below { Pane { below: false, ..pane } } else { Pane::MAIN };
        for tab in self.tabs.iter_mut().filter(|t| t.pane == pane || !pane.below && pane.right && t.pane.right) {
            tab.pane = if tab.pane.below && !pane.below { Pane { right: false, below: true } } else { into };
        }
        self.current = into;
        self.normalize();
        self.sync_visibility(cx);
        self.show_diff_if_seen(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The zoomed pane, for the window to float over everything. While it is
    /// up, the task's own layout leaves that pane's slot empty: a view (a
    /// terminal above all) is drawn in one place only.
    pub fn render_zoomed(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pane = self.zoomed?;
        Some(
            div()
                .size_full()
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &ToggleZoom, window, cx| this.toggle_zoom_focused(window, cx)))
                .on_action(cx.listener(|this, _: &ShowOutline, window, cx| this.show_outline(window, cx)))
                .on_action(cx.listener(|this, _: &ShowBookmarks, window, cx| this.show_bookmarks(window, cx)))
                .on_action(cx.listener(|this, _: &ShowUsages, window, cx| this.show_usages_at_cursor(window, cx)))
                .on_action(cx.listener(|this, _: &SendToAgent, window, cx| this.send_editor_selection(window, cx)))
                .on_action(cx.listener(|this, _: &CopyReference, _, cx| this.copy_reference(cx)))
                .on_action(cx.listener(|this, _: &CopyFilePath, _, cx| this.copy_file_path(cx)))
                .on_action(cx.listener(|this, _: &ShowFileInFiles, _, cx| this.show_file_in_files(cx)))
                .child(self.render_pane(pane, cx))
                .into_any_element(),
        )
    }

    /// Focus mode for a pane, like Zed's: it takes the whole window, over the
    /// sidebars and the other panes. The same gesture brings everything back.
    /// Shift+Esc: focus mode for the pane you are in.
    pub fn toggle_zoom_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.current;
        self.toggle_zoom(pane, window, cx)
    }

    pub(super) fn toggle_zoom(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        let pane = if self.has(pane) { pane } else { Pane::MAIN };
        self.zoomed = if self.zoomed.is_some() { None } else { Some(pane) };
        self.current = pane;
        self.focus_active(window, cx);
        cx.emit(TaskEvent::Changed);
        cx.notify();
    }

    fn focus_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if self.current != pane && self.has(pane) {
            self.current = pane;
            self.autosave_unfocused(cx);
            cx.notify();
        }
    }

    pub(super) fn tab_content(&self, ix: Option<usize>, cx: &mut Context<Self>) -> AnyElement {
        match ix.and_then(|ix| self.tabs.get(ix)).map(|t| &t.kind) {
            Some(TabKind::Terminal(view)) => view.clone().into_any_element(),
            Some(TabKind::Chat(chat)) => chat.clone().into_any_element(),
            Some(TabKind::Asleep) => match ix {
                Some(ix) => self.render_asleep(ix, cx),
                None => div().into_any_element(),
            },
            Some(TabKind::Changes) => self.changes.clone().into_any_element(),
            Some(TabKind::History) => self.history.clone().into_any_element(),
            Some(TabKind::File(file)) => file.clone().into_any_element(),
            Some(TabKind::Browser(view)) => view.clone().into_any_element(),
            Some(TabKind::Binary(view)) => view.clone().into_any_element(),
            Some(TabKind::Diff(view)) => view.clone().into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// A tab dropped on a pane moves there (or just shows, if already there).
    /// A tab dragged onto another takes its place, in that one's pane. The
    /// task's agent stays first.
    pub(crate) fn reorder_tab(&mut self, tab_id: usize, onto: usize, window: &mut Window, cx: &mut Context<Self>) {
        if tab_id == onto {
            return;
        }
        let (Some(from), Some(to)) = (self.ix_of(tab_id), self.ix_of(onto)) else { return };
        if self.tabs[from].agent == Some(Who::Main) {
            return;
        }
        let pane = self.tabs[to].pane;
        if self.tabs[from].pane != pane {
            self.move_to(from, pane, window, cx);
        }
        let (Some(from), Some(to)) = (self.ix_of(tab_id), self.ix_of(onto)) else { return };
        let to = if self.tabs[to].agent == Some(Who::Main) && to < from { to + 1 } else { to };
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        cx.emit(TaskEvent::Changed);
        cx.notify();
    }

    fn drop_tab(&mut self, tab_id: usize, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.ix_of(tab_id) else { return };
        if self.tabs[ix].pane == pane {
            self.activate(ix, window, cx);
        } else {
            self.move_to(ix, pane, window, cx);
        }
    }

    /// The panes in their columns, each column split when it has a lower pane.
    pub(super) fn render_panes(&self, cx: &mut Context<Self>) -> AnyElement {
        let slot = |this: &Self, pane: Pane, cx: &mut Context<Self>| -> AnyElement {
            if this.zoomed == Some(pane) {
                div().size_full().into_any_element()
            } else {
                this.render_pane(pane, cx).into_any_element()
            }
        };
        let column = |this: &Self, right: bool, cx: &mut Context<Self>| -> AnyElement {
            let (upper, lower) = (Pane { right, below: false }, Pane { right, below: true });
            if this.has(lower) {
                gpui_kit::component::resizable::v_resizable(if right { "panes-right" } else { "panes-left" })
                    .child(resizable_panel().child(slot(this, upper, cx)))
                    .child(resizable_panel().child(slot(this, lower, cx)))
                    .into_any_element()
            } else {
                slot(this, upper, cx)
            }
        };
        if self.has(Pane { right: true, below: false }) {
            h_resizable("panes")
                .child(resizable_panel().child(column(self, false, cx)))
                .child(resizable_panel().child(column(self, true, cx)))
                .into_any_element()
        } else {
            column(self, false, cx)
        }
    }

    /// One pane: its own tab bar over the tab it shows.
    pub(super) fn render_pane(&self, pane: Pane, cx: &mut Context<Self>) -> impl IntoElement {
        let split = self.tabs.iter().any(|t| t.pane != Pane::MAIN);
        let focused = split && self.current == pane;
        let shown = self.shown_in(pane);
        let highlight = cx.theme().primary.opacity(0.18);
        // While a tab is dragged, a transparent layer over the pane takes the
        // drop and shows the highlight: content such as a terminal paints an
        // opaque background over anything set on the pane itself. Near the
        // right edge of a left pane, with no right column yet, the drop opens
        // one; near the bottom of an upper pane with nothing under it, a lower
        // pane.
        let drag = drag_active(cx);
        // A terminal takes dropped files itself, as paths typed in: over it
        // only the tab bar opens them.
        let over_terminal = shown.and_then(|id| self.ix_of(id)).is_some_and(|ix| matches!(self.tabs[ix].kind, TabKind::Terminal(_)));
        let right_zone = drag != Drag::None && !pane.right && !self.has(Pane { right: true, below: false });
        let down_zone = drag == Drag::Tab && !pane.below && !self.has(Pane { below: true, ..pane });
        let layer = drag == Drag::Tab || drag == Drag::Files && !over_terminal;
        let zone = |id: &'static str, to: Pane, label: &'static str, cx: &mut Context<Self>| {
            div()
                .id(id)
                .absolute()
                .border_color(cx.theme().primary)
                .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(highlight))
                .drag_over::<DraggedFiles>(move |style, _, _, _| style.bg(highlight))
                .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                    if let Some(ix) = this.ix_of(dragged.tab_id) {
                        this.move_to(ix, to, window, cx);
                    }
                }))
                .on_drop(cx.listener(move |this, files: &DraggedFiles, window, cx| this.drop_files(files, to, window, cx)))
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label)
        };
        // With the tabs listed in the side, a label over the pane instead of its row.
        let rowless = self.rowless();
        if !rowless {
            if let Some(TabKind::File(file)) = shown.and_then(|id| self.ix_of(id)).map(|ix| &self.tabs[ix].kind) {
                file.read(cx).label_room.set(px(0.));
            }
        }
        let top = if rowless { px(0.) } else { TAB_ROW };
        v_flex()
            .id(pane.id())
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| this.focus_pane(pane, cx)))
            .when(!rowless, |el| el.child(
                div()
                    .relative()
                    .border_t_2()
                    .border_color(if focused { cx.theme().primary } else { cx.theme().transparent })
                    .child(self.render_tabs(pane, cx))
                    .when(drag == Drag::Files && over_terminal, |bar| {
                        bar.child(
                            div()
                                .id("tab-bar-drop")
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .drag_over::<DraggedFiles>(move |style, _, _, _| style.bg(highlight))
                                .on_drop(cx.listener(move |this, files: &DraggedFiles, window, cx| this.drop_files(files, pane, window, cx))),
                        )
                    }),
            ))
            .children(shown.and_then(|id| self.ix_of(id)).and_then(|ix| self.render_question_bar(ix, cx)))
            .children(shown.and_then(|id| self.ix_of(id)).and_then(|ix| self.render_loop_bar(ix, cx)))
            .child(div().flex_1().min_h_0().child(self.tab_content(shown.and_then(|id| self.ix_of(id)), cx)))
            .when(rowless, |el| el.children(self.render_tab_label(pane, cx)))
            .when(layer, |el| {
                el.child(
                    div()
                        .id("drop-layer")
                        .absolute()
                        // Below the tabs: a tab dropped on another takes its place.
                        .top(top)
                        .left_0()
                        .h(if down_zone { relative(0.7) } else { relative(1.) })
                        .w(if right_zone { relative(0.7) } else { relative(1.) })
                        .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(highlight))
                        .drag_over::<DraggedFiles>(move |style, _, _, _| style.bg(highlight))
                        .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| this.drop_tab(dragged.tab_id, pane, window, cx)))
                        .on_drop(cx.listener(move |this, files: &DraggedFiles, window, cx| this.drop_files(files, pane, window, cx))),
                )
            })
            .when(right_zone, |el| {
                let label = if drag == Drag::Files { "Drop to open beside" } else { "Drop to split" };
                el.child(zone("split-zone", Pane { right: true, below: false }, label, cx).top(top).right_0().h_full().w(relative(0.3)).border_l_2())
            })
            .when(down_zone, |el| {
                el.child(
                    zone("split-down-zone", Pane { below: true, ..pane }, "Drop to split down", cx)
                        .bottom_0()
                        .left_0()
                        .h(relative(0.3))
                        .w(if right_zone { relative(0.7) } else { relative(1.) })
                        .border_t_2(),
                )
            })
    }

    /// Files dropped on a pane open there, kept; one open already moves over.
    fn drop_files(&mut self, files: &DraggedFiles, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if !files.opens {
            return;
        }
        for path in &files.paths {
            // A pane that is not there yet opens with the file.
            self.current = if self.has(pane) { pane } else { self.current };
            self.open_file_as(path.clone(), 0, 0, OpenAs::Kept, window, cx);
            let opened = self.tabs.iter().position(|t| match &t.kind {
                TabKind::File(f) => f.read(cx).path == *path,
                TabKind::Binary(b) => b.read(cx).path == *path,
                _ => false,
            });
            if let Some(ix) = opened.filter(|&ix| self.tabs[ix].pane != pane) {
                self.move_to(ix, pane, window, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{settle, Pane};

    const LT: Pane = Pane::MAIN;
    const LB: Pane = Pane { right: false, below: true };
    const RT: Pane = Pane { right: true, below: false };
    const RB: Pane = Pane { right: true, below: true };

    fn settled(mut panes: Vec<Pane>) -> Vec<Pane> {
        settle(&mut panes);
        panes
    }

    #[test]
    fn an_empty_pane_takes_the_tabs_of_the_one_after_it() {
        // Nothing moves while the panes that are needed hold tabs.
        assert_eq!(settled(vec![LT, LB, RT, RB]), vec![LT, LB, RT, RB]);
        assert_eq!(settled(vec![LT, LB]), vec![LT, LB]);
        // The upper pane emptied: the lower one comes up.
        assert_eq!(settled(vec![LB, LB, RT]), vec![LT, LT, RT]);
        assert_eq!(settled(vec![LT, RB]), vec![LT, RT]);
        // The left column emptied: the right one comes over, its floors kept.
        assert_eq!(settled(vec![RT, RB]), vec![LT, LB]);
        // Both at once: only a lower right pane left.
        assert_eq!(settled(vec![RB, RB]), vec![LT, LT]);
        assert_eq!(settled(vec![]), Vec::<Pane>::new());
    }
}
