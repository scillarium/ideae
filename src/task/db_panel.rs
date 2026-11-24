//! The Database panel: the connections, the databases of one opened and the
//! tables of a database opened, read on opening and kept for every task
//! until Refresh; a console on any of them is a scratch file on it.
//!
//! Its lines are made when what it read or the connections change, not on
//! every frame, and only those in view are drawn.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::menu::PopupMenuItem;

use super::*;
use crate::db::{browse, config, guard};

enum Load<T> {
    /// The read under way: what comes from another is of no use.
    Loading(u64),
    Ready(T),
    Failed(String),
}

impl<T> From<Result<T, String>> for Load<T> {
    fn from(result: Result<T, String>) -> Self {
        result.map_or_else(Load::Failed, Load::Ready)
    }
}

/// A table as a line shows it: not its columns, which the schema cache has.
struct Table {
    name: String,
    view: bool,
    columns: usize,
}

/// One line of the panel.
enum Line {
    Group { production: bool },
    Connection { id: String, name: String, detail: String, color: Option<u8>, production: bool, open: bool },
    Database { id: String, name: String, open: bool },
    Table { id: String, database: String, name: String, view: bool, columns: usize, driver: config::Driver },
    /// Under a connection or a database: being read, or why it was not.
    Note { depth: usize, text: String, failed: bool, unlock: Option<String> },
}

/// What the panel has read, the same in every task.
#[derive(Default)]
struct DbTree {
    open: HashSet<String>,
    open_databases: HashSet<(String, String)>,
    databases: HashMap<String, Load<Vec<String>>>,
    tables: HashMap<(String, String), Load<Vec<Table>>>,
    turn: u64,
    /// Counts every change, so the lines are made again only after one.
    changes: u64,
    lines: Rc<Vec<Line>>,
    made_for: Option<(u64, u64)>,
}

impl Global for DbTree {}

impl DbTree {
    fn next(&mut self) -> u64 {
        self.turn += 1;
        self.turn
    }
}

/// The tree, to change: its lines are made again.
fn edit(cx: &mut App) -> &mut DbTree {
    let tree = cx.default_global::<DbTree>();
    tree.changes += 1;
    tree
}

fn locked_or_keyring(e: anyhow::Error) -> String {
    if e.to_string() == guard::LOCKED { e.to_string() } else { format!("The keyring: {e}") }
}

fn load_databases(id: String, fresh: bool, cx: &mut App) {
    let Some(connection) = config::find(&id) else { return };
    let tree = edit(cx);
    if !fresh && tree.databases.get(&id).is_some_and(|l| !matches!(l, Load::Failed(_))) {
        return;
    }
    let turn = tree.next();
    tree.databases.insert(id.clone(), Load::Loading(turn));
    let password = crate::db::password::read_for(&connection, cx);
    cx.spawn(async move |cx| {
        let read = match password.await {
            Ok(password) => cx.background_spawn(crate::procs::on_thread(move || browse::databases(&connection, password))).await,
            Err(e) => Err(locked_or_keyring(e)),
        };
        cx.update(|cx| {
            let tree = edit(cx);
            if matches!(tree.databases.get(&id), Some(Load::Loading(t)) if *t == turn) {
                tree.databases.insert(id, read.into());
                cx.refresh_windows();
            }
        });
    })
    .detach();
}

fn load_tables(id: String, database: String, fresh: bool, cx: &mut App) {
    let Some(connection) = config::find(&id) else { return };
    let key = (id, database);
    let tree = edit(cx);
    if !fresh && tree.tables.get(&key).is_some_and(|l| !matches!(l, Load::Failed(_))) {
        return;
    }
    let turn = tree.next();
    tree.tables.insert(key.clone(), Load::Loading(turn));
    cx.spawn(async move |cx| {
        // What was kept first: the keyring is asked only when there is nothing, or for Refresh.
        let (of, database) = (connection.id.clone(), key.1.clone());
        let kept = if fresh { None } else { cx.background_spawn(async move { browse::kept_tables(&of, &database) }).await };
        let read = match kept {
            Some(tables) => Ok(tables),
            None => {
                let password = cx.update(|cx| crate::db::password::read_for(&connection, cx));
                let database = key.1.clone();
                match password.await {
                    Ok(password) => cx.background_spawn(crate::procs::on_thread(move || browse::tables(&connection, password, &database, true))).await,
                    Err(e) => Err(locked_or_keyring(e)),
                }
            }
        };
        let read = read.map(|tables| tables.into_iter().map(|t| Table { columns: t.columns.len(), name: t.name, view: t.view }).collect::<Vec<_>>());
        cx.update(|cx| {
            let tree = edit(cx);
            if matches!(tree.tables.get(&key), Some(Load::Loading(t)) if *t == turn) {
                tree.tables.insert(key, read.into());
                cx.refresh_windows();
            }
        });
    })
    .detach();
}

