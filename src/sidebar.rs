//! The left sidebar: a search, the tasks that need you, then each project
//! with its tasks. Rows drag to reorder; right-click for the rest.

use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::Input,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    h_flex, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{agent::AgentStatus, task, workspace::Workspace};

#[derive(Clone)]
pub enum DraggedRow {
    Project { ix: usize, name: SharedString },
    /// `at`: its place in its project when the drag began.
    Task { project: usize, at: usize, path: PathBuf, name: SharedString },
}

impl Render for DraggedRow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = match self {
            DraggedRow::Project { name, .. } | DraggedRow::Task { name, .. } => name.clone(),
        };
        div()
            .px_3()
            .py_1()
            .text_sm()
            .rounded_md()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().primary)
            .child(name)
    }
}

fn section_header(label: &'static str, cx: &App) -> Div {
    h_flex()
        .px_3()
        .pt_3()
        .pb_1()
        .justify_between()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

/// A ring filled clockwise from the top as far as the context is full:
/// quiet while there is room, yellow when it will soon be squeezed, red
/// when that is about to happen.
pub(crate) fn context_dial(fill: f32, cx: &App) -> impl IntoElement {
    let color = if fill >= 0.9 { DIAL_RED } else if fill >= 0.75 { DIAL_AMBER } else { cx.theme().muted_foreground };
    dial("context-dial", fill, color, cx.theme().foreground)
}

/// Colors that show on a light ground as on a dark one: the theme's
/// yellow is all but white on white.
pub(crate) const DIAL_AMBER: Hsla = Hsla { h: 0.09, s: 0.95, l: 0.5, a: 1. };
pub(crate) const DIAL_RED: Hsla = Hsla { h: 0.0, s: 0.8, l: 0.52, a: 1. };

/// Whether a branch is named after the task: its title's words, in
/// order, begin the branch's (`fix-login-x7k2` for "Fix login").
fn branch_follows(title: &str, branch: &str) -> bool {
    let words = |text: &str| text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-");
    let (title, branch) = (words(title), words(branch));
    !title.is_empty() && branch.starts_with(&title)
}

/// A ring of 12 px, filled clockwise from the top by `fill` of a turn in
/// `color`, on a faint whole ring of `ink`: a share of something, at a
/// glance.
pub(crate) fn dial(id: &'static str, fill: f32, color: Hsla, ink: Hsla) -> impl IntoElement {
    let dial = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (center, radius) = (bounds.center(), bounds.size.width.min(bounds.size.height) / 2. - px(1.));
            let at = |turn: f32| {
                let angle = turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                point(center.x + radius * angle.cos(), center.y + radius * angle.sin())
            };
            // An arc of a whole turn has no direction: every arc is drawn as halves.
            let mut arc = |from: f32, to: f32, width: Pixels, color: Hsla| {
                let mut ring = PathBuilder::stroke(width);
                ring.move_to(at(from));
                let middle = from + (to - from) / 2.;
                ring.arc_to(point(radius, radius), px(0.), false, true, at(middle));
                ring.arc_to(point(radius, radius), px(0.), false, true, at(to));
                if let Ok(path) = ring.build() {
                    window.paint_path(path, color);
                }
            };
            arc(0., 1., px(2.), ink.opacity(0.14));
            arc(0., fill.clamp(0.03, 1.), px(2.5), color);
        },
    );
    div().id(id).test_support().flex_none().size(px(12.)).child(dial.size_full())
}

/// The width of the sidebar as a rail, and how narrow the whole one is
/// dragged to become it.
pub const RAIL: Pixels = px(96.);
pub const RAIL_BELOW: Pixels = px(120.);

/// "now", "5m", "3h", "2d", "2w", "4mo".
pub(crate) fn ago(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let d = crate::state::now().saturating_sub(secs);
    match d {
        0..60 => "now".into(),
        60..3600 => format!("{}m", d / 60),
        3600..86400 => format!("{}h", d / 3600),
        86400..1_209_600 => format!("{}d", d / 86400),
        1_209_600..5_184_000 => format!("{}w", d / 604_800),
        _ => format!("{}mo", d / 2_592_000),
    }
}

/// Claude puts a spinner or mark before its session title ("✳ Fix login").
fn clean_title(title: &str) -> String {
    title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim().to_string()
}

impl Workspace {
    pub fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().trim().to_lowercase();
        // As high as its rows, inside what scrolls: a column held to the
        // panel's height would squeeze a row that clips (a truncated line) to nothing.
        let mut list = v_flex().id("sidebar-rows").min_h_full().pb_2();

