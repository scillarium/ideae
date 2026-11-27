use super::*;

// ---------------------------------------------------------------- the side

/// The panels of the side, in the order of their keys.
pub(super) const PANES: [RightPane; 7] = [RightPane::Changes, RightPane::Files, RightPane::Structure, RightPane::Run, RightPane::Database, RightPane::Http, RightPane::Tabs];

/// How wide the strip of their marks is, at the window's edge.
const STRIP: f32 = 38.;

/// Which panels are of the lower half of the side (a bit each, by their
/// place in `PANES`), and how high that half is: the same in every task.
#[derive(Clone, Copy)]
pub struct SideLayout {
    pub below: u8,
    pub split: Option<Pixels>,
}
impl Global for SideLayout {}

impl SideLayout {
    /// `below` with the Tabs panel's bit as given.
    pub fn with_tabs(below: u8, tabs_below: bool) -> u8 {
        let bit = 1 << RightPane::Tabs as u8;
        if tabs_below { below | bit } else { below & !bit }
    }

    pub fn tabs_below(&self) -> bool {
        self.below & 1 << RightPane::Tabs as u8 != 0
    }
}

/// The app's own drawings, over the kit's: the marks of the side's strip.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        macro_rules! ours {
            ($($name:literal),* $(,)?) => {
                match path {
                    $(concat!("side-icons/", $name, ".svg") => include_bytes!(concat!("../../assets/side-icons/", $name, ".svg")).as_slice(),)*
                    _ => return gpui_kit::assets::Assets.load(path),
                }
            };
        }
        let ours: &[u8] = ours!(
            "diff", "folder", "list-tree", "play", "locate-fixed", "chevrons-down-up", "folder-tree", "folder-plus",
            "plus", "globe", "history", "undo-2", "arrow-left", "arrow-right", "rotate-cw", "x",
            "square-dashed-mouse-pointer", "code-xml", "monitor-smartphone", "triangle-alert", "arrow-right-left",
            "lock-open", "variable", "arrow-down-a-z", "house", "square-plus", "archive-restore", "archive",
            "git-branch-minus", "settings", "message-circle-question-mark", "copy", "folder-open", "square-terminal",
            "pencil", "trash", "file-plus", "pin", "pin-off", "columns-2", "rows-2", "pause", "star", "star-off",
            "external-link", "rotate-ccw", "git-compare", "eye", "clock", "bot", "git-fork", "minus", "scissors", "clipboard-paste", "lightbulb", "send", "bookmark", "wand-sparkles", "database", "table", "layers",
        );
        Ok(Some(ours.into()))
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}

impl Default for SideLayout {
    /// Changes and Files above, Structure, Run and Tabs below.
    fn default() -> Self {
        Self { below: 1 << RightPane::Structure as u8 | 1 << RightPane::Run as u8 | 1 << RightPane::Tabs as u8, split: None }
    }
}

/// A panel's mark, taken by the pointer to the other half of the strip.
#[derive(Clone)]
struct DraggedMark(RightPane);

impl Render for DraggedMark {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1p5()
            .px_2()
            .py_1()
            .text_sm()
            .rounded_md()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().primary)
            .child(self.0.mark(cx.theme().foreground))
            .child(self.0.name())
    }
}