/// Production unlocked: what was refused for the lock is read now.
pub fn reload_locked(cx: &mut App) {
    let Some(tree) = cx.try_global::<DbTree>() else { return };
    let locked = |e: &String| e == guard::LOCKED;
    let databases: Vec<String> = tree.databases.iter().filter(|(_, l)| matches!(l, Load::Failed(e) if locked(e))).map(|(id, _)| id.clone()).collect();
    let tables: Vec<(String, String)> = tree.tables.iter().filter(|(_, l)| matches!(l, Load::Failed(e) if locked(e))).map(|(k, _)| k.clone()).collect();
    for id in databases {
        load_databases(id, true, cx);
    }
    for (id, database) in tables {
        load_tables(id, database, true, cx);
    }
}

fn hue(color: Option<u8>) -> Option<Hsla> {
    color.and_then(|c| config::COLORS.get(usize::from(c))).map(|(_, h)| hsla(*h, 0.7, 0.5, 1.))
}

/// What the panel read of a connection that changed or is gone: read
/// again when next opened.
pub fn forget_in_panel(id: &str, cx: &mut App) {
    if let Some(tree) = cx.try_global::<DbTree>()
        && !tree.databases.contains_key(id)
        && !tree.tables.keys().any(|(of, _)| of == id)
    {
        return;
    }
    let tree = edit(cx);
    tree.databases.remove(id);
    tree.tables.retain(|(of, _), _| of != id);
    tree.open.remove(id);
    tree.open_databases.retain(|(of, _)| of != id);
    cx.refresh_windows();
}

/// The panel's lines: development connections, then production ones under
/// a heading, each with what was read of it when opened.
fn make_lines(tree: &DbTree, mut connections: Vec<config::Connection>) -> Vec<Line> {
    connections.sort_by_key(|c| (c.production, c.name.to_lowercase()));
    let grouped = connections.iter().any(|c| c.production);
    let mut lines = Vec::new();
    let mut group = None;
    let note = |depth: usize, failed: Option<&String>, id: &str, waiting: &str| match failed {
        Some(e) if e == guard::LOCKED => Line::Note { depth, text: "Production is locked · click to unlock".into(), failed: true, unlock: Some(id.to_string()) },
        Some(e) => Line::Note { depth, text: e.clone(), failed: true, unlock: None },
        None => Line::Note { depth, text: waiting.into(), failed: false, unlock: None },
    };
    for connection in connections {
        if grouped && group != Some(connection.production) {
            group = Some(connection.production);
            lines.push(Line::Group { production: connection.production });
        }
        let id = connection.id.clone();
        let open = tree.open.contains(&id);
        lines.push(Line::Connection {
            id: id.clone(),
            name: connection.name.clone(),
            detail: format!("{} · {}", connection.driver.name(), connection.host),
            color: connection.color,
            production: connection.production,
            open,
        });
        if !open {
            continue;
        }
        let databases = match tree.databases.get(&id) {
            Some(Load::Ready(databases)) => databases,
            Some(Load::Failed(e)) => {
                lines.push(note(0, Some(e), &id, ""));
                continue;
            }
            _ => {
                lines.push(note(0, None, &id, "Connecting…"));
                continue;
            }
        };
        for database in databases {
            let key = (id.clone(), database.clone());
            let open = tree.open_databases.contains(&key);
            lines.push(Line::Database { id: id.clone(), name: database.clone(), open });
            if !open {
                continue;
            }
            match tree.tables.get(&key) {
                Some(Load::Ready(tables)) if tables.is_empty() => lines.push(note(1, None, &id, "No tables")),
                Some(Load::Ready(tables)) => lines.extend(tables.iter().map(|t| Line::Table {
                    id: id.clone(),
                    database: database.clone(),
                    name: t.name.clone(),
                    view: t.view,
                    columns: t.columns,
                    driver: connection.driver,
                })),
                Some(Load::Failed(e)) => lines.push(note(1, Some(e), &id, "")),
                _ => lines.push(note(1, None, &id, "Reading…")),
            }
        }
    }
    lines
}

