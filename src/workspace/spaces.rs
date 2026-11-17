//! Spaces: the projects sorted into a few sets (work, personal, a client),
//! one of them in the window at a time. A filter and no more: the agents
//! of the others go on working, and nothing of them is told until their
//! space, or all of them, is looked at.

use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::Workspace;
use crate::agent::AgentStatus;

/// A set of projects, by its name; the mark it is known by in the title bar.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Space {
    pub name: String,
    /// A sign or a few letters; drawn on `color` when it has one.
    pub icon: String,
    /// One of [`COLORS`], counted from 1; 0 for none.
    pub color: u8,
}

/// The colors a space's mark can be drawn on: they show on a light
/// ground as on a dark one.
pub const COLORS: [Hsla; 7] = [
    Hsla { h: 0.58, s: 0.75, l: 0.5, a: 1. },
    Hsla { h: 0.38, s: 0.6, l: 0.4, a: 1. },
    Hsla { h: 0.09, s: 0.9, l: 0.5, a: 1. },
    Hsla { h: 0.0, s: 0.72, l: 0.52, a: 1. },
    Hsla { h: 0.78, s: 0.5, l: 0.55, a: 1. },
    Hsla { h: 0.5, s: 0.65, l: 0.4, a: 1. },
    Hsla { h: 0.0, s: 0.0, l: 0.45, a: 1. },
];

/// A mark as it is drawn: its sign or letters, on its color when it has one.
pub fn chip(mark: &str, color: u8) -> Div {
    let label = div().text_size(px(11.)).line_height(px(16.)).child(mark.to_string());
    match COLORS.get(usize::from(color).wrapping_sub(1)) {
        Some(color) => label.px(px(5.)).rounded_sm().bg(*color).text_color(gpui_kit::white()).font_weight(FontWeight::SEMIBOLD),
        None => label,
    }
}

/// The mark of all the spaces together.
pub const ALL: &str = "◎";

impl Space {
    /// Its mark, or the start of its name when it has none.
    pub fn mark(&self) -> String {
        match self.icon.trim() {
            "" => self.name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into()),
            // A sign or a few letters: more would push the title bar apart.
            icon => icon.chars().take(3).collect(),
        }
    }
}

impl Workspace {
    /// Whether a project is of the space in the window: any is when all
    /// are shown, and one sorted into none is everywhere.
    pub fn in_space(&self, pi: usize) -> bool {
        match (&self.space, &self.projects[pi].space) {
            (Some(shown), Some(its)) => shown == its,
            _ => true,
        }
    }

    /// Tells the load readout which tasks are out of the window, so that
    /// it does not name them: after anything that changes what is shown.
    pub fn tell_what_is_hidden(&self) {
        let hidden = self
            .projects
            .iter()
            .enumerate()
            .flat_map(|(pi, project)| project.tasks.iter().enumerate().map(move |(ti, task)| (pi, ti, task)))
            .filter(|(pi, ti, _)| !self.shows(*pi, *ti))
            .filter_map(|(_, _, task)| task.path.file_name().map(|name| name.to_string_lossy().into_owned()))
            .collect();
        *crate::sysmon::OUT_OF_WINDOW.lock().unwrap() = hidden;
    }

    /// The first project of the space in the window: where a new task
    /// goes when none is in hand.
    pub fn first_project(&self) -> Option<usize> {
        (0..self.projects.len()).find(|&pi| self.in_space(pi))
    }

    /// The tasks of other spaces that wait for you: a number by the
    /// switch, no more of them than that.
    pub fn waiting_elsewhere(&self, cx: &App) -> usize {
        if self.space.is_none() {
            return 0;
        }
        self.projects
            .iter()
            .enumerate()
            .filter(|(pi, _)| !self.in_space(*pi))
            .flat_map(|(_, project)| &project.tasks)
            .filter(|task| {
                task.view.as_ref().is_some_and(|view| {
                    let view = view.read(cx);
                    matches!(view.agent_status, AgentStatus::NeedsInput(_)) || view.unseen
                })
            })
            .count()
    }