impl RightPane {
    /// The shortcut that shows it (Alt+1 to 5), told by Learn the keys.
    pub(super) fn shortcut(self) -> &'static str {
        match self {
            RightPane::Changes => "changes_panel",
            RightPane::Files => "files_panel",
            RightPane::Structure => "structure_panel",
            RightPane::Run => "run_panel",
            RightPane::Database => "database_panel",
            RightPane::Http => "http_panel",
            RightPane::Tabs => "tabs_panel",
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            RightPane::Changes => "Changes",
            RightPane::Files => "Files",
            RightPane::Structure => "Structure",
            RightPane::Run => "Run",
            RightPane::Database => "Database",
            RightPane::Http => "HTTP",
            RightPane::Tabs => "Tabs",
        }
    }

    /// Its mark: a drawing of Lucide's (assets/side-icons), in the color given.
    fn mark(self, color: Hsla) -> impl IntoElement {
        let drawing = match self {
            RightPane::Changes => "side-icons/diff.svg",
            RightPane::Files => "side-icons/folder.svg",
            RightPane::Structure => "side-icons/list-tree.svg",
            RightPane::Run => "side-icons/play.svg",
            RightPane::Database => "side-icons/database.svg",
            RightPane::Http => "side-icons/send.svg",
            RightPane::Tabs => "side-icons/layers.svg",
        };
        svg().path(drawing).size(px(18.)).flex_none().text_color(color)
    }

    fn key(self) -> &'static str {
        match self {
            RightPane::Changes => "changes_panel",
            RightPane::Files => "files_panel",
            RightPane::Structure => "structure_panel",
            RightPane::Run => "run_panel",
            RightPane::Database => "database_panel",
            RightPane::Http => "http_panel",
            RightPane::Tabs => "tabs_panel",
        }
    }
}

impl TaskView {
    pub(super) fn below(pane: RightPane, cx: &App) -> bool {
        cx.try_global::<SideLayout>().copied().unwrap_or_default().below & (1 << pane as u8) != 0
    }

    /// Whether the panel is one of the two the side holds.
    pub(super) fn panel_open(&self, pane: RightPane) -> bool {
        self.side.contains(&Some(pane))
    }

    /// Whether the side's panels are there to be seen: in a narrow window
    /// they are over the task, on asking.
    pub(super) fn side_in_view(&self) -> bool {
        self.side.iter().any(Option::is_some) && (!self.narrow || self.side_over)
    }

    /// Whether the panel is drawn: held by the side, or come over the task
    /// from its mark.
    pub(super) fn panel_shown(&self, pane: RightPane) -> bool {
        self.side_peek == Some(pane) || (self.panel_open(pane) && self.side_in_view())
    }

    /// What a panel needs when it comes into view.
    fn panel_came(&mut self, pane: RightPane, cx: &mut Context<Self>) {
        self.fill_tree(cx);
        if pane == RightPane::Files && crate::state::with_settings(|s| s.tree_follows) {
            self.locate_in_tree(cx);
        }
        if pane == RightPane::Run {
            self.refresh_services(cx);
            self.watch_listening(cx);
        }
    }

    /// The panel takes its half of the side, in place of the one there.
    pub(super) fn open_panel(&mut self, pane: RightPane, cx: &mut Context<Self>) {
        let half = usize::from(Self::below(pane, cx));
        self.side[half] = Some(pane);
        self.side[1 - half] = self.side[1 - half].filter(|other| *other != pane);
        (self.side_peek, self.side_peek_wait) = (None, Task::ready(()));
        self.panel_came(pane, cx);
        cx.notify();
    }

    pub(super) fn close_panel(&mut self, pane: RightPane, cx: &mut Context<Self>) {
        for half in &mut self.side {
            *half = half.filter(|open| *open != pane);
        }
        // The tabs come back to the half another panel leaves.
        if pane != RightPane::Tabs {
            self.tabs_to_side(cx);
        }
        if self.side == [None, None] {
            self.side_over = false;
        }
        cx.notify();
    }

    /// A click on a panel's mark: there, it goes; not there, it comes.
    fn toggle_panel(&mut self, pane: RightPane, cx: &mut Context<Self>) {
        if self.panel_open(pane) && self.side_in_view() {
            self.close_panel(pane, cx);
        } else {
            self.side_over = true;
            self.open_panel(pane, cx);
        }
    }

    /// A panel is of the upper half or the lower in every task: one that
    /// was moved while this task was out of view goes where it belongs.
    pub(super) fn settle_side(&mut self, cx: &App) {
        for half in 0..2 {
            let Some(pane) = self.side[half].filter(|pane| usize::from(Self::below(*pane, cx)) != half) else { continue };
            self.side[half] = None;
            if self.side[1 - half].is_none() {
                self.side[1 - half] = Some(pane);
            }
        }
    }