const LINE: f32 = 24.;

#[allow(clippy::too_many_arguments)]
fn row(id: SharedString, depth: usize, open: Option<bool>, icon: &'static str, color: Hsla, name: String, detail: Option<String>, cx: &App) -> Stateful<Div> {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .id(id)
        .w_full()
        .h(px(LINE))
        .pl(px(8. + depth as f32 * 14.))
        .pr_2()
        .gap_1p5()
        .cursor_pointer()
        .hover(|row| row.bg(cx.theme().accent))
        .child(div().w(px(10.)).flex_none().text_xs().text_color(muted).child(match open {
            Some(true) => "▾",
            Some(false) => "▸",
            None => "",
        }))
        .child(svg().path(icon).size(px(14.)).flex_none().text_color(color))
        .child(div().flex_none().max_w_full().truncate().child(name))
        .when_some(detail, |row, detail| row.child(div().min_w_0().truncate().text_xs().text_color(muted).child(detail)))
}

type Act = Rc<dyn Fn(&mut TaskView, &ClickEvent, &mut Window, &mut Context<TaskView>)>;

fn on(view: &WeakEntity<TaskView>, run: Act) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |e, window, cx| {
        let _ = view.update(cx, |t, cx| run(t, e, window, cx));
    }
}

impl TaskView {
    /// Closed, a connection lets go of its tables; its databases marked open
    /// are read with it when it opens again, from the schema cache.
    fn db_toggle(&mut self, id: String, cx: &mut Context<Self>) {
        let tree = edit(cx);
        if tree.open.remove(&id) {
            tree.tables.retain(|(of, _), _| *of != id);
            return cx.notify();
        }
        tree.open.insert(id.clone());
        let databases: Vec<String> = tree.open_databases.iter().filter(|(of, _)| *of == id).map(|(_, d)| d.clone()).collect();
        load_databases(id.clone(), false, cx);
        for database in databases {
            load_tables(id.clone(), database, false, cx);
        }
        cx.notify();
    }

    /// The question; what the lock refused is read again with the unlock.
    fn db_unlock(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if guard::is_open() {
            return self.db_refresh(Some(id), cx);
        }
        super::sql_guard::production_gate(Rc::new(|_, _| {}), window, cx);
    }

    fn db_toggle_database(&mut self, id: String, database: String, cx: &mut Context<Self>) {
        let key = (id, database);
        let tree = edit(cx);
        if tree.open_databases.remove(&key) {
            // Its tables go with it: read again when opened.
            tree.tables.remove(&key);
        } else {
            tree.open_databases.insert(key.clone());
            load_tables(key.0, key.1, false, cx);
        }
        cx.notify();
    }

    /// Reads again what is open; what is closed is read when opened.
    fn db_refresh(&mut self, only: Option<String>, cx: &mut Context<Self>) {
        let tree = edit(cx);
        tree.databases.retain(|id, _| only.as_ref().is_some_and(|o| o != id));
        tree.tables.retain(|(id, _), _| only.as_ref().is_some_and(|o| o != id));
        let open: Vec<String> = tree.open.iter().filter(|id| only.as_ref().is_none_or(|o| o == *id)).cloned().collect();
        let databases: Vec<(String, String)> = tree.open_databases.iter().filter(|(id, _)| open.contains(id)).cloned().collect();
        for id in open {
            load_databases(id, true, cx);
        }
        for (id, database) in databases {
            load_tables(id, database, true, cx);
        }
        cx.notify();
    }

    /// A scratch file on the connection and database, with `text` in it;
    /// on a connection, its own database.
    fn db_console(&mut self, id: String, database: Option<String>, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let database = database.or_else(|| config::find(&id).and_then(|c| c.database));
        let making = cx.background_spawn(async move {
            let path = crate::db::scratch::create()?;
            if let Some(text) = text {
                std::fs::write(&path, text)?;
            }
            Ok::<_, std::io::Error>(path)
        });
        cx.spawn_in(window, async move |this, cx| {
            let made = making.await;
            let _ = this.update_in(cx, |this, window, cx| match made {
                Ok(path) => {
                    config::remember(&path, &id, database.as_deref());
                    this.open_file(path, 0, window, cx);
                }
                Err(e) => window.push_notification(Notification::error(format!("No console: {e}")).title("Database"), cx),
            });
        })
        .detach();
    }

