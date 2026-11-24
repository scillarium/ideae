//! A connection's form: the database, and the bastion it is reached through.

use std::cell::RefCell;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::PopupMenuItem;

use super::*;
use crate::db::config::{self, Bastion, Connection, Driver, SshHop, Tls, TlsMode};

pub type Saved = Rc<dyn Fn(String, &mut Window, &mut App)>;

#[derive(Clone)]
struct Fields {
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    driver: Rc<std::cell::Cell<Driver>>,
    /// The bastion chosen; none is a direct connection.
    bastion: Rc<RefCell<Option<String>>>,
    tls: Rc<std::cell::Cell<TlsMode>>,
    ca: Entity<InputState>,
    color: Rc<std::cell::Cell<Option<u8>>>,
    production: Rc<std::cell::Cell<bool>>,
    said: Rc<RefCell<Option<(bool, String)>>>,
    /// Whether a password is kept for it, once the keyring has said.
    kept: Rc<RefCell<Option<crate::db::password::Kept>>>,
}

fn default_key() -> String {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(|home| PathBuf::from(home).join(".ssh").join("id_ed25519"))
        .filter(|key| key.is_file())
        .map(|key| key.display().to_string())
        .unwrap_or_default()
}

impl Fields {
    fn read(&self, id: String, cx: &App) -> Result<Connection, String> {
        let text = |field: &Entity<InputState>| field.read(cx).value().trim().to_string();
        let port = |field: &Entity<InputState>, name: &str, default: u16| {
            let typed = text(field);
            if typed.is_empty() { Ok(default) } else { typed.parse::<u16>().ok().filter(|p| *p > 0).ok_or(format!("{name} is not a port")) }
        };
        let host = text(&self.host);
        if host.is_empty() {
            return Err("A host is needed".into());
        }
        let bastion = self.bastion.borrow().clone();
        let ssh = bastion.as_deref().and_then(config::find_bastion).map(|b| b.hop);
        if bastion.is_some() && ssh.is_none() {
            return Err("That bastion is gone: choose another".into());
        }
        let ca = text(&self.ca);
        let name = text(&self.name);
        let database = text(&self.database);
        Ok(Connection {
            id,
            name: if name.is_empty() { host.clone() } else { name },
            driver: self.driver.get(),
            port: port(&self.port, "The port", self.driver.get().default_port())?,
            host,
            user: text(&self.user),
            database: (!database.is_empty()).then_some(database),
            bastion,
            ssh,
            tls: Tls { mode: self.tls.get(), ca: (!ca.is_empty()).then(|| PathBuf::from(ca)) },
            color: self.color.get(),
            production: self.production.get(),
        })
    }
}