    pub(super) fn move_panel(&mut self, pane: RightPane, below: bool, cx: &mut Context<Self>) {
        let mut layout = cx.try_global::<SideLayout>().copied().unwrap_or_default();
        layout.below = if below { layout.below | 1 << pane as u8 } else { layout.below & !(1 << pane as u8) };
        cx.set_global(layout);
        // Moved, it is shown where it went.
        self.side_over = true;
        self.open_panel(pane, cx);
        cx.refresh_windows();
    }

    /// The pointer came to a panel's mark, or left it and the panel: after
    /// a while the panel is over the task, or gone from there.
    pub(super) fn peek_panel(&mut self, want: Option<RightPane>, window: &mut Window, cx: &mut Context<Self>) {
        // Over the task only while the side holds nothing in view: a panel
        // put there by a click is not taken over by one the pointer passed.
        let want = want.filter(|_| !self.side_in_view());
        let rest = crate::state::with_settings(|s| s.rail_open_ms);
        // Set never to open: the marks are for clicking.
        if want == self.side_peek || (want.is_some() && rest == 0) {
            self.side_peek_wait = Task::ready(());
            return;
        }
        let wait = std::time::Duration::from_millis(if want.is_some() { u64::from(rest) } else { 300 });
        self.side_peek_wait = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(wait).await;
            // A row's menu is over the task, not over the panel: the pointer on it has not left.
            if want.is_none() {
                crate::sidebar::menu_waited_out(cx).await;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                // Not taken from under the keyboard: a commit message is being written there.
                if want.is_none() && this.peek_focus.contains_focused(window, cx) {
                    return;
                }
                this.side_peek = want;
                if let Some(pane) = want {
                    this.panel_came(pane, cx);
                }
                cx.notify();
            });
        });
    }

    /// The strip at the window's edge: a mark for each panel, those of the
    /// upper half above and those of the lower below. A click shows or
    /// hides a panel, the pointer resting on a mark brings it over the task.
    pub(super) fn render_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // Each half takes a mark dropped on it: dragged there, a panel is of that half.
        let half = |below: bool, cx: &mut Context<Self>| {
            let over = cx.theme().accent;
            v_flex()
                .id(if below { "side-strip-below" } else { "side-strip-above" })
                .w_full()
                .flex_1()
                .min_h_0()
                .py_1()
                .gap_1()
                .items_center()
                .when(below, |half| half.justify_end())
                .drag_over::<DraggedMark>(move |half, mark, _, cx| if TaskView::below(mark.0, cx) != below { half.bg(over) } else { half })
                .on_drop(cx.listener(move |this, mark: &DraggedMark, _, cx| {
                    if Self::below(mark.0, cx) != below {
                        this.move_panel(mark.0, below, cx);
                    }
                }))
        };
        let mut halves = [half(false, cx), half(true, cx)];
        let tabs_mark = crate::state::with_settings(|s| s.tabs_in_side);
        for pane in PANES.into_iter().filter(|pane| *pane != RightPane::Tabs || tabs_mark) {
            let shown = self.panel_shown(pane);
            let below = Self::below(pane, cx);
            // What can be told without the panel: how many files changed (green when some are staged), how many services run.
            let told = match pane {
                RightPane::Changes if !self.files.is_empty() => Some((self.files.len().to_string(), if self.staged_paths().is_empty() { muted } else { gpui_kit::green() })),
                RightPane::Run => Some(self.running_services(cx).len()).filter(|running| *running > 0).map(|running| (running.to_string(), gpui_kit::green())),
                _ => None,
            };
            let tip = SharedString::from(format!("{}  {}", pane.name(), crate::keymap::label(pane.key())));
            let task = cx.weak_entity();
            let mark = div()
                .id(("side-mark", pane as usize))
                .test_support()
                .w(px(30.))
                .py_1p5()
                .gap_0p5()
                .rounded_md()
                .flex()
                .flex_col()
                .items_center()
                .cursor_pointer()
                .when(shown, |mark| mark.bg(cx.theme().accent))
                .hover(|mark| mark.bg(cx.theme().accent))
                .child(pane.mark(if shown { cx.theme().foreground } else { muted }))
                // Over the drawing's corner, so a mark is as tall with a count as without.
                .when_some(told, |mark, (told, color)| {
                    mark.relative().child(
                        div()
                            .absolute()
                            .right(px(1.))
                            .bottom(px(1.))
                            .min_w(px(12.))
                            .px(px(2.))
                            .rounded(px(6.))
                            .bg(color)
                            .text_color(cx.theme().background)
                            .text_size(px(9.))
                            .line_height(px(12.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .flex()
                            .justify_center()
                            .child(told),
                    )
                })
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                .on_hover(cx.listener(move |this, hovered: &bool, window, cx| this.peek_panel(hovered.then_some(pane), window, cx)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.toggle_panel(pane, cx);
                    crate::learn::used(pane.shortcut(), window, cx);
                }))
                .on_drag(DraggedMark(pane), |mark, _, _, cx| cx.new(|_| mark.clone()))
                .context_menu(move |menu, _, _| {
                    let task = task.clone();
                    let to = if below { "Move to the upper half" } else { "Move to the lower half" };
                    // The same as dragging the mark there.
                    menu.item(gpui_kit::component::menu::PopupMenuItem::new(to).on_click(move |_, _, cx| {
                        let _ = task.update(cx, |task, cx| task.move_panel(pane, !below, cx));
                    }))
                });
            let at = usize::from(below);
            halves[at] = std::mem::replace(&mut halves[at], div().id("moved").flex().flex_col()).child(mark);
        }
        let [above, below] = halves;
        v_flex()
            .id("side-strip")
            .w(px(STRIP))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(above)
            .child(below)
    }

    /// What a panel's title has to act on it: the buttons before its ×.
    fn panel_tools(&self, pane: RightPane, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let muted = cx.theme().muted_foreground;
        let drawn = move |id: &'static str, drawing: &'static str, tip: &'static str| Button::new(id).xsmall().ghost().child(svg().path(drawing).size(px(14.)).text_color(muted)).tooltip(tip);
        // A switch kept in the settings: the same in every task, and after a restart.
        let (lit, dim) = (cx.theme().foreground, cx.theme().muted_foreground);
        let kept = |id: &'static str, drawing: &'static str, tip: &'static str, on: bool, set: fn(&mut crate::state::Settings, bool), cx: &mut Context<Self>| {
            Button::new(id)
                .xsmall()
                .ghost()
                .child(svg().path(drawing).size(px(14.)).text_color(if on { lit } else { dim }))
                .selected(on)
                .tooltip(tip)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut settings = crate::state::settings();
                    set(&mut settings, !on);
                    crate::state::set_settings(settings);
                    if id == "tree-follow" && !on {
                        this.locate_in_tree(cx);
                    }
                    if id == "changes-tree" {
                        this.reorder_diffs(cx);
                    }
                    cx.refresh_windows();
                }))
                .into_any_element()
        };
        let settings = crate::state::with_settings(|s| (s.changes_flat, s.tree_follows, s.structure_public, s.structure_no_fields, s.structure_sorted));
        match pane {
            // With files in one state only, the one button for all of them is here, not on a line of its own.
            RightPane::Changes => {
                let staged = self.files.iter().filter(|f| self.state_of(&f.path).staged).count();
                let unstaged = self.files.iter().filter(|f| self.state_of(&f.path).unstaged).count();
                let all = |stage: bool, cx: &mut Context<Self>| {
                    Button::new(if stage { "stage-all" } else { "unstage-all" })
                        .xsmall()
                        .ghost()
                        .label(if stage { "Stage all" } else { "Unstage all" })
                        .on_click(cx.listener(move |this, _, window, cx| this.stage_all(stage, window, cx)))
                        .into_any_element()
                };
                let tree = kept("changes-tree", "side-icons/folder-tree.svg", "As a tree of folders, here and beside a diff", !settings.0, |s, on| s.changes_flat = !on, cx);
                match (staged, unstaged) {
                    (0, 1..) => vec![tree, all(true, cx)],
                    (1.., 0) => vec![tree, all(false, cx)],
                    (0, 0) => Vec::new(),
                    _ => vec![tree],
                }
            }
            RightPane::Files => vec![
                kept("tree-follow", "side-icons/arrow-right-left.svg", "Follow the file in the editor", settings.1, |s, on| s.tree_follows = on, cx),
                drawn("tree-locate", "side-icons/locate-fixed.svg", "Show the file in the editor")
                    .on_click(cx.listener(|this, _, _, cx| this.locate_in_tree(cx)))
                    .into_any_element(),
                drawn("tree-collapse", "side-icons/chevrons-down-up.svg", "Close every folder")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.expanded.clear();
                        this.tree_stale = true;
                        this.fill_tree(cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            ],
            RightPane::Structure => vec![
                kept("structure-public", "side-icons/lock-open.svg", "Only what is public", settings.2, |s, on| s.structure_public = on, cx),
                kept("structure-fields", "side-icons/variable.svg", "Fields, constants and variables too", !settings.3, |s, on| s.structure_no_fields = !on, cx),
                kept("structure-sorted", "side-icons/arrow-down-a-z.svg", "By name, not as in the file", settings.4, |s, on| s.structure_sorted = on, cx),
            ],
            RightPane::Run => Vec::new(),
            RightPane::Database => self.db_panel_tools(cx),
            RightPane::Http => self.http_panel_tools(cx),
            RightPane::Tabs => Vec::new(),
        }
    }

    /// The Files panel opens the folders down to the file in the editor,
    /// and has it chosen and in view.
    pub(super) fn locate_in_tree(&mut self, cx: &mut Context<Self>) {
        let Some((_, file)) = self.structure_target() else { return };
        let path = file.read(cx).path.clone();
        self.locate_path(&path, cx);
    }

    /// Show in Files: the panel comes, with the file or folder chosen in it.
    pub(super) fn show_in_files(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        self.side_over = true;
        self.open_panel(RightPane::Files, cx);
        self.locate_path(path, cx);
    }

    fn locate_path(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let Ok(relative) = path.strip_prefix(&self.root).map(|path| path.to_string_lossy().replace('\\', "/")) else { return };
        for (slash, _) in relative.match_indices('/') {
            self.expanded.insert(format!("{}/", &relative[..slash]).into());
        }
        self.tree_stale = true;
        self.fill_tree(cx);
        let found = gpui_kit::component::tree::TreeItem::new(relative.clone(), relative);
        self.tree.update(cx, |tree, cx| {
            // Chosen already, it was opened from here: the tree stays as it is under the pointer.
            if tree.selected_item().is_some_and(|chosen| chosen.id == found.id) {
                return;
            }
            tree.set_selected_item(Some(&found), cx);
            if let Some(ix) = tree.selected_index() {
                tree.scroll_to_item(ix, ScrollStrategy::Center);
            }
        });
        cx.notify();
    }

    /// One panel: its name, what acts on it, the way to put it away, and
    /// what it shows.
    fn render_panel(&self, pane: RightPane, cx: &mut Context<Self>) -> AnyElement {
        let name = match pane {
            RightPane::Changes if !self.files.is_empty() => {
                let viewed = (0..self.files.len()).filter(|&ix| self.is_viewed(ix)).count();
                if viewed > 0 { format!("CHANGES ({}) · {viewed} viewed", self.files.len()) } else { format!("CHANGES ({})", self.files.len()) }
            }
            _ => pane.name().to_uppercase(),
        };
        let title = h_flex()
            .flex_none()
            .h(px(26.))
            .pl_3()
            .pr_1()
            .gap_1()
            // The ground of the tab bar: a panel's title is told from what the panel above ends with.
            .bg(cx.theme().tab_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(div().flex_1().min_w_0().truncate().child(name))
            .children(self.panel_tools(pane, cx))
            .child(Button::new(("side-hide", pane as usize)).xsmall().ghost().label("×").tooltip("Hide the panel").on_click(cx.listener(move |this, _, _, cx| {
                if this.side_peek == Some(pane) {
                    this.side_peek = None;
                    cx.notify();
                } else {
                    this.close_panel(pane, cx);
                }
            })));
        let panel = v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            // The arrows go to the panel last clicked in.
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                if this.keyed != pane {
                    this.keyed = pane;
                    cx.notify();
                }
            }))
            .child(title);
        match pane {
            RightPane::Run => panel.child(self.render_run_panel(cx)),
            RightPane::Database => panel.child(self.render_db_panel(cx)),
            RightPane::Http => panel.child(self.render_http_panel(cx)),
            RightPane::Tabs => panel.child(self.render_tab_list(cx)),
            RightPane::Files => panel.child(self.render_tree(cx)),
            RightPane::Structure => panel.child(self.render_structure(cx)),
            RightPane::Changes => panel.child(self.render_change_lists(cx)).child(self.render_commit_box(cx)),
        }
        .into_any_element()
    }

    /// The panels the side holds: one, or one above another with a border
    /// to drag between them.
    pub(super) fn render_side(&self, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let halves: AnyElement = match self.side {
            [Some(above), Some(below)] => gpui_kit::component::resizable::v_resizable("side-halves")
                .on_resize(|state, _, cx| {
                    if let Some(height) = state.read(cx).sizes().get(1).copied() {
                        let mut layout = cx.try_global::<SideLayout>().copied().unwrap_or_default();
                        layout.split = Some(height);
                        cx.set_global(layout);
                    }
                })
                .child(resizable_panel().child(self.render_panel(above, cx)))
                .child(
                    resizable_panel()
                        .size(cx.try_global::<SideLayout>().and_then(|layout| layout.split).unwrap_or(px(260.)))
                        .size_range(px(80.)..px(4000.))
                        // A line as wide as the one between the sidebar and the tabs: where one panel ends and the other begins.
                        .child(div().size_full().border_t_2().border_color(border).child(self.render_panel(below, cx))),
                )
                .into_any_element(),
            [Some(pane), None] | [None, Some(pane)] => self.render_panel(pane, cx),
            [None, None] => div().into_any_element(),
        };
        v_flex()
            .id("changed-files")
            .track_focus(&self.side_focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| this.side_key(e, window, cx)))
            .size_full()
            .border_l_2()
            .border_color(border)
            .child(halves)
            .into_any_element()
    }

    /// A panel come over the task from its mark: there while the pointer
    /// is on it or the keyboard in it.
    pub(super) fn render_peek(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pane = self.side_peek?;
        let width = cx.try_global::<SidePanelWidth>().map_or(px(300.), |width| width.0);
        Some(
            div()
                .id("side-peek")
                .test_support()
                .track_focus(&self.peek_focus)
                .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| this.side_key(e, window, cx)))
                .absolute()
                .top_0()
                .right_0()
                .h_full()
                .w(width)
                .bg(cx.theme().background)
                .border_l_1()
                .border_color(cx.theme().border)
                .shadow_lg()
                .occlude()
                .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                    let want = this.side_peek.filter(|_| *hovered);
                    this.peek_panel(want, window, cx)
                }))
                .child(self.render_panel(pane, cx))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource as _;

    #[test]
    fn the_menus_marks_are_there() {
        for name in ["house", "square-plus", "archive-restore", "archive", "git-branch-minus", "settings", "message-circle-question-mark"] {
            let path = format!("side-icons/{name}.svg");
            assert!(super::Assets.load(&path).ok().flatten().is_some(), "{path}");
        }
    }
}

#[cfg(test)]
mod shortcut_tests {
    use super::{PANES, ShowSidePanel};

    /// What Learn the keys tells of a panel's mark is the key that shows it.
    #[test]
    fn a_panel_s_shortcut_shows_that_panel() {
        for (ix, pane) in PANES.iter().enumerate() {
            let shortcut = crate::keymap::find(pane.shortcut()).expect("a shortcut in keymap.rs");
            assert!(shortcut.action().partial_eq(&ShowSidePanel(ix)), "{} for {}", shortcut.id, pane.name());
        }
    }
}