    fn db_edit(id: Option<String>, window: &mut Window, cx: &mut App) {
        super::sql_dialog::connection_dialog(id, None, window, cx);
    }

    pub(super) fn db_panel_tools(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let muted = cx.theme().muted_foreground;
        let drawn = move |id: &'static str, drawing: &'static str, tip: &'static str| Button::new(id).xsmall().ghost().child(svg().path(drawing).size(px(14.)).text_color(muted)).tooltip(tip);
        vec![
            drawn("db-add", "side-icons/plus.svg", "New connection…").on_click(|_, window, cx| Self::db_edit(None, window, cx)).into_any_element(),
            drawn("db-import", "side-icons/archive-restore.svg", "Import from DataGrip…")
                .on_click(|_, window, cx| super::sql_dialog::import_dialog(window, cx))
                .into_any_element(),
            drawn("db-refresh", "side-icons/rotate-cw.svg", "Read the open connections again")
                .on_click(cx.listener(|this, _, _, cx| this.db_refresh(None, cx)))
                .into_any_element(),
        ]
    }

    pub(super) fn render_db_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let generation = config::generation();
        let stale = cx.try_global::<DbTree>().is_none_or(|tree| tree.made_for != Some((tree.changes, generation)));
        if stale {
            let connections = config::list();
            let tree = cx.default_global::<DbTree>();
            tree.lines = Rc::new(make_lines(tree, connections));
            tree.made_for = Some((tree.changes, generation));
        }
        let lines = cx.default_global::<DbTree>().lines.clone();
        if lines.is_empty() {
            let muted = cx.theme().muted_foreground;
            return v_flex()
                .gap_2()
                .px_3()
                .py_3()
                .text_sm()
                .child(div().font_semibold().child("Databases"))
                .child(div().text_xs().text_color(muted).child(
                    "MySQL, MariaDB, PostgreSQL and Redis: the connections, their databases and tables, and a console on any of them.",
                ))
                .child(Button::new("db-first").small().outline().label("New connection…").on_click(|_, window, cx| Self::db_edit(None, window, cx)))
                .child(Button::new("db-first-import").small().ghost().label("Import from DataGrip…").on_click(|_, window, cx| super::sql_dialog::import_dialog(window, cx)))
                .into_any_element();
        }
        let view = cx.weak_entity();
        uniform_list("db-panel", lines.len(), move |range, _, cx| range.map(|ix| draw_line(&lines[ix], ix, &view, cx)).collect())
            .size_full()
            .py_1()
            .text_sm()
            .into_any_element()
    }
}