/// Adds a connection (`id` none) or edits one; `saved` is told its id.
pub fn connection_dialog(id: Option<String>, saved: Option<Saved>, window: &mut Window, cx: &mut App) {
    let editing = id.as_deref().and_then(config::find);
    let input = |value: String, placeholder: &'static str, window: &mut Window, cx: &mut App| {
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        state.update(cx, |i, cx| i.set_value(value, window, cx));
        state
    };
    let e = editing.as_ref();
    let fields = Fields {
        name: input(e.map(|c| c.name.clone()).unwrap_or_default(), "Stage", window, cx),
        host: input(e.map(|c| c.host.clone()).unwrap_or_default(), "db-internal", window, cx),
        port: input(e.map_or(3306, |c| c.port).to_string(), "3306", window, cx),
        user: input(e.map(|c| c.user.clone()).unwrap_or_default(), "user", window, cx),
        password: input(String::new(), if e.is_some() { "Type to replace" } else { "Password" }, window, cx),
        database: input(e.and_then(|c| c.database.clone()).unwrap_or_default(), "Optional", window, cx),
        driver: Rc::new(std::cell::Cell::new(e.map_or(Driver::MySql, |c| c.driver))),
        bastion: Rc::new(RefCell::new(e.and_then(|c| c.bastion.clone()))),
        tls: Rc::new(std::cell::Cell::new(e.map_or(TlsMode::Prefer, |c| c.tls.mode))),
        ca: input(e.and_then(|c| c.tls.ca.as_ref()).map(|p| p.display().to_string()).unwrap_or_default(), "Optional: else the public authorities", window, cx),
        color: Rc::new(std::cell::Cell::new(e.and_then(|c| c.color))),
        production: Rc::new(std::cell::Cell::new(e.is_some_and(|c| c.production))),
        said: Rc::default(),
        kept: Rc::default(),
    };
    if let Some(connection) = e {
        let (asking, kept) = (crate::db::password::kept(&connection.id, &connection.name, cx), fields.kept.clone());
        window
            .spawn(cx, async move |cx| {
                *kept.borrow_mut() = Some(asking.await);
                let _ = cx.update(|window, _| window.refresh());
            })
            .detach();
    }
    let old_name = e.map(|c| c.name.clone());
    let was_production = e.is_some_and(|c| c.production);
    fields.password.update(cx, |i, cx| i.set_masked(true, window, cx));
    let the_id = id.clone().unwrap_or_else(config::new_id);
    let existing = id.is_some();
    let first = fields.name.clone();

    window.open_dialog(cx, move |dialog, _, _| {
        let store = {
            let (fields, the_id, saved, old_name) = (fields.clone(), the_id.clone(), saved.clone(), old_name.clone());
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let connection = match fields.read(the_id.clone(), cx) {
                    Ok(connection) => connection,
                    Err(why) => {
                        *fields.said.borrow_mut() = Some((false, why));
                        return window.refresh();
                    }
                };
                let password = fields.password.read(cx).value().to_string();
                let (id, name) = (connection.id.clone(), connection.name.clone());
                config::put(connection);
                crate::task::forget_in_panel(&id, cx);
                window.close_dialog(cx);
                let saved = saved.clone();
                if password.is_empty() {
                    if old_name.as_ref().is_some_and(|old| *old != name) {
                        crate::db::password::rename(&id, &name, cx).detach();
                    }
                    if let Some(saved) = saved {
                        saved(id, window, cx);
                    }
                    return;
                }
                // Told once the keyring has it: connecting reads it from there.
                let writing = crate::db::password::write(&id, &name, &password, cx);
                window
                    .spawn(cx, async move |cx| {
                        let failed = writing.await.err();
                        let _ = cx.update(|window, cx| {
                            if let Some(e) = failed {
                                window.push_notification(format!("The password was not saved: the system's keyring said {e}"), cx);
                            }
                            if let Some(saved) = saved {
                                saved(id, window, cx);
                            }
                        });
                    })
                    .detach();
            })
        };
        // A production connection is changed, or made a development one, only unlocked.
        let save: Rc<dyn Fn(&mut Window, &mut App)> = {
            let store = store.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                if was_production {
                    let store = store.clone();
                    super::sql_guard::production_gate(Rc::new(move |window, cx| store(window, cx)), window, cx);
                } else {
                    store(window, cx);
                }
            })
        };
        let test = {
            let (fields, the_id) = (fields.clone(), the_id.clone());
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                let connection = match fields.read(the_id.clone(), cx) {
                    Ok(connection) => connection,
                    Err(why) => {
                        *fields.said.borrow_mut() = Some((false, why));
                        return window.refresh();
                    }
                };
                *fields.said.borrow_mut() = Some((true, "Connecting…".into()));
                window.refresh();
                let typed = fields.password.read(cx).value().to_string();
                let stored = (typed.is_empty() && existing).then(|| crate::db::password::read_for(&connection, cx));
                let said = fields.said.clone();
                window
                    .spawn(cx, async move |cx| {
                        let password = match stored {
                            Some(reading) => match reading.await {
                                Ok(password) => password,
                                Err(e) => {
                                    *said.borrow_mut() = Some((false, e.to_string()));
                                    let _ = cx.update(|window, _| window.refresh());
                                    return;
                                }
                            },
                            None => (!typed.is_empty()).then_some(typed),
                        };
                        let product = connection.driver.name();
                        let outcome = crate::procs::on_thread(move || crate::db::session::test(&connection, password)).await;
                        *said.borrow_mut() = Some(match outcome {
                            Ok(version) => (true, format!("Connected: {product} {version}")),
                            Err(e) => (false, e.to_string()),
                        });
                        let _ = cx.update(|window, _| window.refresh());
                    })
                    .detach();
            }
        };
        let content_fields = fields.clone();
        dialog
            .title(if existing { "Edit connection" } else { "New connection" })
            .w(px(560.))
            .on_ok({
                let save = save.clone();
                move |_, window, cx| {
                    save(window, cx);
                    false
                }
            })
            .content(move |content, _, cx| {
                let f = content_fields.clone();
                let muted = cx.theme().muted_foreground;
                let label = |text: &'static str| div().text_sm().w(px(96.)).flex_none().child(text);
                let row = |name: &'static str, field: &Entity<InputState>| h_flex().gap_2().items_center().child(label(name)).child(div().flex_1().child(Input::new(field)));
                let with_port = |name: &'static str, field: &Entity<InputState>, port: &Entity<InputState>| {
                    h_flex().gap_2().items_center().child(label(name)).child(div().flex_1().child(Input::new(field))).child(div().text_sm().child("Port")).child(div().w(px(76.)).flex_none().child(Input::new(port)))
                };
                let chosen = f.bastion.borrow().clone();
                let through = chosen.as_deref().and_then(config::find_bastion);
                let on = through.is_some();
                let mode = f.tls.get();
                let said = f.said.borrow().clone();
                content.child(
                    v_flex()
                        .gap_2()
                        .child(h_flex().gap_1().items_center().child(label("Server")).children(Driver::ALL.into_iter().enumerate().map(|(ix, driver)| {
                            let (chosen, port) = (f.driver.clone(), f.port.clone());
                            Button::new(("sql-driver", ix)).xsmall().ghost().label(driver.name()).selected(chosen.get() == driver).on_click(move |_, window, cx| {
                                let before = chosen.replace(driver);
                                // A port that was the old server's own follows the new one.
                                if port.read(cx).value().trim() == before.default_port().to_string() {
                                    port.update(cx, |i, cx| i.set_value(driver.default_port().to_string(), window, cx));
                                }
                                window.refresh();
                            })
                        })))
                        .child(row("Name", &f.name))
                        .child(with_port("Host", &f.host, &f.port))
                        .child(row("User", &f.user))
                        .child(h_flex().gap_2().items_center().child(label("Password")).child(div().flex_1().child(Input::new(&f.password).mask_toggle())))
                        .child({
                            use crate::db::password::{Kept, keyring_name};
                            let typed = !f.password.read(cx).value().is_empty();
                            let (text, warn) = match f.kept.borrow().clone() {
                                _ if typed => (format!("Saved in {} when you press Save, never in a file.", keyring_name()), false),
                                Some(Kept::As(item)) => (format!("Saved in {} as “{item}”.", keyring_name()), false),
                                Some(Kept::Nothing) => (format!("No password saved. One typed here goes to {}.", keyring_name()), true),
                                Some(Kept::Unavailable(why)) => (format!("{} cannot be reached: {why}", keyring_name()), true),
                                Some(Kept::Unknown) => (format!("Kept in {}, never in a file.", keyring_name()), false),
                                None => (format!("Kept in {}, never in a file.", keyring_name()), false),
                            };
                            h_flex().gap_2().child(div().w(px(96.)).flex_none()).child(div().flex_1().text_xs().text_color(if warn { hsla(0.08, 0.9, 0.45, 1.) } else { muted }).child(text))
                        })
                        .child(row("Database", &f.database))
                        .child(h_flex().gap_1().items_center().child(label("Profile")).children([(false, "Development"), (true, "Production")].into_iter().map(|(on, name)| {
                            let production = f.production.clone();
                            Button::new(SharedString::from(format!("sql-profile-{name}")))
                                .xsmall()
                                .ghost()
                                .label(name)
                                .when(on, |b| b.text_color(gpui_kit::red()))
                                .selected(production.get() == on)
                                .on_click(move |_, window, _| {
                                    production.set(on);
                                    window.refresh();
                                })
                        })))
                        .when(f.production.get(), |this| {
                            this.child(h_flex().gap_2().child(div().w(px(96.)).flex_none()).child(div().flex_1().text_xs().text_color(muted).child(format!(
                                "Locked: nothing runs on it until you unlock production, which asks first, for {} minutes at a time.",
                                config::production_minutes()
                            ))))
                        })
                        .child(h_flex().gap_1().items_center().child(label("Color")).child({
                            let color = f.color.clone();
                            Button::new("sql-color-none").xsmall().ghost().label("None").selected(color.get().is_none()).on_click(move |_, window, _| {
                                color.set(None);
                                window.refresh();
                            })
                        }).children(config::COLORS.iter().enumerate().map(|(ix, (name, hue))| {
                            let color = f.color.clone();
                            Button::new(("sql-color", ix)).xsmall().ghost().label(*name).text_color(hsla(*hue, 0.75, 0.45, 1.)).selected(color.get() == Some(ix as u8)).on_click(move |_, window, _| {
                                color.set(Some(ix as u8));
                                window.refresh();
                            })
                        })))
                        .child(h_flex().gap_2().items_center().child(label("Connect")).child({
                            let (pick, new, picked) = (f.bastion.clone(), f.bastion.clone(), chosen.clone());
                            let shown = through.as_ref().map_or_else(|| "Directly".to_string(), |b| format!("Through {}", b.name));
                            Button::new("sql-bastion").small().outline().label(format!("{shown} ▾")).dropdown_menu(move |mut menu, _, _| {
                                let direct = pick.clone();
                                menu = menu.item(PopupMenuItem::new("Directly").checked(picked.is_none()).on_click(move |_, window, _| {
                                    *direct.borrow_mut() = None;
                                    window.refresh();
                                }));
                                let bastions = config::bastions();
                                if !bastions.is_empty() {
                                    menu = menu.separator();
                                }
                                for bastion in bastions {
                                    let (choose, id) = (pick.clone(), bastion.id.clone());
                                    menu = menu.item(PopupMenuItem::new(bastion.name.clone()).checked(picked.as_deref() == Some(bastion.id.as_str())).on_click(move |_, window, _| {
                                        *choose.borrow_mut() = Some(id.clone());
                                        window.refresh();
                                    }));
                                }
                                let new = new.clone();
                                menu.separator().item(PopupMenuItem::new("New bastion…").on_click(move |_, window, cx| {
                                    let new = new.clone();
                                    bastion_dialog(None, Some(Rc::new(move |id, window, _| {
                                        *new.borrow_mut() = Some(id);
                                        window.refresh();
                                    })), window, cx);
                                }))
                            })
                        }).when_some(chosen.clone(), |row, id| {
                            row.child(Button::new("sql-bastion-edit").xsmall().ghost().label("Edit bastion…").on_click(move |_, window, cx| {
                                bastion_dialog(Some(id.clone()), Some(Rc::new(|_, window, _| window.refresh())), window, cx);
                            }))
                        }))
                        .when(on, |this| this.child(div().text_xs().text_color(muted).child("The host above is as the bastion knows it.")))
                        .child(h_flex().gap_1().items_center().child(label("Encryption")).children(TlsMode::ALL.into_iter().enumerate().map(|(ix, option)| {
                            let tls = f.tls.clone();
                            Button::new(("sql-tls", ix)).xsmall().ghost().label(option.name()).selected(mode == option).on_click(move |_, window, _| {
                                tls.set(option);
                                window.refresh();
                            })
                        })))
                        .when(mode == TlsMode::Verify, |this| this.child(row("CA file", &f.ca)))
                        .child(div().text_xs().text_color(muted).child(match mode {
                            TlsMode::Off if on => "Not encrypted past the bastion.",
                            TlsMode::Off => "Not encrypted: what you read can be seen on the network.",
                            TlsMode::Prefer => "Encrypted when the server can do it, plain when it cannot.",
                            TlsMode::Require => "Always encrypted; the server's certificate is not checked.",
                            TlsMode::Verify => "Always encrypted, and the certificate has to be this server's.",
                        }))
                        .when_some(said, |this, (good, text)| this.child(div().id("sql-test-said").test_support().text_xs().text_color(if good { gpui_kit::green() } else { gpui_kit::red() }).child(text))),
                )
            })
            .footer(
                h_flex()
                    .justify_between()
                    .w_full()
                    .child(Button::new("sql-test").ghost().label("Test Connection").on_click(test))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("sql-dialog-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                            .child(Button::new("sql-dialog-save").primary().label("Save").on_click({
                                let save = save.clone();
                                move |_, window, cx| save(window, cx)
                            })),
                    ),
            )
    });
    first.update(cx, |i, cx| i.focus(window, cx));
}