    /// Goes to a space, or to all of them. The task in hand leaves the
    /// window when it is not of that space; the one last looked at there
    /// takes its place.
    pub fn switch_space(&mut self, space: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let space = space.filter(|name| self.spaces.iter().any(|s| &s.name == name));
        if space == self.space {
            return;
        }
        if let Some(active) = self.active.clone() {
            self.last_in_space.insert(self.space.clone(), active);
        }
        self.space = space;
        let fits = |w: &Self, path: &PathBuf| w.find(path).is_some_and(|(pi, ti)| w.shows(pi, ti));
        if !self.active.clone().is_some_and(|path| fits(self, &path)) {
            // The one last looked at here, else the one last worked in.
            let remembered = self.last_in_space.get(&self.space).cloned().filter(|path| fits(self, path));
            let latest = self
                .projects
                .iter()
                .enumerate()
                .flat_map(|(pi, project)| project.tasks.iter().enumerate().map(move |(ti, task)| (pi, ti, task)))
                .filter(|(pi, ti, task)| task.view.is_some() && self.shows(*pi, *ti))
                .max_by_key(|(_, _, task)| task.last_active)
                .map(|(_, _, task)| task.path.clone());
            match remembered.or(latest) {
                Some(path) => self.switch(&path, window, cx),
                None => self.leave_task(window, cx),
            }
        }
        self.tell_what_is_hidden();
        self.save_later(cx);
        cx.notify();
    }

    /// Puts a project into a space, or into none: it is then in every one.
    pub fn set_project_space(&mut self, pi: usize, space: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.get_mut(pi) else { return };
        project.space = space.filter(|name| self.spaces.iter().any(|s| &s.name == name));
        // The task in hand may have left the space in the window with it.
        if self.active.clone().is_some_and(|path| self.find(&path).is_some_and(|(pi, _)| !self.in_space(pi))) {
            self.leave_task(window, cx);
        }
        self.tell_what_is_hidden();
        self.save_later(cx);
        cx.notify();
    }

    /// The spaces as they are after an edit: projects of one renamed
    /// follow it, those of one removed are in none.
    pub fn set_spaces(&mut self, spaces: Vec<Space>, renamed: &[(String, String)], window: &mut Window, cx: &mut Context<Self>) {
        let follow = |name: &mut Option<String>| {
            if let Some((_, new)) = name.as_ref().and_then(|old| renamed.iter().find(|(from, _)| from == old)) {
                *name = Some(new.clone());
            }
            if name.as_ref().is_some_and(|name| !spaces.iter().any(|s| &s.name == name)) {
                *name = None;
            }
        };
        for project in &mut self.projects {
            follow(&mut project.space);
        }
        let mut shown = self.space.clone();
        follow(&mut shown);
        // The menu's list of spaces, made again once typing a name pauses.
        if self.spaces.iter().map(|s| &s.name).ne(spaces.iter().map(|s| &s.name)) {
            crate::menu::install_soon(cx);
        }
        self.spaces = spaces;
        if shown != self.space {
            self.space = shown;
            if self.active.clone().is_some_and(|path| self.find(&path).is_some_and(|(pi, _)| !self.in_space(pi))) {
                self.leave_task(window, cx);
            }
        }
        self.tell_what_is_hidden();
        self.save_later(cx);
        cx.notify();
    }