/// One line, drawn: only those in view are.
fn draw_line(line: &Line, ix: usize, view: &WeakEntity<TaskView>, cx: &mut App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    match line {
        Line::Group { production: false } => div().h(px(LINE)).px_3().pt_2().text_xs().text_color(muted).child("DEVELOPMENT").into_any_element(),
        Line::Group { production: true } => h_flex()
            .id("db-group-production")
            .test_support()
            .h(px(LINE))
            .px_3()
            .pt_2()
            .gap_2()
            .text_xs()
            .cursor_pointer()
            .child(div().text_color(gpui_kit::red()).child("PRODUCTION"))
            .child(div().text_color(muted).child(match guard::open_until() {
                Some(until) => format!("unlocked until {} · click to lock", crate::local_time::hour_minute(until)),
                None => "locked · click to unlock".into(),
            }))
            .on_click(|_, window, cx| {
                if guard::is_open() {
                    super::sql_guard::lock_now(cx);
                } else {
                    super::sql_guard::production_gate(Rc::new(|_, _| {}), window, cx);
                }
            })
            .into_any_element(),
        Line::Connection { id, name, detail, color, production, open } => {
            let color = hue(*color).unwrap_or(if *production { gpui_kit::red() } else { cx.theme().primary });
            let (click, menu_id, menu_view) = (id.clone(), id.clone(), view.clone());
            row(format!("db-c-{id}").into(), 0, Some(*open), "side-icons/database.svg", color, name.clone(), Some(detail.clone()), cx)
                .test_support()
                .on_click(on(
                    view,
                    Rc::new(move |t, e, window, cx| {
                        // The first click of a double one opened or closed it: put back.
                        t.db_toggle(click.clone(), cx);
                        if e.click_count() == 2 {
                            t.db_console(click.clone(), None, None, window, cx);
                        }
                    }),
                ))
                .context_menu(move |menu, _, _| {
                    let (view, id) = (menu_view.clone(), menu_id.clone());
                    let (refresh, again, edit) = (view.clone(), id.clone(), id.clone());
                    menu.item(PopupMenuItem::new("New console").on_click(move |_, window, cx| {
                        let _ = view.update(cx, |t, cx| t.db_console(id.clone(), None, None, window, cx));
                    }))
                    .item(PopupMenuItem::new("Refresh").on_click(move |_, _, cx| {
                        let _ = refresh.update(cx, |t, cx| t.db_refresh(Some(again.clone()), cx));
                    }))
                    .separator()
                    .item(PopupMenuItem::new("Edit connection…").on_click(move |_, window, cx| TaskView::db_edit(Some(edit.clone()), window, cx)))
                })
                .into_any_element()
        }
        Line::Database { id, name, open } => {
            let (click, menu_key, menu_view) = ((id.clone(), name.clone()), (id.clone(), name.clone()), view.clone());
            row(format!("db-d-{id}-{name}").into(), 1, Some(*open), "side-icons/database.svg", muted, name.clone(), None, cx)
                .test_support()
                .on_click(on(
                    view,
                    Rc::new(move |t, e, window, cx| {
                        let (id, database) = click.clone();
                        t.db_toggle_database(id.clone(), database.clone(), cx);
                        if e.click_count() == 2 {
                            t.db_console(id, Some(database), None, window, cx);
                        }
                    }),
                ))
                .context_menu(move |menu, _, _| {
                    let (view, (id, database)) = (menu_view.clone(), menu_key.clone());
                    let name = database.clone();
                    menu.item(PopupMenuItem::new("New console").on_click(move |_, window, cx| {
                        let _ = view.update(cx, |t, cx| t.db_console(id.clone(), Some(database.clone()), None, window, cx));
                    }))
                    .item(crate::copying::item("Copy name", name))
                })
                .into_any_element()
        }
        Line::Table { id, database, name, view: is_view, columns, driver } => {
            let redis = *driver == config::Driver::Redis;
            let detail = (!redis).then(|| format!("{columns}{}", if *is_view { " · view" } else { "" }));
            let select = (!redis).then(|| format!("SELECT * FROM {} LIMIT 100;\n", driver.quoted(name)));
            let (click, open_select) = ((id.clone(), database.clone()), select.clone());
            let (menu_view, menu_key, menu_select, menu_name) = (view.clone(), (id.clone(), database.clone()), select, name.clone());
            let icon = if *is_view { "side-icons/eye.svg" } else { "side-icons/table.svg" };
            row(format!("db-t-{id}-{database}-{name}").into(), 2, None, icon, muted, name.clone(), detail, cx)
                .test_support()
                .on_click(on(
                    view,
                    Rc::new(move |t, e, window, cx| {
                        if e.click_count() == 2 {
                            t.db_console(click.0.clone(), Some(click.1.clone()), open_select.clone(), window, cx);
                        }
                    }),
                ))
                .context_menu(move |menu, _, _| {
                    let (view, (id, db), select) = (menu_view.clone(), menu_key.clone(), menu_select.clone());
                    menu.when_some(select, |menu, select| {
                        menu.item(PopupMenuItem::new("Open in a console").on_click(move |_, window, cx| {
                            let _ = view.update(cx, |t, cx| t.db_console(id.clone(), Some(db.clone()), Some(select.clone()), window, cx));
                        }))
                    })
                    .item(crate::copying::item("Copy name", menu_name.clone()))
                })
                .into_any_element()
        }
        Line::Note { depth, text, failed, unlock } => {
            let color = if *failed { gpui_kit::red() } else { muted };
            let note = h_flex()
                .id(("db-note", ix))
                .h(px(LINE))
                .pl(px(8. + *depth as f32 * 14. + 24.))
                .pr_2()
                .text_xs()
                .text_color(color)
                .child(div().min_w_0().truncate().child(text.clone()));
            match unlock {
                Some(id) => {
                    let id = id.clone();
                    note.cursor_pointer().on_click(on(view, Rc::new(move |t, _, window, cx| t.db_unlock(id.clone(), window, cx)))).into_any_element()
                }
                // An error is cut to its line: the whole of it on the pointer.
                None if *failed => {
                    let whole = SharedString::from(text.clone());
                    note.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(whole.clone()).build(window, cx)).into_any_element()
                }
                None => note.into_any_element(),
            }
        }
    }
}