/// Adds a bastion (`id` none) or edits one, for every connection going through it.
pub fn bastion_dialog(id: Option<String>, saved: Option<Saved>, window: &mut Window, cx: &mut App) {
    let editing = id.as_deref().and_then(config::find_bastion);
    let input = |value: String, placeholder: &'static str, window: &mut Window, cx: &mut App| {
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        state.update(cx, |i, cx| i.set_value(value, window, cx));
        state
    };
    let e = editing.as_ref();
    let name = input(e.map(|b| b.name.clone()).unwrap_or_default(), "Production bastion", window, cx);
    let host = input(e.map(|b| b.hop.host.clone()).unwrap_or_default(), "bastion.example.com", window, cx);
    let port = input(e.map_or(22, |b| b.hop.port).to_string(), "22", window, cx);
    let user = input(e.map(|b| b.hop.user.clone()).unwrap_or_default(), "user", window, cx);
    let key = input(e.map_or_else(default_key, |b| b.hop.key.as_ref().map(|k| k.display().to_string()).unwrap_or_default()), "ssh's own choice, or its agent", window, cx);
    let uses = id.as_deref().map_or(0, config::bastion_uses);
    let the_id = id.clone().unwrap_or_else(config::new_id);
    let said: Rc<RefCell<Option<String>>> = Rc::default();
    let first = if e.is_some() { name.clone() } else { host.clone() };
    window.open_dialog(cx, move |dialog, _, _| {
        let save = {
            let (name, host, port, user, key, the_id, saved, said) = (name.clone(), host.clone(), port.clone(), user.clone(), key.clone(), the_id.clone(), saved.clone(), said.clone());
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let text = |field: &Entity<InputState>| field.read(cx).value().trim().to_string();
                let (host_text, key_text, user_text) = (text(&host), text(&key), text(&user));
                let typed_port = text(&port);
                let parsed = if typed_port.is_empty() { Some(22) } else { typed_port.parse::<u16>().ok().filter(|p| *p > 0) };
                let problem = if host_text.is_empty() { Some("A host is needed") } else if parsed.is_none() { Some("That is not a port") } else { None };
                if let Some(problem) = problem {
                    *said.borrow_mut() = Some(problem.to_string());
                    return window.refresh();
                }
                let typed_name = text(&name);
                let login = if user_text.is_empty() { host_text.clone() } else { format!("{user_text}@{host_text}") };
                config::put_bastion(Bastion {
                    id: the_id.clone(),
                    name: if typed_name.is_empty() { login } else { typed_name },
                    hop: SshHop { host: host_text, port: parsed.unwrap_or(22), user: user_text, key: (!key_text.is_empty()).then(|| PathBuf::from(key_text)) },
                });
                window.close_dialog(cx);
                if let Some(saved) = &saved {
                    saved(the_id.clone(), window, cx);
                }
            })
        };
        let (name, host, port, user, key, said) = (name.clone(), host.clone(), port.clone(), user.clone(), key.clone(), said.clone());
        dialog
            .title(if uses > 0 { format!("Edit bastion ({uses} connections use it)") } else { "Bastion".to_string() })
            .w(px(520.))
            .on_ok({
                let save = save.clone();
                move |_, window, cx| {
                    save(window, cx);
                    false
                }
            })
            .content(move |content, _, cx| {
                let muted = cx.theme().muted_foreground;
                let label = |text: &'static str| div().text_sm().w(px(96.)).flex_none().child(text);
                let row = |text: &'static str, field: &Entity<InputState>| h_flex().gap_2().items_center().child(label(text)).child(div().flex_1().child(Input::new(field)));
                content.child(
                    v_flex()
                        .gap_2()
                        .child(row("Name", &name))
                        .child(h_flex().gap_2().items_center().child(label("Host")).child(div().flex_1().child(Input::new(&host))).child(div().text_sm().child("Port")).child(div().w(px(76.)).flex_none().child(Input::new(&port))))
                        .child(row("User", &user))
                        .child(row("Private key", &key))
                        .child(div().text_xs().text_color(muted).child("Runs your ssh: its agent, known_hosts and ~/.ssh/config apply. A key with a passphrase has to be in ssh-agent."))
                        .when_some(said.borrow().clone(), |this, text| this.child(div().text_xs().text_color(gpui_kit::red()).child(text))),
                )
            })
            .footer(h_flex().justify_end().gap_2().child(Button::new("sql-bastion-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                Button::new("sql-bastion-save").primary().label("Save").on_click({
                    let save = save.clone();
                    move |_, window, cx| save(window, cx)
                }),
            ))
    });
    first.update(cx, |i, cx| i.focus(window, cx));
}