        // Read once: Needs you lists them, Starred leaves them out.
        let active = if query.is_empty() { self.active_rows(cx) } else { Vec::new() };
        let waiting: std::collections::HashSet<(usize, usize)> = active.iter().copied().collect();
        if query.is_empty() {
            if !active.is_empty() {
                let collapsed = self.active_collapsed;
                list = list.child(
                    section_header(if collapsed { "▸ NEEDS YOU" } else { "▾ NEEDS YOU" }, cx)
                        .id("active-header")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.active_collapsed = !this.active_collapsed;
                            cx.notify();
                        })),
                );
                if !collapsed {
                    // A group that is done is one row, not one per variant.
                    let mut groups = std::collections::HashSet::new();
                    for (pi, ti) in active {
                        let t = &self.projects[pi].tasks[ti];
                        let asks = t.view.as_ref().is_some_and(|v| matches!(v.read(cx).agent_status, AgentStatus::NeedsInput(_)));
                        if t.group.is_empty() || asks {
                            list = list.child(self.render_task(pi, ti, true, cx));
                        } else if groups.insert(t.group.clone()) {
                            list = list.child(self.render_group_row(pi, &t.group, true, cx));
                        }
                    }
                }
            }
        }

        // Starred: wherever their project is, below what needs you; as far
        // as the window shows them (its space, a presentation).
        if query.is_empty() {
            let starred: Vec<(usize, usize)> = self
                .projects
                .iter()
                .enumerate()
                .flat_map(|(pi, p)| p.tasks.iter().enumerate().filter(|(_, t)| t.starred && t.side.is_none()).map(move |(ti, _)| (pi, ti)))
                .filter(|&(pi, ti)| self.shows(pi, ti) && !waiting.contains(&(pi, ti)))
                .collect();
            if !starred.is_empty() {
                let collapsed = self.starred_collapsed;
                list = list.child(
                    section_header(if collapsed { "▸ STARRED" } else { "▾ STARRED" }, cx)
                        .id("starred-header")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.starred_collapsed = !this.starred_collapsed;
                            this.save(cx);
                            cx.notify();
                        })),
                );
                if !collapsed {
                    for (pi, ti) in starred {
                        list = list.child(self.render_task(pi, ti, true, cx));
                    }
                }
            }
        }

        if self.projects.is_empty() {
            list = list.child(
                div().px_3().py_3().child(
                    Button::new("add-first-project")
                        .small()
                        .outline()
                        .label("Add project…")
                        .on_click(cx.listener(|this, _, window, cx| this.add_project_dialog(window, cx))),
                ),
            );
        }
        let mut shown = 0;
        let show_sides = crate::state::with_settings(|s| s.show_side_worktrees);
        for pi in 0..self.projects.len() {
            let p = &self.projects[pi];
            let project_hit = query.is_empty() || p.name.to_lowercase().contains(&query);
            let tasks: Vec<usize> = (0..p.tasks.len())
                .filter(|&ti| self.shows(pi, ti) && (project_hit || self.task_matches(pi, ti, &query, cx)))
                .collect();
            // The agents' own worktrees go under the task they came from, or
            // at the end, folded; a search lists them as it finds them.
            let (sides, tasks): (Vec<usize>, Vec<usize>) = tasks.into_iter().partition(|&ti| p.tasks[ti].side.is_some());
            let sides: Vec<usize> = if show_sides { sides } else { Vec::new() };
            let mut tasks: Vec<usize> = if query.is_empty() { tasks } else { tasks.into_iter().chain(sides.iter().copied()).collect() };
            // The main checkout, then the starred, then the rest in their order.
            tasks.sort_by_key(|&ti| (!p.tasks[ti].main, !p.tasks[ti].starred));
            let sides = if query.is_empty() { sides } else { Vec::new() };
            let parents: std::collections::HashSet<&std::path::Path> = tasks.iter().map(|&ti| p.tasks[ti].path.as_path()).collect();
            let side_parent = |ti: usize| p.tasks[ti].side.as_ref().and_then(|s| s.parent.as_deref()).filter(|parent| parents.contains(parent));
            // A project with nothing to show is not shown: of another space, while presenting, or when the search has no hit in it.
            if !self.in_space(pi) || (tasks.is_empty() && (self.is_presenting() || (!query.is_empty() && !project_hit))) {
                continue;
            }
            list = list.child(self.render_project(pi, shown == 0, cx));
            shown += 1;
            // A search shows its hits even in folded projects.
            if !p.collapsed || !query.is_empty() {
                let mut groups = std::collections::HashSet::new();
                for ti in tasks {
                    let group = &p.tasks[ti].group;
                    // A search lists its hits one by one.
                    if group.is_empty() || !query.is_empty() {
                        list = list.child(self.render_task(pi, ti, false, cx));
                        let under: Vec<usize> = sides.iter().copied().filter(|&s| side_parent(s) == Some(p.tasks[ti].path.as_path())).collect();
                        list = self.render_sides(list, pi, p.tasks[ti].path.clone(), "", under, cx);
                    } else if groups.insert(group.clone()) {
                        list = list.child(self.render_group_row(pi, group, false, cx));
                        if !self.collapsed_groups.contains(group) {
                            for (_, ti) in self.members(group).into_iter().filter(|m| m.0 == pi && self.shows(m.0, m.1)) {
                                list = list.child(self.render_task(pi, ti, false, cx));
                            }
                        }
                    }
                }
                let elsewhere: Vec<usize> = sides.iter().copied().filter(|&s| side_parent(s).is_none()).collect();
                list = self.render_sides(list, pi, p.path.clone(), "temporary ", elsewhere, cx);
            }
        }
        // Dropping below the last project moves it to the end.
        list = list.child(
            div()
                .id("sidebar-end")
                .flex_1()
                .min_h(px(24.))
                .drag_over::<DraggedRow>(|s, row, _, cx| match row {
                    DraggedRow::Project { .. } => s.border_t_2().border_color(cx.theme().primary),
                    _ => s,
                })
                .on_drop(cx.listener(|this, row: &DraggedRow, _, cx| {
                    if let DraggedRow::Project { ix, .. } = row {
                        let end = this.projects.len();
                        this.move_project(*ix, end, cx);
                    }
                })),
        );

        v_flex()
            .size_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .group("sidebar-top")
                    .gap_1()
                    .px_2()
                    .py_1p5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().child(Input::new(&self.search).xsmall().appearance(false).cleanable(true)))
                    // Under the pointer: the projects where nothing runs fold, those where something does open.
                    .child(
                        div().invisible().group_hover("sidebar-top", |s| s.visible()).child(
                            Button::new("fold-idle")
                                .xsmall()
                                .ghost()
                                .child(svg().path("side-icons/chevrons-down-up.svg").size(px(15.)).text_color(cx.theme().muted_foreground))
                                .tooltip("Fold the projects where nothing runs; again, unfold all")
                                .on_click(cx.listener(|this, _, _, cx| this.fold_idle_projects(cx))),
                        ),
                    )
                    // Not while presenting: the window shows what was chosen, and nothing is added to it by a stray click.
                    .when(!self.is_presenting(), |row| {
                        row.child(
                            // A folder, not a bare plus: beside the search of tasks that would read as a new task.
                            Button::new("add-project")
                                .xsmall()
                                .ghost()
                                .child(svg().path("side-icons/folder-plus.svg").size(px(16.)).text_color(cx.theme().muted_foreground))
                                .tooltip("Add a project")
                                .on_click(cx.listener(|this, _, window, cx| this.add_project_dialog(window, cx))),
                        )
                    }),
            )
            .child(div().id("sidebar-list").flex_1().min_h_0().overflow_y_scroll().child(list))
    }

    /// Folds the projects whose tasks are all paused and unfolds the others;
    /// when that is how they are already, unfolds them all.
    pub(crate) fn fold_idle_projects(&mut self, cx: &mut Context<Self>) {
        let runs: Vec<bool> = self.projects.iter().map(|p| p.tasks.iter().any(|t| t.view.is_some())).collect();
        let folded = self.projects.iter().zip(&runs).all(|(p, runs)| p.collapsed == !runs);
        for (p, runs) in self.projects.iter_mut().zip(runs) {
            p.collapsed = !folded && !runs;
        }
        self.save_later(cx);
        cx.notify();
    }

    /// The sidebar as a rail when it was dragged that narrow, and while
    /// presenting: the tasks are there, their names are not.
    pub fn sidebar_is_rail(&self) -> bool {
        self.sidebar_rail || (self.is_presenting() && crate::state::with_settings(|s| s.present_rail))
    }

    /// Opens or closes the whole sidebar over the task, after a while: the
    /// pointer that crosses the rail, or clicks a task on it, opens
    /// nothing, and one that slips off the edge for an instant closes nothing.
    pub fn peek_sidebar(&mut self, open: bool, window: &Window, cx: &mut Context<Self>) {
        self.peek_hovered = open;
        // Not closed under a search being typed.
        let searching = !open && self.search.read(cx).focus_handle(cx).is_focused(window);
        let rest = crate::state::with_settings(|s| s.rail_open_ms);
        // Set never to open: the rail alone, for going from task to task.
        if open == self.sidebar_peek || searching || (open && rest == 0) {
            self.peek_wait = Task::ready(());
            return;
        }
        // A while on the rail before it opens: until then the rail is for
        // going from task to task, and nothing moves under the pointer.
        let wait = std::time::Duration::from_millis(if open { u64::from(rest) } else { 300 });
        self.peek_wait = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            // A row's menu is drawn over the task, not over the sidebar: the pointer on it has
            // not left, and the sidebar going would take the menu with it. Waited out, for a
            // while at most; then it goes unless the pointer is back.
            if !open && menu_waited_out(cx).await && this.read_with(cx, |this, _| this.peek_hovered).unwrap_or(false) {
                return;
            }
            // A menu of the rail is up: it is what was asked for, not the sidebar.
            if open && gpui_kit::component::menu::context_menu_open() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.sidebar_peek = open;
                cx.notify();
            });
        });
    }

    /// The rail: a mark for each task that is shown, the one in hand lit,
    /// and how its agent is and how full its context.
    pub fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let dialed = crate::state::with_settings(|s| s.context_dial);
        let mut rail = v_flex().id("sidebar-rail").w(RAIL).h_full().flex_none().items_center().py_2().gap_0p5().overflow_hidden().border_r_1().border_color(cx.theme().border);
        // As the list has them, above the projects: what waits for you, then what is starred.
        let heading = |name: &'static str, color: Hsla| div().w_full().px_2().pt_1().text_size(px(10.)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(name);
        let waiting = self.active_rows(cx);
        if !waiting.is_empty() {
            rail = rail.child(heading("NEEDS YOU", hsla(0.08, 0.9, 0.55, 1.)));
            for &(pi, ti) in &waiting {
                rail = rail.child(self.render_rail_row(pi, ti, "waiting", dialed, cx));
            }
        }
        let starred: Vec<(usize, usize)> = self
            .projects
            .iter()
            .enumerate()
            .flat_map(|(pi, p)| p.tasks.iter().enumerate().filter(|(_, t)| t.starred && t.side.is_none()).map(move |(ti, _)| (pi, ti)))
            .filter(|&(pi, ti)| self.shows(pi, ti) && !waiting.contains(&(pi, ti)))
            .collect();
        if !starred.is_empty() {
            rail = rail.child(heading("STARRED", muted));
            for (pi, ti) in starred {
                rail = rail.child(self.render_rail_row(pi, ti, "starred", dialed, cx));
            }
        }
        for (pi, project) in self.projects.iter().enumerate() {
            let tasks: Vec<usize> = (0..project.tasks.len()).filter(|&ti| self.shows(pi, ti) && project.tasks[ti].side.is_none()).collect();
            if tasks.is_empty() {
                continue;
            }
            // A name is cut where the rail ends, not after so many letters. Under the pointer the project's
            // archived tasks and its + are over the name's end, and a right click has its menu, as in the list:
            // a task is made and a project seen to without the sidebar opening.
            let (menu, this) = (self.project_menu(pi), cx.weak_entity());
            let (menu_a, this_a) = (menu.clone(), this.clone());
            let group = SharedString::from(format!("rail-project-{pi}"));
            let presenting = self.is_presenting();
            rail = rail.child(
                h_flex()
                    .id(("rail-project", pi))
                    .group(group.clone())
                    .relative()
                    .w_full()
                    .px_2()
                    .pt_1()
                    .text_size(px(10.))
                    .text_color(muted)
                    .child(div().flex_1().min_w_0().truncate().child(project.name.clone()))
                    .when(!presenting, |head| {
                        head.child(
                            h_flex()
                                .absolute()
                                .right_0()
                                .top_0()
                                .bottom_0()
                                .pl_1()
                                .bg(cx.theme().background)
                                .invisible()
                                .group_hover(group, |buttons| buttons.visible())
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(
                                    Button::new(SharedString::from(format!("rail-archived-{pi}")))
                                        .xsmall()
                                        .ghost()
                                        .child(svg().path("side-icons/history.svg").size(px(13.)).text_color(muted))
                                        .tooltip("Archived tasks of the project")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.archived_dialog(Some(pi), window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("rail-add-{pi}")))
                                        .xsmall()
                                        .ghost()
                                        .child(svg().path("side-icons/plus.svg").size(px(14.)).text_color(muted))
                                        .dropdown_menu(move |m, _, _| menu_a(m, this_a.clone())),
                                ),
                        )
                    })
                    .context_menu(move |m, _, _| menu(m, this.clone())),
            );
            for ti in tasks {
                rail = rail.child(self.render_rail_row(pi, ti, "project", dialed, cx));
            }
        }
        rail.test_support()
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| this.peek_sidebar(*hovered, window, cx)))
            // The wait is for a pointer that rests: one on its way down the rail to a task
            // is not asking for the sidebar, and on a touchpad that way takes a while.
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, cx| {
                if !this.sidebar_peek && this.peek_moved.elapsed() > std::time::Duration::from_millis(40) {
                    this.peek_moved = std::time::Instant::now();
                    this.peek_wait = Task::ready(());
                    this.peek_sidebar(true, window, cx);
                }
            }))
    }

    /// A task's row in the rail. `section`: which part of the rail it is in,
    /// a task being in its project's and in Needs you or Starred as well.
    fn render_rail_row(&self, pi: usize, ti: usize, section: &'static str, dialed: bool, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let project = &self.projects[pi];
                let task = &project.tasks[ti];
                let path = task.path.clone();
                let title = self.task_title(pi, ti, cx);
                let fill = self.usage.get(&path).map_or(0., |(_, usage)| usage.fill());
                let in_hand = self.active.as_deref() == Some(path.as_path());
                // Paused: no agent runs for it. Told apart from one that rests, which has the same circle in the list.
                let paused = task.view.is_none() && task.busy.is_none();
                let (glyph, unseen) = match &task.view {
                    Some(view) => {
                        let view = view.read(cx);
                        (crate::task::status_glyph(&view.agent_status), view.unseen)
                    }
                    None => (None, false),
                };
                let mut notes = self.rail_notes(pi, ti, cx);
                // Out of its project's own part of the rail, a task is told whose it is: "main" is in every one.
                let whose: Option<String> = (section != "project").then(|| project.name.chars().take(8).collect());
                if section != "project" {
                    notes.insert(0, (project.name.clone(), None));
                }
                let (doing, figures) = (self.rail_doing(pi, ti, cx), self.rail_figures(pi, ti, cx));
                let menu = self.task_menu(pi, ti, cx);
                let never_opens = crate::state::with_settings(|s| s.rail_open_ms) == 0;
                // In the tooltip too, when it is not one of the notes: what the agent does, or how long ago.
                if let Some((doing, _)) = doing.as_ref().filter(|(doing, _)| !notes.iter().any(|(note, _)| note == doing)) {
                    notes.push((doing.clone(), None));
                }
                let told: SharedString = match notes.is_empty() {
                    true => title.clone().into(),
                    false => format!("{title}\n{}", notes.iter().map(|(note, _)| note.as_str()).collect::<Vec<_>>().join(" · ")).into(),
                };
                // Finished or asking while out of view: the line at its side has the color of that, as in the list.
                let side = match (in_hand, unseen, glyph) {
                    (true, ..) => cx.theme().primary,
                    (false, true, Some((_, color))) => color,
                    _ => transparent_black(),
                };
                (
                    v_flex()
                        .id(SharedString::from(format!("rail-{section}-{}", path.display())))
                        .w_full()
                        .px_2()
                        .py_0p5()
                        .cursor_pointer()
                        .border_l_2()
                        .border_color(side)
                        .when(in_hand, |row| row.bg(cx.theme().accent))
                        .hover(|row| row.bg(cx.theme().accent.opacity(0.6)))
                        .child(
                            h_flex()
                                .w_full()
                                .gap_1()
                                .child(div().w(px(10.)).flex_none().text_xs().map(|mark| match (glyph, paused) {
                                    (Some((glyph, color)), _) => mark.text_color(color).child(glyph),
                                    (None, true) => mark.text_color(muted).child("‖"),
                                    (None, false) => mark.text_color(muted).child("○"),
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(12.))
                                        .when(paused, |name| name.text_color(muted))
                                        .when(unseen && !in_hand, |name| name.font_weight(FontWeight::SEMIBOLD))
                                        .child(title),
                                )
                                .when(dialed && fill > 0., |row| row.child(context_dial(fill, cx))),
                        )
                        // What goes on in it, in a line of its own: the one thing most worth knowing, all the width its.
                        .when(doing.is_some() || whose.is_some(), |row| {
                            row.child(
                                h_flex()
                                    .w_full()
                                    .pl(px(14.))
                                    .gap_1()
                                    .text_size(px(10.))
                                    .line_height(px(12.))
                                    .text_color(muted)
                                    .when_some(whose, |line, whose| line.child(div().flex_none().child(whose)))
                                    .when_some(doing, |line, (doing, color)| line.child(div().flex_1().min_w_0().truncate().text_color(color.unwrap_or(muted)).child(doing))),
                            )
                        })
                        // Its figures in another: signs and numbers, short enough to be there together.
                        .when(!figures.is_empty(), |row| {
                            row.child(
                                h_flex().w_full().pl(px(14.)).gap_1p5().overflow_hidden().text_size(px(10.)).line_height(px(12.)).children(
                                    figures.into_iter().map(|(figure, color)| div().flex_none().text_color(color.unwrap_or(muted)).child(figure)),
                                ),
                            )
                        })
                        // A tooltip only on a rail that never opens: where it does, the pointer resting
                        // brings the whole list, and a tooltip would be left over it.
                        .when(never_opens, |row| row.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(told.clone()).build(window, cx)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // Gone to a task: the rail did its work, and does not open over it.
                            this.peek_wait = Task::ready(());
                            this.switch(&path, window, cx);
                            if let Some(view) = this.active_view() {
                                view.update(cx, |t, cx| t.show_view(0, window, cx));
                            }
                        }))
                        // The task's own menu, as on its row in the list.
                        .context_menu(menu)
                )
                .into_any_element()
    }

    /// What goes on in a task, for the rail's second line: one thing, the
    /// most pressing there is. Being made; asking; its setup or check
    /// failed; what its agent does now; setting up or checking; or how long
    /// ago it last moved.
    pub(crate) fn rail_doing(&self, pi: usize, ti: usize, cx: &App) -> Option<(String, Option<Hsla>)> {
        let task = &self.projects[pi].tasks[ti];
        if let Some(busy) = &task.busy {
            return Some((busy.to_string(), None));
        }
        // Paused is told by the sign before the name and its color: of these there can be many, and they keep to one line.
        let view = task.view.as_ref()?;
        let view = view.read(cx);
        if matches!(view.agent_status, AgentStatus::NeedsInput(_)) {
            return Some(("asks".into(), Some(hsla(0.08, 0.9, 0.55, 1.))));
        }
        let marks = view.auto_marks();
        if let Some((failed, color)) = marks.iter().find(|(mark, _)| mark.starts_with('✗')) {
            return Some((failed.to_string(), Some(*color)));
        }
        if let Some(activity) = view.agent_activity.clone().filter(|_| view.agent_status == AgentStatus::Working) {
            return Some((activity, None));
        }
        if let Some((running, _)) = marks.iter().find(|(mark, _)| mark.ends_with('…')) {
            return Some((running.to_string(), None));
        }
        Some(ago(task.last_active)).filter(|when| !when.is_empty()).map(|when| (when, None))
    }

    /// A task's figures, for the rail's third line: the check passed, files
    /// changed, commits not pushed, services running, review comments, its
    /// pull request, the star.
    pub(crate) fn rail_figures(&self, pi: usize, ti: usize, cx: &App) -> Vec<(String, Option<Hsla>)> {
        let task = &self.projects[pi].tasks[ti];
        let mut figures: Vec<(String, Option<Hsla>)> = Vec::new();
        if let Some(view) = &task.view {
            let view = view.read(cx);
            if view.auto_marks().iter().any(|(mark, _)| *mark == "✓ check") {
                figures.push(("✓".into(), Some(gpui_kit::green())));
            }
            let (changes, unpushed, services) = (view.change_count(), view.unpushed(), view.running_services(cx).len());
            if changes > 0 {
                figures.push((format!("±{changes}"), None));
            }
            if unpushed > 0 {
                figures.push((format!("↑{unpushed}"), None));
            }
            if services > 0 {
                figures.push((if services == 1 { "▶".into() } else { format!("▶{services}") }, Some(gpui_kit::green())));
            }
        }
        if let Some(pr) = task.pr.as_ref().filter(|_| crate::state::with_settings(|s| s.pr_status)) {
            use crate::pr::{Checks, PrState};
            if pr.open_feedback > 0 {
                figures.push((format!("{} rev", pr.open_feedback), None));
            }
            figures.push(match (pr.state, pr.checks) {
                (PrState::Open, Checks::Passed) => (format!("#{}✓", pr.number), Some(gpui_kit::green())),
                (PrState::Open, Checks::Failed) => (format!("#{}✗", pr.number), Some(gpui_kit::red())),
                (PrState::Open, Checks::Pending) => (format!("#{}…", pr.number), None),
                _ => (format!("#{}", pr.number), None),
            });
        }
        if task.starred {
            figures.push(("★".into(), None));
        }
        figures
    }

    /// All there is to say of a task in the rail, for its tooltip: the few
    /// things that are worth going to it for, the most pressing first.
    pub(crate) fn rail_notes(&self, pi: usize, ti: usize, cx: &App) -> Vec<(String, Option<Hsla>)> {
        let task = &self.projects[pi].tasks[ti];
        let mut notes: Vec<(String, Option<Hsla>)> = Vec::new();
        if let Some(busy) = &task.busy {
            notes.push((busy.to_string(), None));
        }
        match &task.view {
            Some(view) => {
                let view = view.read(cx);
                if matches!(view.agent_status, AgentStatus::NeedsInput(_)) {
                    notes.push(("asks".into(), Some(hsla(0.08, 0.9, 0.55, 1.))));
                }
                // How the project's setup and check went: a failure first of all.
                let mut marks = view.auto_marks();
                marks.sort_by_key(|(mark, _)| !mark.starts_with('✗'));
                notes.extend(marks.into_iter().map(|(mark, color)| (mark.to_string(), Some(color))));
                let (changes, unpushed) = (view.change_count(), view.unpushed());
                if changes > 0 {
                    notes.push((format!("±{changes}"), None));
                }
                if unpushed > 0 {
                    notes.push((format!("↑{unpushed}"), None));
                }
                let services = view.running_services(cx).len();
                if services > 0 {
                    notes.push((if services == 1 { "▶".into() } else { format!("▶{services}") }, Some(gpui_kit::green())));
                }
            }
            None if task.busy.is_none() => notes.push(("paused".into(), None)),
            None => {}
        }
        let pr_status = crate::state::with_settings(|s| s.pr_status);
        if let Some(pr) = task.pr.as_ref().filter(|pr| pr.open_feedback > 0 && pr_status) {
            notes.push((format!("{} review", pr.open_feedback), None));
        }
        if task.starred {
            notes.push(("★".into(), None));
        }
        notes
    }

    fn task_matches(&self, pi: usize, ti: usize, query: &str, cx: &App) -> bool {
        let t = &self.projects[pi].tasks[ti];
        let title = self.task_title(pi, ti, cx).to_lowercase();
        title.contains(query) || t.branch.to_lowercase().contains(query)
    }

    /// What a task is called in the list: its own title, or for the main
    /// thread (and untitled worktrees) the agent's session title.
    pub(crate) fn task_title(&self, pi: usize, ti: usize, cx: &App) -> String {
        let t = &self.projects[pi].tasks[ti];
        if !t.group.is_empty() {
            return format!("{} · {}", t.label(), crate::workspace::variants::letter(t.variant));
        }
        if !t.main && !t.title.is_empty() {
            return t.title.clone();
        }
        let agent = t
            .view
            .as_ref()
            .and_then(|v| v.read(cx).agent_title(cx))
            .map(|s| clean_title(&s))
            // Until the agent names itself the terminal shows the command line.
            .filter(|s| !s.is_empty() && !s.contains(" --settings ") && s != "Claude Code");
        agent.unwrap_or_else(|| if t.main { "main".into() } else { t.branch.clone() })
    }

    /// Tasks whose agent waits for an answer or finished while out of view:
    /// an inbox, empty most of the time, most recent first.
    fn active_rows(&self, cx: &App) -> Vec<(usize, usize)> {
        let mut rows: Vec<(usize, usize)> = Vec::new();
        for (pi, p) in self.projects.iter().enumerate() {
            for (ti, t) in p.tasks.iter().enumerate() {
                let waiting = t.view.as_ref().is_some_and(|v| {
                    let v = v.read(cx);
                    let asks = matches!(v.agent_status, AgentStatus::NeedsInput(_));
                    // A variant that is done waits for the others: the group
                    // is looked at once, when all of it is ready.
                    asks || v.unseen && (t.group.is_empty() || self.group_settled(&t.group, cx))
                });
                let in_view = self.active.as_deref() == Some(t.path.as_path())
                    || (!t.group.is_empty() && self.active_group.as_deref() == Some(t.group.as_str()));
                if waiting && !in_view && self.shows(pi, ti) {
                    rows.push((pi, ti));
                }
            }
        }
        rows.sort_by_key(|&(pi, ti)| std::cmp::Reverse(self.projects[pi].tasks[ti].last_active));
        rows
    }

    /// Alt+Down: the task that waits for you, the questions first, then the
    /// finished ones, the longest waiting first; into its agent.
    pub fn next_waiting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut rows = self.active_rows(cx);
        let asks = |&(pi, ti): &(usize, usize), cx: &App| {
            self.projects[pi].tasks[ti]
                .view
                .as_ref()
                .is_some_and(|v| matches!(v.read(cx).agent_status, AgentStatus::NeedsInput(_)))
        };
        rows.reverse();
        rows.sort_by_key(|row| !asks(row, cx));
        let Some(&(pi, ti)) = rows.first() else {
            gpui_kit::component::WindowExt::push_notification(window, gpui_kit::component::notification::Notification::info("No task waits for you."), cx);
            return;
        };
        let path = self.projects[pi].tasks[ti].path.clone();
        self.switch(&path, window, cx);
        if let Some(view) = self.active_view() {
            view.update(cx, |t, cx| t.show_view(0, window, cx));
        }
    }

    /// What a project's menu offers: from its row in the list (a right click, and its +) and from the rail.
    fn project_menu(&self, pi: usize) -> impl Fn(PopupMenu, WeakEntity<Workspace>) -> PopupMenu + Clone + 'static {
        let p = &self.projects[pi];
        let spaces: Vec<(String, String)> = self.spaces.iter().map(|s| (s.name.clone(), s.mark())).collect();
        let in_space = p.space.clone();
        let merged = p.tasks.iter().filter(|t| !t.main && t.side.is_none() && t.busy.is_none() && t.pr.as_ref().is_some_and(|pr| pr.state == crate::pr::PrState::Merged)).count();
        // Which agent its new tasks run, when there is a choice.
        let profiles: Vec<(String, String)> = crate::state::with_settings(|s| if s.several_agents() { s.agents.iter().map(|a| (a.id.clone(), a.name.clone())).collect() } else { Vec::new() });
        let project_agent = p.agent.clone();
        let stale = if self.is_presenting() { 0 } else { p.merged_branches.len() };
        let menu = move |menu: PopupMenu, this: WeakEntity<Workspace>| {
            let (spaces, in_space) = (spaces.clone(), in_space.clone());
            let (profiles, project_agent, agent_w) = (profiles.clone(), project_agent.clone(), this.clone());
            let settings = this.clone();
            let (a, b, c, d) = (this.clone(), this.clone(), this.clone(), this.clone());
            let (archived, stale_w) = (this.clone(), this);
            menu.item(PopupMenuItem::new("Open main thread").on_click(move |_, window, cx| {
                let _ = a.update(cx, |w, cx| w.open_main(pi, window, cx));
            }))
            .item(PopupMenuItem::new("New task…").on_click(move |_, window, cx| {
                let _ = b.update(cx, |w, cx| w.new_task_dialog(pi, window, cx));
            }))
            .item(PopupMenuItem::new("From an archived task…").on_click(move |_, window, cx| {
                let _ = archived.update(cx, |w, cx| w.archived_dialog(Some(pi), window, cx));
            }))
            // Only when there is something to clean up, and how much.
            .when(merged > 0, |menu| {
                menu.item(PopupMenuItem::new(format!("Archive {merged} merged task{}…", if merged == 1 { "" } else { "s" })).on_click(move |_, window, cx| {
                    let _ = d.update(cx, |w, cx| w.archive_merged(pi, window, cx));
                }))
            })
            .when(stale > 0, |menu| {
                menu.item(PopupMenuItem::new(format!("Delete remote branches of merged PRs ({stale})…")).on_click(move |_, window, cx| {
                    let _ = stale_w.update(cx, |w, cx| w.merged_branches_dialog(pi, window, cx));
                }))
            })
            .separator()
            .item(PopupMenuItem::new("Project settings…").on_click(move |_, window, cx| {
                let _ = settings.update(cx, |w, cx| w.project_settings(pi, window, cx));
            }))
            .separator()
            // Which space it is of: one of them, or none, which is all.
            .map(|mut menu| {
                for (name, mark) in &spaces {
                    let (to, w) = (name.clone(), c.clone());
                    menu = menu.item(PopupMenuItem::new(format!("{mark}  In {name}")).checked(in_space.as_ref() == Some(name)).on_click(move |_, window, cx| {
                        let to = to.clone();
                        let _ = w.update(cx, |w, cx| w.set_project_space(pi, Some(to), window, cx));
                    }));
                }
                if spaces.is_empty() {
                    return menu;
                }
                let w = c.clone();
                menu.item(PopupMenuItem::new("In no space").checked(in_space.is_none()).on_click(move |_, window, cx| {
                    let _ = w.update(cx, |w, cx| w.set_project_space(pi, None, window, cx));
                }))
                .separator()
            })
            .map(|mut menu| {
                if profiles.is_empty() {
                    return menu;
                }
                let w = agent_w.clone();
                menu = menu.item(PopupMenuItem::new("New tasks: the default agent").checked(project_agent.is_none()).on_click(move |_, _, cx| {
                    let _ = w.update(cx, |w, cx| w.set_project_agent(pi, None, cx));
                }));
                for (id, name) in &profiles {
                    let (to, w) = (id.clone(), agent_w.clone());
                    menu = menu.item(PopupMenuItem::new(format!("New tasks: {name}")).checked(project_agent.as_ref() == Some(id)).on_click(move |_, _, cx| {
                        let _ = w.update(cx, |w, cx| w.set_project_agent(pi, Some(to.clone()), cx));
                    }));
                }
                menu.separator()
            })
        };
        menu
    }

    /// What a right click on a task offers: the same from its row in the list and from the rail.
    fn task_menu(&self, pi: usize, ti: usize, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let t = &self.projects[pi].tasks[ti];
        let path = t.path.clone();
        let several = crate::state::with_settings(|s| s.several_agents());
        let this = cx.weak_entity();
        let (starred, main) = (t.starred, t.main);
        let (ready, side) = (t.busy.is_none(), t.side.is_some());
        let can_continue = several && t.side.is_none() && ready;
        let hand = crate::commands::InHand::basic(t);
        let can = move |what| hand.allows(what);
        let named = move |what: T| crate::commands::sentence(what.name(starred));
        use crate::commands::TaskCommand as T;
        let menu_path = path.clone();
        let branch = t.branch.clone();
        let menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let (a, b, c, d) = (this.clone(), this.clone(), this.clone(), this.clone());
            let (pa, pb, pc, pd) = (menu_path.clone(), menu_path.clone(), menu_path.clone(), menu_path.clone());
            let (e, f, pe, pf) = (this.clone(), this.clone(), menu_path.clone(), menu_path.clone());
            let (g, pg) = (this.clone(), menu_path.clone());
            let (h, ph) = (this.clone(), menu_path.clone());
            let branch = branch.clone();
            // What it is called and how it is found; its variants; its end.
            menu.when(can(T::Rename), |m| {
                m.item(PopupMenuItem::new(named(T::Rename)).on_click(move |_, window, cx| {
                    let _ = d.update(cx, |w, cx| w.rename_task_dialog(&pd, window, cx));
                }))
            })
            .item(PopupMenuItem::new(named(T::Star)).on_click(move |_, _, cx| {
                let _ = a.update(cx, |w, cx| w.toggle_star(&pa, cx));
            }))
            .item(PopupMenuItem::new(named(T::CopyBranchName)).on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(branch.clone()));
            }))
            .when(can(T::AnotherVariant), |m| {
                m.separator()
                    .item(PopupMenuItem::new(named(T::AnotherVariant)).on_click(move |_, window, cx| {
                        let _ = e.update(cx, |w, cx| w.another_variant_dialog(&pe, window, cx));
                    }))
                    .when(can(T::KeepVariant), |m| {
                        m.item(PopupMenuItem::new(named(T::KeepVariant)).on_click(move |_, window, cx| {
                            let _ = f.update(cx, |w, cx| w.keep_variant_dialog(&pf, window, cx));
                        }))
                    })
            })
            .when(can_continue, |m| {
                m.when(main, |m| m.separator()).item(PopupMenuItem::new("Continue with another agent…").on_click(move |_, window, cx| {
                    let _ = g.update(cx, |w, cx| w.continue_with_dialog(&pg, window, cx));
                }))
            })
            .when(can(T::Pause) || can(T::Archive) || can(T::RemoveSide), |m| m.separator())
            // Pause stops what runs and keeps the task; Archive is the end
            // of one, which a restore takes up again.
            .when(can(T::Pause), |m| {
                m.item(PopupMenuItem::new("Pause").on_click(move |_, window, cx| {
                    let _ = b.update(cx, |w, cx| w.close_task(&pb, window, cx));
                }))
            })
            .when(can(T::Archive) || can(T::RemoveSide), |m| {
                // In a row of its own the task needs no naming: "Pause", not "Pause task".
                m.item(PopupMenuItem::new(if side { "Remove" } else { "Archive" }).on_click(move |_, window, cx| {
                    let _ = c.update(cx, |w, cx| if side { w.remove_side(&pc, window, cx) } else { w.archive_task(&pc, window, cx) });
                }))
            })
            // Archived and kept, back on a day: a task to come back to.
            .when(can(T::Defer), |m| {
                m.item(PopupMenuItem::new("Defer…").on_click(move |_, window, cx| {
                    let _ = h.update(cx, |w, cx| w.defer_task(&ph, window, cx));
                }))
            })
        };
        menu
    }

    fn render_project(&self, pi: usize, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.projects[pi];
        let name: SharedString = p.name.clone().into();
        let running = p.tasks.iter().filter(|t| t.view.is_some()).count();
        let group = SharedString::from(format!("project-group-{pi}"));
        let muted = cx.theme().muted_foreground;
        let this = cx.weak_entity();
        let menu = self.project_menu(pi);
        let (menu_a, this_a) = (menu.clone(), this.clone());
        let (presenting, in_hand) = (self.is_presenting(), self.active_project() == Some(pi));
        h_flex()
            .id(SharedString::from(format!("project-{}", p.path.display())))
            .group(group.clone())
            .pl_3()
            .pr_1()
            .pt_2()
            .pb_1()
            .gap_1()
            .when(!first, |s| s.mt_1().border_t_1().border_color(cx.theme().border))
            .cursor_pointer()
            .text_sm()
            .text_color(muted)
            .child(div().flex_1().min_w_0().truncate().child(name.clone()))
            .when(p.collapsed, |s| {
                s.child(div().text_xs().child(if running > 0 { format!("{running} running ▸") } else { "▸".into() }))
            })
            .child(
                h_flex()
                    .gap_0p5()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    // The project's archived tasks: to find one and bring it back.
                    .child(
                        div().opacity(0.).when(!presenting, |s| s.group_hover(group.clone(), |s| s.opacity(1.))).child(
                            Button::new(SharedString::from(format!("project-archived-{pi}")))
                                .xsmall()
                                .ghost()
                                .child(svg().path("side-icons/history.svg").size(px(15.)).text_color(muted))
                                .tooltip("Archived tasks of the project")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.archived_dialog(Some(pi), window, cx)
                                })),
                        ),
                    )
                    // A new task: always there for the project in hand, where the next one is most often made; for the others under the pointer.
                    .child(
                        div().when(!in_hand || presenting, |s| s.opacity(0.)).when(!presenting, |s| s.group_hover(group, |s| s.opacity(1.))).child(
                            Button::new(SharedString::from(format!("project-add-{pi}")))
                                .xsmall()
                                .ghost()
                                .child(svg().path("side-icons/plus.svg").size(px(16.)).text_color(muted))
                                .dropdown_menu(move |m, _, _| menu_a(m, this_a.clone())),
                        ),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(p) = this.projects.get_mut(pi) {
                    p.collapsed = !p.collapsed;
                }
                cx.notify();
            }))
            .on_drag(DraggedRow::Project { ix: pi, name: name.clone() }, |row, _, _, cx| cx.new(|_| row.clone()))
            .drag_over::<DraggedRow>(move |s, row, _, cx| match row {
                DraggedRow::Project { .. } => s.border_t_2().border_color(cx.theme().primary),
                DraggedRow::Task { project, .. } if *project == pi => s.border_b_2().border_color(cx.theme().primary),
                _ => s,
            })
            .on_drop(cx.listener(move |this, row: &DraggedRow, _, cx| match row {
                DraggedRow::Project { ix, .. } => this.move_project(*ix, pi, cx),
                DraggedRow::Task { project, path, .. } => this.move_task(path, *project, Some(0), cx),
            }))
            .context_menu(move |m, _, _| menu(m, this.clone()))
            .into_any_element()
    }

    /// The variants of one task as one row: its title, a glyph per
    /// variant, and how far they are. A click opens the group's screen.
    fn render_group_row(&self, pi: usize, group: &str, in_active: bool, cx: &mut Context<Self>) -> AnyElement {
        let members = self.members(group);
        let Some(&(_, first)) = members.first() else { return div().into_any_element() };
        let project = &self.projects[pi];
        let head = &project.tasks[first];
        let muted = cx.theme().muted_foreground;
        let folded = self.collapsed_groups.contains(group);
        let is_active = self.active_group.as_deref() == Some(group);
        // A variant in view keeps its group in sight.
        let holds_active = members.iter().any(|&(pi, ti)| self.active.as_deref() == Some(self.projects[pi].tasks[ti].path.as_path()));
        let (summary, asks) = self.group_summary(group, cx);
        let mut details = vec![summary];
        if in_active {
            details.push(project.name.clone());
        }
        let (open, fold, another, stop) = (group.to_string(), group.to_string(), head.path.clone(), group.to_string());
        let this = cx.weak_entity();
        let menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let (a, b, another, stop) = (this.clone(), this.clone(), another.clone(), stop.clone());
            menu.item(PopupMenuItem::new("Another variant…").on_click(move |_, window, cx| {
                let _ = a.update(cx, |w, cx| w.another_variant_dialog(&another, window, cx));
            }))
            .item(PopupMenuItem::new("Stop all").on_click(move |_, window, cx| {
                let _ = b.update(cx, |w, cx| w.stop_group(&stop, window, cx));
            }))
        };
        v_flex()
            .id(SharedString::from(format!("{}group-{group}", if in_active { "active-" } else { "" })))
            .pl_3()
            .pr_3()
            .py_1()
            .cursor_pointer()
            .border_l_2()
            .border_color(transparent_black())
            .when(is_active, |s| s.bg(cx.theme().accent).border_color(cx.theme().primary))
            .when(holds_active && folded, |s| s.border_color(cx.theme().primary))
            .hover(|s| s.bg(cx.theme().accent.opacity(0.6)))
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .id(SharedString::from(format!("fold-{group}-{in_active}")))
                            .flex_none()
                            .w(px(10.))
                            .text_xs()
                            .text_color(muted)
                            .child(if in_active { "" } else if folded { "▸" } else { "▾" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if !this.collapsed_groups.remove(&fold) {
                                    this.collapsed_groups.insert(fold.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(head.label()))
                    .child(h_flex().flex_none().gap_1().text_xs().children(
                        self.group_glyphs(group, cx).into_iter().map(|(glyph, color)| div().text_color(color).child(glyph)),
                    )),
            )
            .child(
                div()
                    .pl(px(18.))
                    .text_xs()
                    .truncate()
                    .text_color(if asks { hsla(0.08, 0.9, 0.55, 1.) } else { muted })
                    .child(details.join(" · ")),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open_group(&open, window, cx)))
            .context_menu(menu)
            .into_any_element()
    }

    /// The agents' worktrees of one task, or of the project for those in
    /// temporary places (`what`): one folded row, and theirs when opened.
    fn render_sides(&self, mut list: Stateful<Div>, pi: usize, key: PathBuf, what: &str, sides: Vec<usize>, cx: &mut Context<Self>) -> Stateful<Div> {
        if sides.is_empty() {
            return list;
        }
        let tasks = &self.projects[pi].tasks;
        let open = self.open_sides.contains(&key);
        let changed = sides.iter().filter(|&&ti| tasks[ti].side.as_ref().is_some_and(|s| s.dirty || s.ahead > 0)).count();
        let mut text = format!("{} {} {what}agent worktree{}", if open { "▾" } else { "▸" }, sides.len(), if sides.len() == 1 { "" } else { "s" });
        if changed > 0 {
            text.push_str(&format!(" · {changed} changed"));
        }
        let fold = key.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("sides-{}", key.display())))
                .test_support()
                .pl(px(26.))
                .pr_3()
                .py_0p5()
                .text_xs()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .cursor_pointer()
                .hover(|s| s.text_color(cx.theme().foreground))
                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Worktrees agents made for themselves: opened without an agent").build(window, cx))
                .child(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.open_sides.remove(&fold) {
                        this.open_sides.insert(fold.clone());
                    }
                    cx.notify();
                })),
        );
        if open {
            for ti in sides {
                list = list.child(self.render_task(pi, ti, false, cx));
            }
        }
        list
    }

    fn render_task(&self, pi: usize, ti: usize, in_active: bool, cx: &mut Context<Self>) -> AnyElement {
        let project = &self.projects[pi];
        let t = &project.tasks[ti];
        let path = t.path.clone();
        // Under its group a variant is its letter and what sets it apart.
        let listed = !t.group.is_empty() && !in_active && self.search.read(cx).value().trim().is_empty();
        let label: SharedString = if listed {
            let letter = crate::workspace::variants::letter(t.variant);
            if t.note.is_empty() { letter } else { format!("{letter} · {}", t.note) }.into()
        } else {
            self.task_title(pi, ti, cx).into()
        };
        let is_active = self.active.as_deref() == Some(path.as_path());
        let running = t.view.is_some();
        // With more than one agent: which one it runs, as a letter.
        let several = crate::state::with_settings(|s| s.several_agents());
        let agent_mark = if several && t.side.is_none() { self.task_profile_at(pi, ti, cx) } else { None };
        let guessed = t.view.as_ref().is_some_and(|v| v.read(cx).status_guessed);

        let ((glyph, changes, unseen, needs), unpushed) = match &t.view {
            Some(view) => {
                let v = view.read(cx);
                let needs = match &v.agent_status {
                    AgentStatus::NeedsInput(message) => Some(message.clone()),
                    _ => None,
                };
                ((task::status_glyph(&v.agent_status), v.change_count(), v.unseen, needs), v.unpushed())
            }
            None => ((None, 0, false, None), 0),
        };
        let muted = cx.theme().muted_foreground;
        // Second line: when it was last busy, and where (the project in the
        // inbox, the branch when the title is not it).
        let mut details: Vec<String> = Vec::new();
        // A working agent's latest step says more than when it last moved.
        let activity = t.view.as_ref().and_then(|v| {
            let v = v.read(cx);
            (v.agent_status == AgentStatus::Working).then(|| v.agent_activity.clone()).flatten()
        });
        // A deferred task come back on its own stays marked till it is opened.
        let woke = t.woke.is_some();
        match activity {
            Some(activity) => details.push(activity),
            None if woke => details.push("back from deferred".into()),
            None => details.extend(Some(ago(t.last_active)).filter(|when| !when.is_empty())),
        }
        // What stays whole at the end of the second line.
        let mut fixed: Vec<String> = Vec::new();
        if in_active {
            details.push(project.name.clone());
        } else if t.main {
            fixed.push(format!("⌂ {}", t.branch));
        } else if !listed && !branch_follows(label.as_ref(), &t.branch) {
            // A branch named after the task says nothing its title does not.
            fixed.push(t.branch.clone());
        }
        // Its worktree not on its branch: detached, on another, mid-rebase.
        if let Some(off) = t.head.as_ref().and_then(|h| h.off(&t.branch)) {
            fixed.push(off);
        }
        if let Some(info) = self.variant_info.get(&path).filter(|i| listed && i.files > 0) {
            details.push(format!("+{} −{}", info.additions, info.deletions));
        }
        if let Some(view) = &t.view {
            for (name, port) in view.read(cx).running_services(cx) {
                details.push(match port {
                    Some(port) => format!("▶ {name} :{port}"),
                    None => format!("▶ {name}"),
                });
            }
        }
        let pr_status = crate::state::with_settings(|s| s.pr_status);
        if let Some(pr) = t.pr.as_ref().filter(|p| p.open_feedback > 0 && pr_status) {
            details.push(format!("{} review comment{}", pr.open_feedback, if pr.open_feedback == 1 { "" } else { "s" }));
        }
        // How full its agent's context is, once that is worth knowing.
        let fill = self.usage.get(&path).map_or(0., |(_, usage)| usage.fill());
        let dialed = crate::state::with_settings(|s| s.context_dial);
        if t.starred {
            fixed.push("★".into());
        }
        if t.locked.is_some() {
            details.push("locked".into());
        }
        // What an agent's own worktree holds beyond where it came from.
        if let Some(side) = &t.side {
            if side.ahead > 0 {
                details.push(format!("{} commit{} ahead", side.ahead, if side.ahead == 1 { "" } else { "s" }));
            }
            if side.dirty {
                details.push("uncommitted".into());
            }
        }
        if let Some(busy) = &t.busy {
            details.push(busy.to_string());
        }
        // How the project's setup and check went in it.
        let auto_marks = t.view.as_ref().map(|v| v.read(cx).auto_marks()).unwrap_or_default();
        let needs = if in_active { None } else { needs };
        // What its agent said: the start under the task in Needs you, the whole in its tooltip.
        let said = t.view.as_ref().and_then(|v| self.said_shown(v, cx));
        let lead = t.view.as_ref().filter(|_| in_active && said.is_some()).and_then(|v| {
            let v = v.read(cx);
            let asks = matches!(v.agent_status, AgentStatus::NeedsInput(_));
            if asks { v.agent_asks.as_ref() } else { v.agent_said.as_ref() }.map(|said| (said.lead.clone(), asks))
        });
        let pr = t.pr.clone().filter(|_| pr_status);
        let overlaps = self.overlaps(project, &path);
        let whole_branch = t.branch.clone();

        let grouped = !t.group.is_empty();
        let menu = self.task_menu(pi, ti, cx);

        let click_path = path.clone();
        let drop_path = path.clone();
        let id = SharedString::from(format!("{}task-{}", if in_active { "active-" } else { "" }, path.display()));
        let mut row = v_flex()
            .id(id)
            .pl(if listed || (t.side.is_some() && !in_active) { px(26.) } else { px(12.) })
            .pr_3()
            .py_0p5()
            .cursor_pointer()
            .border_l_2()
            .border_color(transparent_black())
            .when(is_active, |s| s.bg(cx.theme().accent).border_color(cx.theme().primary))
            .when(unseen && !is_active, |s| s.border_color(glyph.map(|g| g.1).unwrap_or(cx.theme().border)))
            .when(woke && !is_active, |s| s.border_color(cx.theme().primary))
            .hover(|s| s.bg(cx.theme().accent.opacity(0.6)))
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .w(px(10.))
                            .text_xs()
                            .when_some(glyph, |s, (g, color)| s.text_color(color).child(g)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .truncate()
                            .when(!running, |s| s.text_color(muted))
                            .child(label.clone()),
                    )
                    .when_some(agent_mark.as_ref(), |s, profile| {
                        let name = profile.name.clone();
                        s.child(
                            crate::workspace::spaces::agent_chip(&profile.letter(), profile.color, cx.theme().border, muted)
                                .id(SharedString::from(format!("agent-mark-{}", path.display())))
                                .test_support()
                                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(format!("Runs {name}")).build(window, cx)),
                        )
                    })
                    // How full the agent's context is, as a dial.
                    .when(dialed && fill > 0., |s| s.child(context_dial(fill, cx)))
                    .when(unpushed > 0, |s| {
                        s.child(
                            div()
                                .id(SharedString::from(format!("unpushed-{}", path.display())))
                                .flex_none()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("↑{unpushed}"))
                                .tooltip(move |window, cx| {
                                    let n = if unpushed == 1 { "1 commit".into() } else { format!("{unpushed} commits") };
                                    gpui_kit::component::tooltip::Tooltip::new(format!("{n} not pushed")).build(window, cx)
                                }),
                        )
                    })
                    .when(changes > 0, |s| {
                        s.child(
                            div()
                                .id(SharedString::from(format!("changes-{}", path.display())))
                                .flex_none()
                                .text_xs()
                                .text_color(muted)
                                .child(changes.to_string())
                                .tooltip(move |window, cx| {
                                    let n = if changes == 1 { "1 file".into() } else { format!("{changes} files") };
                                    gpui_kit::component::tooltip::Tooltip::new(format!("{n} with uncommitted changes")).build(window, cx)
                                }),
                        )
                    }),
            )
            // Always there, and of one height: the list does not jump as agents start and stop.
            .map(|s| {
                s.child(
                    h_flex()
                        .pl(px(18.))
                        .gap_1p5()
                        .min_w_0()
                        .h(px(15.))
                        .text_size(px(11.))
                        .line_height(px(15.))
                        .when_some(pr, |row, pr| {
                            use crate::pr::{Checks, PrState};
                            let color = match (pr.state, pr.checks) {
                                (PrState::Merged, _) => hsla(0.75, 0.5, 0.62, 1.),
                                (PrState::Open, Checks::Passed) => hsla(0.36, 0.6, 0.45, 1.),
                                (PrState::Open, Checks::Failed) => hsla(0.0, 0.7, 0.55, 1.),
                                (PrState::Open, _) => hsla(0.11, 0.8, 0.5, 1.),
                                _ => muted,
                            };
                            let url = pr.url.clone();
                            row.child(
                                div()
                                    .id(SharedString::from(format!("pr-{}", pr.url)))
                                    .flex_none()
                                    .text_color(color)
                                    .hover(|s| s.underline())
                                    .child(pr.label())
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        cx.open_url(&url);
                                    }),
                            )
                        })
                        .when(!overlaps.is_empty(), |row| {
                            let files: std::collections::BTreeSet<&String> = overlaps.iter().flat_map(|(_, f)| f).collect();
                            let n = files.len();
                            let tip = overlaps
                                .iter()
                                .map(|(task, files)| {
                                    let mut list = files.iter().take(8).cloned().collect::<Vec<_>>().join("\n  ");
                                    if files.len() > 8 {
                                        list.push_str(&format!("\n  … {} more", files.len() - 8));
                                    }
                                    format!("Also changed in {task}:\n  {list}")
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            row.child(
                                div()
                                    .id(SharedString::from(format!("overlap-{}", path.display())))
                                    .flex_none()
                                    .text_color(hsla(0.11, 0.8, 0.5, 1.))
                                    .child(format!("⇄ {n}"))
                                    .tooltip(move |window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                                    }),
                            )
                        })
                        .children(auto_marks.into_iter().map(|(mark, color)| div().flex_none().text_color(color).child(mark)))
                        // What may be cut off, then what may not.
                        .child(div().flex_1().min_w_0().truncate().text_color(muted).child(details.join(" · ")))
                        .when(!fixed.is_empty(), |row| row.child(div().flex_none().max_w(relative(0.6)).truncate().text_color(muted).child(fixed.join(" · ")))),
                )
            })
            .when_some(needs, |s, message| {
                s.child(div().pl(px(18.)).text_xs().truncate().text_color(hsla(0.08, 0.9, 0.55, 1.)).child(message))
            })
            .when_some(lead, |s, (lead, asks)| {
                s.child(
                    div()
                        .id(SharedString::from(format!("said-{}", path.display())))
                        .pl(px(18.))
                        .pb_0p5()
                        .min_w_0()
                        .text_size(px(11.))
                        .line_height(px(15.))
                        .line_clamp(if asks { 2 } else { 3 })
                        .text_ellipsis()
                        .text_color(if asks { hsla(0.08, 0.9, 0.55, 0.9) } else { muted.opacity(0.9) })
                        .child(lead),
                )
            })
            // A click on a task, current or not, lands in its agent.
            .on_click(cx.listener(move |this, _, window, cx| {
                this.switch(&click_path, window, cx);
                if let Some(view) = this.active_view() {
                    view.update(cx, |t, cx| t.show_view(0, window, cx));
                }
            }));
        // A variant keeps its place in its group.
        if !in_active && !grouped {
            row = row
                .on_drag(DraggedRow::Task { project: pi, at: ti, path: path.clone(), name: label }, |row, _, _, cx| {
                    cx.new(|_| row.clone())
                })
                // Dragged down, a task goes after the one it is dropped on;
                // up, before it: either way to where the line shows.
                .drag_over::<DraggedRow>(move |s, row, _, cx| match row {
                    DraggedRow::Task { project, at, .. } if *project == pi && *at < ti => s.border_b_2().border_color(cx.theme().primary),
                    DraggedRow::Task { project, .. } if *project == pi => s.border_t_2().border_color(cx.theme().primary),
                    _ => s,
                })
                .on_drop(cx.listener(move |this, row: &DraggedRow, _, cx| {
                    if let DraggedRow::Task { project, path, .. } = row
                        && let (Some((_, from)), Some((_, to))) = (this.find(path), this.find(&drop_path))
                    {
                        this.move_task(path, *project, Some(if from < to { to + 1 } else { to }), cx);
                    }
                }));
        }
        // Under the row, how full the agent's context is: yellow when it
        // will soon be squeezed, red when that is about to happen.
        // What the row leaves out, when the pointer rests on the task: its
        // branch, and what its agent has used.
        let mut told = format!("⎇ {whole_branch}");
        if let Some(profile) = &agent_mark {
            told.push_str(&format!("\nAgent: {}", profile.name));
        }
        if guessed {
            told.push_str("\nIts status is guessed from its terminal: this agent does not report it.");
        }
        if let Some((_, usage)) = self.usage.get(&path).filter(|(_, usage)| usage.context > 0) {
            let window = match usage.window() {
                1_000_000 => "1M".to_string(),
                w => crate::usage::tokens(w),
            };
            told.push_str(&format!(
                "\nContext {:.0}% full: {} of {window} tokens\nThis session: {} tokens · last 24 hours: {}",
                fill * 100.,
                crate::usage::tokens(usage.context),
                crate::usage::tokens(usage.spent()),
                crate::usage::tokens(usage.recent)
            ));
        }
        // Its last words, the start of them: a whole answer would cover the window.
        if let Some(said) = said {
            told.push_str("\n\n");
            let lines: Vec<&str> = said.lines().filter(|l| !l.trim().is_empty()).take(8).collect();
            let mut short: String = lines.join("\n").chars().take(400).collect();
            if short.len() < said.trim().len() {
                short.push('…');
            }
            told.push_str(&short);
        }
        let (pressed, left) = (path.clone(), path.clone());
        row = row
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                this.menu_on = Some(pressed.clone());
                cx.notify();
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if !hovered && this.menu_on.as_ref() == Some(&left) {
                    this.menu_on = None;
                    cx.notify();
                }
            }));
        if self.menu_on.as_ref() != Some(&path) {
            row = row.tooltip(move |window, cx| {
                let told = told.clone();
                gpui_kit::component::tooltip::Tooltip::element(move |_, _| div().max_w(px(420.)).child(told.clone())).build(window, cx)
            });
        }
        row.test_support().context_menu(menu).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::branch_follows;

    #[core::prelude::v1::test]
    fn a_branch_named_after_its_task_is_not_shown() {
        assert!(branch_follows("performance check", "performance-check-h3crnhm3"));
        assert!(branch_follows("Fix login!", "fix-login"));
        assert!(!branch_follows("Fix login", "DEV-123_fixLogin"), "named by hand, or from an issue");
        assert!(!branch_follows("Сборка приложения", "main"));
        assert!(!branch_follows("", "anything"));
    }

    /// More tasks than the window is high: each row keeps its height and
    /// the list scrolls, where a flex column would squeeze them into it.
    #[gpui_kit::test]
    fn many_tasks_scroll_and_are_not_squeezed(cx: &mut gpui_kit::TestAppContext) {
        use crate::state::{ProjectState, State, TaskState};
        use crate::workspace::Workspace;
        use gpui_kit::test::TestWindowExt as _;
        use gpui_kit::{AppContext as _, Bounds, Point, WindowBounds, WindowOptions, px, size};
        let _one = crate::workspace::variants::tests::ONE_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ideae-many-tasks-test-{}", std::process::id()));
        // SAFETY: set before anything of the app reads them, one window at a time.
        unsafe {
            std::env::set_var("IDEAE_STATE", dir.join("state.json"));
            std::env::set_var("IDEAE_NO_DAEMON", "1");
        }
        let tasks = (0..40)
            .map(|i| TaskState { path: dir.join(format!("t{i}")), branch: format!("t{i}"), title: format!("Task {i}"), ..Default::default() })
            .collect();
        let saved = State { projects: vec![ProjectState { path: dir.join("repo"), tasks, ..Default::default() }], ..Default::default() };
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.json"), serde_json::to_string(&saved).unwrap()).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_cn::init(cx);
            crate::theme::init(cx);
        });
        let (handle, workspace) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(1000.), px(400.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(Vec::new(), Some(None), None, window, cx))).expect("a test window")
        });
        // The agents' own worktrees, folded at the end: a line that truncates.
        let repo = dir.join("repo");
        workspace.update(cx, |w, _| {
            for t in &mut w.projects[0].tasks[35..] {
                t.side = Some(crate::workspace::side::Side { parent: None, ahead: 0, dirty: false, about: None });
            }
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let sides = window.find(gpui_kit::SharedString::from(format!("sides-{}", repo.display()))).bounds();
            assert!(sides.size.height >= px(16.), "the folded worktrees' line as high as its text: {:?}", sides.size.height);
            let row = |i: usize| window.find(gpui_kit::SharedString::from(format!("task-{}", dir.join(format!("t{i}")).display()))).bounds();
            let (first, last) = (row(0), row(34));
            assert!(first.size.height >= px(20.), "a row as high as its line: {:?}", first.size.height);
            assert_eq!(first.size.height, last.size.height, "the last row as high as the first");
            assert!(last.origin.y > px(400.), "the last row below the window, to scroll to: {:?}", last.origin.y);
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Waits while a context menu is up, a minute at the most (one whose owner
/// went away is never told closed). Whether there was one to wait for.
pub(crate) async fn menu_waited_out(cx: &mut AsyncApp) -> bool {
    let mut waited = false;
    for _ in 0..300 {
        if !gpui_kit::component::menu::context_menu_open() {
            break;
        }
        waited = true;
        cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
    }
    waited
}