    /// In the title bar: the mark of the space in the window, the number
    /// of tasks that wait in the others, and the list to go to another.
    pub fn render_space_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        let (spaces, shown) = (self.spaces.clone(), self.space.clone());
        let here = shown.as_ref().and_then(|name| spaces.iter().find(|s| &s.name == name));
        let (mark, color) = here.map_or((ALL.to_string(), 0), |space| (space.mark(), space.color));
        let waiting = self.waiting_elsewhere(cx);
        let tip = match &shown {
            Some(name) => format!("Space: {name}. Other projects are out of the window; their agents go on working."),
            None => "All spaces: every project is in the window.".to_string(),
        };
        Button::new("space-switch")
            .xsmall()
            .ghost()
            .child(h_flex().gap_1().child(chip(&mark, color)).when(waiting > 0, |row| row.child(div().text_xs().child(waiting.to_string()))))
            .tooltip(tip)
            .dropdown_menu(move |menu, _, _| {
                // Each with its mark as it is drawn in the title bar, color and all.
                let go = |menu: gpui_kit::component::menu::PopupMenu, mark: String, color: u8, name: String, to: Option<String>, on: bool| {
                    let this = this.clone();
                    let row = PopupMenuItem::element(move |_, _| h_flex().gap_2().child(div().w(px(34.)).flex().justify_center().child(chip(&mark, color))).child(name.clone()));
                    menu.item(row.checked(on).on_click(move |_, window, cx| {
                        let to = to.clone();
                        let _ = this.update(cx, |w, cx| w.switch_space(to, window, cx));
                    }))
                };
                let mut menu = go(menu, ALL.to_string(), 0, "All".into(), None, shown.is_none());
                for space in &spaces {
                    menu = go(menu, space.mark(), space.color, space.name.clone(), Some(space.name.clone()), shown.as_ref() == Some(&space.name));
                }
                let edit = this.clone();
                menu.separator().item(PopupMenuItem::new("Spaces…").on_click(move |_, window, cx| {
                    let _ = edit.update(cx, |w, cx| w.spaces_dialog(window, cx));
                }))
            })
    }

    /// The spaces to add, name, mark and remove.
    pub fn spaces_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Each row: the name it had (to follow a rename), and its two fields.
        type Row = (String, Entity<InputState>, Entity<InputState>, std::rc::Rc<std::cell::Cell<u8>>);
        let rows: std::rc::Rc<std::cell::RefCell<Vec<Row>>> = Default::default();
        let field = |text: &str, hint: &'static str, window: &mut Window, cx: &mut App| {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(hint));
            input.update(cx, |input, cx| input.set_value(text.to_string(), window, cx));
            input
        };
        for space in &self.spaces {
            rows.borrow_mut().push((space.name.clone(), field(&space.icon, "mark", window, cx), field(&space.name, "name", window, cx), std::rc::Rc::new(std::cell::Cell::new(space.color))));
        }
        let this = cx.weak_entity();
        // The projects and the space each is of, as the window draws them:
        // read again after every change, not while drawing.
        type Sorted = std::rc::Rc<std::cell::RefCell<Vec<(String, Option<String>)>>>;
        let sorted = |w: &Workspace| w.projects.iter().map(|p| (p.name.clone(), p.space.clone())).collect::<Vec<_>>();
        let projects: Sorted = std::rc::Rc::new(std::cell::RefCell::new(sorted(self)));
        // Kept as it is typed: there is no Save to forget.
        let keep = {
            let (rows, this, projects) = (rows.clone(), this.clone(), projects.clone());
            std::rc::Rc::new(move |window: &mut Window, cx: &mut App| {
                let mut spaces: Vec<Space> = Vec::new();
                let mut renamed = Vec::new();
                for (was, icon, name, color) in rows.borrow_mut().iter_mut() {
                    let (icon, name) = (icon.read(cx).value().trim().to_string(), name.read(cx).value().trim().to_string());
                    // One without a name, or with another's, is not one yet.
                    if name.is_empty() || spaces.iter().any(|s| s.name == name) {
                        continue;
                    }
                    if !was.is_empty() && *was != name {
                        renamed.push((was.clone(), name.clone()));
                    }
                    *was = name.clone();
                    spaces.push(Space { name, icon, color: color.get() });
                }
                let now = this.update(cx, |w, cx| {
                    w.set_spaces(spaces, &renamed, window, cx);
                    w.projects.iter().map(|p| (p.name.clone(), p.space.clone())).collect::<Vec<_>>()
                });
                if let Ok(now) = now {
                    *projects.borrow_mut() = now;
                }
            })
        };
        // Kept by the dialog and gone with it: detached, they would hold the inputs they listen to for good.
        let listening: std::rc::Rc<std::cell::RefCell<Vec<Subscription>>> = Default::default();
        let watch = {
            let keep = keep.clone();
            std::rc::Rc::new(move |input: &Entity<InputState>, window: &mut Window, cx: &mut App| {
                let keep = keep.clone();
                let heard = window.subscribe(input, cx, move |_, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        keep(window, cx);
                    }
                });
                listening.borrow_mut().push(heard);
            })
        };
        for (_, icon, name, _) in rows.borrow().iter() {
            watch(icon, window, cx);
            watch(name, window, cx);
        }
        window.open_dialog(cx, move |dialog, _, cx| {
            let (rows, keep, watch) = (rows.clone(), keep.clone(), watch.clone());
            let (projects, sort) = (projects.clone(), this.clone());
            let muted = cx.theme().muted_foreground;
            dialog.title("Spaces").w(px(500.)).content(move |content, _, cx| {
                let mut list = v_flex().gap_1p5();
                for (ix, (_, icon, name, color)) in rows.borrow().iter().enumerate() {
                    let (rows, keep) = (rows.clone(), keep.clone());
                    let (shade, next, again) = (color.get(), color.clone(), keep.clone());
                    // The mark as it will be drawn; a click gives it the next color, the last being none.
                    let drawn: String = match icon.read(cx).value().trim() {
                        "" => name.read(cx).value().trim().chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into()),
                        mark => mark.chars().take(3).collect(),
                    };
                    list = list.child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .id(("space-color", ix))
                                    .w(px(44.))
                                    .flex()
                                    .justify_center()
                                    .cursor_pointer()
                                    .child(chip(&drawn, shade))
                                    .on_click(move |_, window, cx| {
                                        next.set((next.get() + 1) % (COLORS.len() as u8 + 1));
                                        again(window, cx);
                                        window.refresh();
                                    }),
                            )
                            .child(div().w(px(64.)).child(Input::new(icon).small()))
                            .child(div().flex_1().child(Input::new(name).small()))
                            .child(Button::new(("space-remove", ix)).xsmall().ghost().label("×").tooltip("Remove: its projects are then in no space, and in every one").on_click(
                                move |_, window, cx| {
                                    rows.borrow_mut().remove(ix);
                                    keep(window, cx);
                                    window.refresh();
                                },
                            )),
                    );
                }
                // The projects, each with where it can be: the one chosen is lit, a click on another chooses that.
                let spaces: Vec<(String, String, u8)> = rows
                    .borrow()
                    .iter()
                    .filter_map(|(_, icon, name, color)| {
                        let name = name.read(cx).value().trim().to_string();
                        (!name.is_empty()).then(|| (Space { name: name.clone(), icon: icon.read(cx).value().trim().to_string(), color: color.get() }.mark(), name, color.get()))
                    })
                    .map(|(mark, name, color)| (name, mark, color))
                    .collect();
                let mut table = v_flex().id("space-projects").gap_0p5().max_h(px(260.)).overflow_y_scroll();
                for (pi, (project, its)) in projects.borrow().iter().enumerate() {
                    let mut row = h_flex().gap_1p5().child(div().flex_1().min_w_0().truncate().text_sm().child(project.clone()));
                    // One of them is chosen, as of a row of radio buttons: all (in no space, so in every one), or one space.
                    let choices = std::iter::once((None, ALL.to_string(), 7)).chain(spaces.iter().map(|(name, mark, color)| (Some(name.clone()), mark.clone(), *color)));
                    for (to, mark, color) in choices {
                        let on = *its == to;
                        let (projects, sort) = (projects.clone(), sort.clone());
                        let name = to.clone().unwrap_or_else(|| "all".into());
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("sort-{pi}-{name}")))
                                .cursor_pointer()
                                .when(!on, |chip| chip.opacity(0.3))
                                .child(chip(&mark, if color == 0 { 7 } else { color }))
                                .on_click(move |_, window, cx| {
                                    let to = to.clone();
                                    projects.borrow_mut()[pi].1 = to.clone();
                                    let _ = sort.update(cx, |w, cx| w.set_project_space(pi, to, window, cx));
                                    window.refresh();
                                }),
                        );
                    }
                    table = table.child(row);
                }
                let sortable = !spaces.is_empty() && !projects.borrow().is_empty();
                let (rows, watch) = (rows.clone(), watch.clone());
                content
                    .child(div().text_sm().text_color(muted).child(
                        "A space is a set of projects: work, personal, a client. The one chosen in the title bar is all the window shows; the agents of the others go on working, and nothing of them is told until you look there. A project is put into a space from its menu in the sidebar. The mark is a sign or a few letters; a click on it gives it a color.",
                    ))
                    .child(div().pt_3().child(list))
                    .child(h_flex().pt_3().child(Button::new("space-add").small().outline().label("Add a space").on_click(move |_, window, cx| {
                        let mark = cx.new(|cx| InputState::new(window, cx).placeholder("mark"));
                        let name = cx.new(|cx| InputState::new(window, cx).placeholder("name"));
                        watch(&mark, window, cx);
                        watch(&name, window, cx);
                        name.read(cx).focus_handle(cx).focus(window, cx);
                        rows.borrow_mut().push((String::new(), mark, name, Default::default()));
                        window.refresh();
                    })))
                    .when(sortable, |content| {
                        content.child(div().pt_4().pb_1().text_xs().text_color(muted).child("PROJECTS: each is in all the spaces, or in one")).child(table)
                    })
            })
        });
    }
}