type ImportList = Rc<RefCell<Option<Vec<(crate::db::import::Found, bool)>>>>;
/// What a look in a folder is doing, or how it ended.
type Looking = Rc<RefCell<Option<(bool, String)>>>;

/// The data sources of the projects in a folder chosen, added to the list
/// below those found already.
fn look_in_folder(list: ImportList, looking: Looking, window: &mut Window, cx: &mut App) {
    let picked = cx.prompt_for_paths(gpui_kit::PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Look here".into()) });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(folder) = paths.into_iter().next() else { return };
            *looking.borrow_mut() = Some((true, format!("Looking in {}…", folder.display())));
            let _ = cx.update(|window, _| window.refresh());
            let existing = config::list();
            let shown = folder.display().to_string();
            let (found, cut) = cx.background_spawn(async move { crate::db::import::find_in(&folder, &existing) }).await;
            let count = found.len();
            *looking.borrow_mut() = Some((
                false,
                if cut {
                    format!("{shown}: stopped after 20,000 folders, {count} data sources found so far. A smaller folder is looked through whole.")
                } else {
                    format!("{shown}: {count} data sources.")
                },
            ));
            {
                let mut list = list.borrow_mut();
                let items = list.get_or_insert_with(Vec::new);
                for f in found {
                    if !items.iter().any(|(i, _)| i.connection.id == f.connection.id) {
                        let take = !f.known;
                        items.push((f, take));
                    }
                }
            }
            let _ = cx.update(|window, _| window.refresh());
        })
        .detach();
}

