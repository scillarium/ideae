//! The Database panel: the connections, the databases of one opened and the
//! tables of a database opened, read on opening and kept for every task
//! until Refresh; a console on any of them is a scratch file on it.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::menu::PopupMenuItem;

use super::*;
use crate::db::{browse, config, schema::TableMeta};

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

/// What the panel has read, the same in every task.
#[derive(Default)]
struct DbTree {
    open: HashSet<String>,
    open_databases: HashSet<(String, String)>,
    databases: HashMap<String, Load<Vec<String>>>,
    tables: HashMap<(String, String), Load<Vec<TableMeta>>>,
    turn: u64,
}

impl DbTree {
    fn next(&mut self) -> u64 {
        self.turn += 1;
        self.turn
    }
}

fn locked_or_keyring(e: anyhow::Error) -> String {
    if e.to_string() == crate::db::guard::LOCKED { e.to_string() } else { format!("The keyring: {e}") }
}

fn load_databases(id: String, fresh: bool, cx: &mut App) {
    let Some(connection) = config::find(&id) else { return };
    let tree = cx.default_global::<DbTree>();
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
            let tree = cx.default_global::<DbTree>();
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
    let tree = cx.default_global::<DbTree>();
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
        cx.update(|cx| {
            let tree = cx.default_global::<DbTree>();
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
    let locked = |e: &String| e == crate::db::guard::LOCKED;
    let databases: Vec<String> = tree.databases.iter().filter(|(_, l)| matches!(l, Load::Failed(e) if locked(e))).map(|(id, _)| id.clone()).collect();
    let tables: Vec<(String, String)> = tree.tables.iter().filter(|(_, l)| matches!(l, Load::Failed(e) if locked(e))).map(|(k, _)| k.clone()).collect();
    for id in databases {
        load_databases(id, true, cx);
    }
    for (id, database) in tables {
        load_tables(id, database, true, cx);
    }
}

impl Global for DbTree {}

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
    let tree = cx.default_global::<DbTree>();
    tree.databases.remove(id);
    tree.tables.retain(|(of, _), _| of != id);
    tree.open.remove(id);
    tree.open_databases.retain(|(of, _)| of != id);
    cx.refresh_windows();
}

impl TaskView {
    /// Opened, a locked production connection says so under it, and is read once unlocked.
    fn db_toggle(&mut self, id: String, cx: &mut Context<Self>) {
        let tree = cx.default_global::<DbTree>();
        if tree.open.remove(&id) {
            return cx.notify();
        }
        tree.open.insert(id.clone());
        // Its databases still marked open are read with it, not left "Reading…".
        let databases: Vec<String> = tree.open_databases.iter().filter(|(of, _)| *of == id).map(|(_, d)| d.clone()).collect();
        load_databases(id.clone(), false, cx);
        for database in databases {
            load_tables(id.clone(), database, false, cx);
        }
        cx.notify();
    }

    /// The question, then everything of that connection read again.
    fn db_unlock(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if crate::db::guard::is_open() {
            return self.db_refresh(Some(id), cx);
        }
        let this = cx.weak_entity();
        let then: super::sql_guard::Then = Rc::new(move |_, cx| {
            let id = id.clone();
            let _ = this.update(cx, |t, cx| t.db_refresh(Some(id), cx));
        });
        super::sql_guard::production_gate(then, window, cx);
    }

    fn db_toggle_database(&mut self, id: String, database: String, cx: &mut Context<Self>) {
        let key = (id, database);
        let tree = cx.default_global::<DbTree>();
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
        let tree = cx.default_global::<DbTree>();
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

    fn db_row(&self, id: SharedString, depth: usize, open: Option<bool>, icon: &'static str, color: Hsla, name: String, detail: Option<String>, cx: &App) -> Stateful<Div> {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .id(id)
            .w_full()
            .pl(px(8. + depth as f32 * 14.))
            .pr_2()
            .py(px(3.))
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

    /// The heading of the development or the production connections; the
    /// latter's says whether they are locked, and a click turns it.
    fn db_group(&self, production: bool, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let heading = div().px_3().pt_2().pb_1().text_xs();
        if !production {
            return heading.text_color(muted).child("DEVELOPMENT").into_any_element();
        }
        let until = crate::db::guard::open_until();
        h_flex()
            .id("db-group-production")
            .test_support()
            .px_3()
            .pt_3()
            .pb_1()
            .gap_2()
            .text_xs()
            .cursor_pointer()
            .child(div().text_color(gpui_kit::red()).child("PRODUCTION"))
            .child(div().text_color(muted).child(match until {
                Some(until) => format!("unlocked until {} · click to lock", crate::local_time::hour_minute(until)),
                None => "locked · click to unlock".into(),
            }))
            .on_click(cx.listener(|_, _, window, cx| {
                if crate::db::guard::is_open() {
                    super::sql_guard::lock_now(cx);
                } else {
                    super::sql_guard::production_gate(Rc::new(|_, _| {}), window, cx);
                }
            }))
            .into_any_element()
    }

    fn db_note(depth: usize, text: String, color: Hsla) -> Div {
        div().pl(px(8. + depth as f32 * 14. + 24.)).pr_2().py(px(3.)).text_xs().text_color(color).child(text)
    }

    pub(super) fn render_db_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut connections = config::list();
        if connections.is_empty() {
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
        connections.sort_by_key(|c| (c.production, c.name.to_lowercase()));
        let grouped = connections.iter().any(|c| c.production);
        let mut group = None;
        let mut headings = [Some(self.db_group(false, cx)), Some(self.db_group(true, cx))];
        let empty = DbTree::default();
        let tree = cx.try_global::<DbTree>().unwrap_or(&empty);
        let view = cx.weak_entity();
        let mut list = v_flex().id("db-panel").size_full().overflow_y_scroll().py_1().text_sm();
        for connection in connections {
            if grouped && group != Some(connection.production) {
                group = Some(connection.production);
                list = list.children(headings[usize::from(connection.production)].take());
            }
            let id = connection.id.clone();
            let open = tree.open.contains(&id);
            let detail = format!("{} · {}", connection.driver.name(), connection.host);
            let color = hue(connection.color).unwrap_or(if connection.production { gpui_kit::red() } else { cx.theme().primary });
            let (on_click, on_menu) = (id.clone(), id.clone());
            let menu_view = view.clone();
            list = list.child(
                self.db_row(format!("db-c-{id}").into(), 0, Some(open), "side-icons/database.svg", color, connection.name.clone(), Some(detail), cx)
                    .test_support()
                    .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                        // The first click of a double one opened or closed it: put back.
                        this.db_toggle(on_click.clone(), cx);
                        if e.click_count() == 2 {
                            this.db_console(on_click.clone(), None, None, window, cx);
                        }
                    }))
                    .context_menu(move |menu, _, _| {
                        let (view, id) = (menu_view.clone(), on_menu.clone());
                        let (v2, i2, i3) = (view.clone(), id.clone(), id.clone());
                        menu.item(PopupMenuItem::new("New console").on_click(move |_, window, cx| {
                            let _ = view.update(cx, |t, cx| t.db_console(id.clone(), None, None, window, cx));
                        }))
                        .item(PopupMenuItem::new("Refresh").on_click(move |_, _, cx| {
                            let _ = v2.update(cx, |t, cx| t.db_refresh(Some(i2.clone()), cx));
                        }))
                        .separator()
                        .item(PopupMenuItem::new("Edit connection…").on_click(move |_, window, cx| Self::db_edit(Some(i3.clone()), window, cx)))
                    }),
            );
            if !open {
                continue;
            }
            match tree.databases.get(&id) {
                None | Some(Load::Loading(_)) => list = list.child(Self::db_note(0, "Connecting…".into(), muted)),
                Some(Load::Failed(e)) if *e == crate::db::guard::LOCKED => {
                    let unlock = id.clone();
                    list = list.child(
                        Self::db_note(0, "Production is locked · click to unlock".into(), gpui_kit::red())
                            .id(SharedString::from(format!("db-locked-{id}")))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| this.db_unlock(unlock.clone(), window, cx))),
                    )
                }
                Some(Load::Failed(e)) => list = list.child(Self::db_note(0, e.clone(), gpui_kit::red())),
                Some(Load::Ready(databases)) => {
                    for database in databases {
                        let key = (id.clone(), database.clone());
                        let open = tree.open_databases.contains(&key);
                        let (on_click, on_menu) = (key.clone(), key.clone());
                        let menu_view = view.clone();
                        list = list.child(
                            self.db_row(format!("db-d-{id}-{database}").into(), 1, Some(open), "side-icons/database.svg", muted, database.clone(), None, cx)
                                .test_support()
                                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                                    let (id, database) = on_click.clone();
                                    this.db_toggle_database(id.clone(), database.clone(), cx);
                                    if e.click_count() == 2 {
                                        this.db_console(id, Some(database), None, window, cx);
                                    }
                                }))
                                .context_menu(move |menu, _, _| {
                                    let (view, (id, database)) = (menu_view.clone(), on_menu.clone());
                                    menu.item(PopupMenuItem::new("New console").on_click(move |_, window, cx| {
                                        let _ = view.update(cx, |t, cx| t.db_console(id.clone(), Some(database.clone()), None, window, cx));
                                    }))
                                    .item(crate::copying::item("Copy name", on_menu.1.clone()))
                                }),
                        );
                        if open {
                            list = self.db_tables(list, &connection, &database, tree, cx);
                        }
                    }
                }
            }
        }
        list.into_any_element()
    }

    fn db_tables(&self, mut list: Stateful<Div>, connection: &config::Connection, database: &str, tree: &DbTree, cx: &Context<Self>) -> Stateful<Div> {
        let muted = cx.theme().muted_foreground;
        let key = (connection.id.clone(), database.to_string());
        let tables = match tree.tables.get(&key) {
            None | Some(Load::Loading(_)) => return list.child(Self::db_note(1, "Reading…".into(), muted)),
            Some(Load::Failed(e)) if *e == crate::db::guard::LOCKED => {
                let unlock = connection.id.clone();
                return list.child(
                    Self::db_note(1, "Production is locked · click to unlock".into(), gpui_kit::red())
                        .id(SharedString::from(format!("db-locked-{}-{database}", connection.id)))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| this.db_unlock(unlock.clone(), window, cx))),
                );
            }
            Some(Load::Failed(e)) => return list.child(Self::db_note(1, e.clone(), gpui_kit::red())),
            Some(Load::Ready(tables)) => tables,
        };
        if tables.is_empty() {
            return list.child(Self::db_note(1, "No tables".into(), muted));
        }
        let redis = connection.driver == config::Driver::Redis;
        let view = cx.weak_entity();
        for table in tables {
            let detail = (!redis).then(|| format!("{}{}", table.columns.len(), if table.view { " · view" } else { "" }));
            let select = (!redis).then(|| format!("SELECT * FROM {} LIMIT 100;\n", connection.driver.quoted(&table.name)));
            let (id, db) = (connection.id.clone(), database.to_string());
            let (menu_view, menu_select, name) = (view.clone(), select.clone(), table.name.clone());
            let (menu_id, menu_db) = (id.clone(), db.clone());
            list = list.child(
                self.db_row(format!("db-t-{id}-{db}-{}", table.name).into(), 2, None, if table.view { "side-icons/eye.svg" } else { "side-icons/table.svg" }, muted, table.name.clone(), detail, cx)
                    .test_support()
                    .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                        if e.click_count() == 2 {
                            this.db_console(id.clone(), Some(db.clone()), select.clone(), window, cx);
                        }
                    }))
                    .context_menu(move |menu, _, _| {
                        let (view, select, id, db) = (menu_view.clone(), menu_select.clone(), menu_id.clone(), menu_db.clone());
                        menu.when_some(select, |menu, select| {
                            menu.item(PopupMenuItem::new("Open in a console").on_click(move |_, window, cx| {
                                let _ = view.update(cx, |t, cx| t.db_console(id.clone(), Some(db.clone()), Some(select.clone()), window, cx));
                            }))
                        })
                        .item(crate::copying::item("Copy name", name.clone()))
                    }),
            );
        }
        list
    }
}