/// What DataGrip knows, to bring over: never its passwords.
pub fn import_dialog(window: &mut Window, cx: &mut App) {
    let found: ImportList = Rc::default();
    let looking: Looking = Rc::default();
    let filling = found.clone();
    let existing = config::list();
    let reading = cx.background_spawn(async move { crate::db::import::find(&existing) });
    window
        .spawn(cx, async move |cx| {
            let list = reading.await;
            // Below what a folder looked in already brought, not over it.
            let mut filled = filling.borrow_mut();
            let items = filled.get_or_insert_with(Vec::new);
            for f in list {
                if !items.iter().any(|(i, _)| i.connection.id == f.connection.id) {
                    let take = !f.known;
                    items.push((f, take));
                }
            }
            drop(filled);
            let _ = cx.update(|window, _| window.refresh());
        })
        .detach();
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let (shown, taking, adding) = (found.clone(), found.clone(), found.clone());
        let (status, look) = (looking.clone(), looking.clone());
        dialog
            .title("Import from DataGrip")
            .w(px(640.))
            .content(move |content, _, _| {
                let list = shown.borrow();
                let body = match list.as_ref() {
                    None => div().text_sm().text_color(muted).child("Reading DataGrip's settings and the IDEs' projects…").into_any_element(),
                    Some(items) if items.is_empty() => div().text_sm().text_color(muted).child("No MySQL, PostgreSQL or Redis data source found.").into_any_element(),
                    Some(items) => v_flex()
                        .id("sql-import-list")
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .gap_1()
                        .children(items.iter().enumerate().map(|(ix, (item, take))| {
                            let c = &item.connection;
                            let through = c.ssh.as_ref().map(|h| format!(" · through {}", h.host)).unwrap_or_default();
                            let about = format!("{} · {}@{}:{}{through}{}", c.driver.name(), c.user, c.host, c.port, if item.known { " · already here" } else { "" });
                            let toggled = shown.clone();
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(gpui_kit::component::checkbox::Checkbox::new(("sql-import", ix)).checked(*take).on_click(move |checked, window, _| {
                                    if let Some(entry) = toggled.borrow_mut().as_mut().and_then(|l| l.get_mut(ix)) {
                                        entry.1 = *checked;
                                    }
                                    window.refresh();
                                }))
                                .child(v_flex().flex_1().min_w_0().child(div().text_sm().child(c.name.clone())).child(div().text_xs().text_color(muted).truncate().child(about)))
                        }))
                        .into_any_element(),
                };
                let status = status.borrow().clone();
                content.child(v_flex().gap_2().child(body).when_some(status, |this, (busy, text)| this.child(div().text_xs().text_color(if busy { muted } else { gpui_kit::green() }).child(text))).child(div().text_xs().text_color(muted).child("Passwords stay in DataGrip: each connection asks for its own once, in its form.")))
            })
            .footer(h_flex().justify_end().gap_2().child({
                Button::new("sql-import-folder")
                    .ghost()
                    .label("Look in a folder…")
                    .tooltip("The projects in a folder and under it, for those no IDE opened lately")
                    .disabled(look.borrow().as_ref().is_some_and(|(busy, _)| *busy))
                    .on_click({
                        let look = look.clone();
                        move |_, window, cx| look_in_folder(adding.clone(), look.clone(), window, cx)
                    })
            })
            .child(div().flex_1())
            .child(Button::new("sql-import-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                Button::new("sql-import-run").primary().label("Import").on_click(move |_, window, cx| {
                    let held = taking.clone();
                    let import: super::sql_guard::Then = Rc::new(move |window: &mut Window, cx: &mut App| {
                        let taking = held.clone();
                        if let Some(items) = taking.borrow().as_ref() {
                            for (item, take) in items {
                                if *take {
                                    // One already here keeps what DataGrip does not know of it:
                                    // its profile, mark and encryption.
                                    let mut connection = item.connection.clone();
                                    if let Some(here) = config::find(&connection.id) {
                                        connection.production = here.production;
                                        connection.color = here.color;
                                        connection.tls = here.tls;
                                    }
                                    config::put(connection);
                                    crate::task::forget_in_panel(&item.connection.id, cx);
                                }
                            }
                        }
                        window.close_dialog(cx);
                        window.refresh();
                    });
                    // Over a production connection, as in its own form: only unlocked.
                    let over_production = taking.borrow().as_ref().is_some_and(|items| {
                        items.iter().any(|(item, take)| *take && config::find(&item.connection.id).is_some_and(|here| here.production))
                    });
                    if over_production {
                        super::sql_guard::production_gate(import, window, cx);
                    } else {
                        import(window, cx);
                    }
                }),
            ))
    });
}
