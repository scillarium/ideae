//! The menu bar for a desktop that shows the focused app's menus itself
//! (GNOME Shell with a global menu extension): the menus of the ☰, exported
//! on the session bus as GTK exports an app's (`org.gtk.Menus`, their items'
//! actions as `org.gtk.Actions`), and where they are told with each window
//! (`gpui_linux::set_gtk_dbus`: gtk_shell1 on Wayland, `_GTK_*` properties
//! on X11). The ☰ stays: whether anything shows them cannot be known.
//!
//! The bus is spoken on a thread of its own; the menus are built on the UI
//! thread, while something is subscribed to them, again when the workspace
//! changes, and only what changed goes out.

use gpui_kit::App;

#[cfg(target_os = "linux")]
pub fn start(cx: &mut App) {
    imp::start(cx);
}

#[cfg(not(target_os = "linux"))]
pub fn start(_: &mut App) {}

/// A GTK accelerator (`<Control><Shift>n`) for a key as gpui names it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn gtk_key(key: &str) -> Option<String> {
    let named = match key {
        "enter" => "Return",
        "escape" => "Escape",
        "tab" => "Tab",
        "space" => "space",
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "insert" => "Insert",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Page_Up",
        "pagedown" => "Page_Down",
        "," => "comma",
        "." => "period",
        "/" => "slash",
        "\\" => "backslash",
        "-" => "minus",
        "=" => "equal",
        "+" => "plus",
        "[" => "bracketleft",
        "]" => "bracketright",
        ";" => "semicolon",
        "'" => "apostrophe",
        "`" => "grave",
        _ => {
            if let Some(n) = key.strip_prefix('f').filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
                return Some(format!("F{n}"));
            }
            let mut chars = key.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase().to_string()),
                _ => None,
            };
        }
    };
    Some(named.to_string())
}

/// A label as GTK reads one: `_` marks the letter of a mnemonic, so one
/// that is meant is doubled.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn gtk_label(name: &str) -> String {
    name.replace('_', "__")
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{gtk_key, gtk_label};
    use gpui_kit::{Action, App, Menu, MenuItem, Subscription, Window};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::{OwnedValue, Signature, Value};

    /// An item of a menu, as `org.gtk.Menus` tells one.
    #[derive(Clone, PartialEq, Debug)]
    pub(super) struct Item {
        label: Option<String>,
        action: Option<usize>,
        accel: Option<String>,
        submenu: Option<u32>,
        section: Option<u32>,
    }

    /// An action: whether it can be chosen, and its check mark if it has one.
    #[derive(Clone, Copy, PartialEq, Debug)]
    pub(super) struct State {
        enabled: bool,
        checked: Option<bool>,
    }

    /// The menus, all in group 0 (menu 0 the bar), and the actions of their items.
    #[derive(Clone, Default, PartialEq, Debug)]
    pub(super) struct Snapshot {
        menus: Vec<Vec<Item>>,
        actions: Vec<State>,
    }

    struct Shared {
        now: Mutex<Snapshot>,
        /// Something called Start and not End: the menus are kept up to date.
        subscribed: AtomicBool,
    }

    enum Ask {
        /// Built again; told back when the bus has it, if asked.
        Refresh(Option<async_channel::Sender<()>>),
        Activate(usize),
    }

    /// The menus as built, and who waits till the bus has them.
    struct Told {
        snapshot: Snapshot,
        done: Option<async_channel::Sender<()>>,
    }

    thread_local! {
        /// The actions of the items last told, by their number.
        static ACTIONS: RefCell<Vec<Box<dyn Action>>> = const { RefCell::new(Vec::new()) };
        static WATCH: RefCell<Option<Subscription>> = const { RefCell::new(None) };
        static PENDING: RefCell<bool> = const { RefCell::new(false) };
    }

    fn names() -> (String, String, String, String) {
        let app = crate::state::APP_ID.replace('-', "_");
        let bus = format!("io.github.scillarium.{app}.p{}", std::process::id());
        let path = format!("/io/github/scillarium/{app}");
        let menubar = format!("{path}/menus/menubar");
        (crate::state::APP_ID.to_string(), bus, path, menubar)
    }

    pub fn start(cx: &mut App) {
        let (application_id, bus, path, menubar) = names();
        // Before the window opens, which tells it: the name is the app's to
        // claim on the bus, done by the thread below.
        gpui_linux::set_gtk_dbus(gpui_linux::GtkDbus {
            application_id,
            bus_name: bus.clone(),
            application_path: path.clone(),
            menubar_path: menubar.clone(),
        });
        let shared = Arc::new(Shared { now: Mutex::new(Snapshot::default()), subscribed: AtomicBool::new(false) });
        let (ask_tx, ask_rx) = async_channel::unbounded::<Ask>();
        let (told_tx, told_rx) = async_channel::unbounded::<Told>();
        let on_bus = shared.clone();
        let spawned = std::thread::Builder::new().name("global-menu".into()).spawn(move || {
            if let Err(e) = serve(&bus, &path, &menubar, on_bus, ask_tx, told_rx) {
                // No session bus (a test's run, a bare session): no global menu.
                if std::env::var_os("IDEAE_DEBUG").is_some() {
                    eprintln!("global menu: {e}");
                }
            }
        });
        if spawned.is_err() {
            return;
        }
        cx.spawn(async move |cx| {
            while let Ok(ask) = ask_rx.recv().await {
                let shared = shared.clone();
                let told = told_tx.clone();
                cx.update(|cx| match ask {
                    Ask::Refresh(done) => {
                        watch(shared.clone(), told.clone(), cx);
                        refresh(&told, done, cx);
                    }
                    Ask::Activate(n) => {
                        if let Some(action) = ACTIONS.with_borrow(|a| a.get(n).map(|a| a.boxed_clone())) {
                            crate::menu::chosen_elsewhere(action, cx);
                        }
                    }
                });
            }
        })
        .detach();
    }

    /// While something is subscribed, the menus again after the workspace
    /// changed, a quarter of a second after the first change.
    fn watch(shared: Arc<Shared>, told: async_channel::Sender<Told>, cx: &mut App) {
        if WATCH.with_borrow(|w| w.is_some()) {
            return;
        }
        let Some(workspace) = cx.try_global::<crate::menu::Target>().and_then(|t| t.workspace.upgrade()) else { return };
        let subscription = cx.observe(&workspace, move |_, cx| {
            if !shared.subscribed.load(Ordering::Relaxed) || PENDING.with_borrow(|p| *p) {
                return;
            }
            PENDING.set(true);
            let told = told.clone();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                PENDING.set(false);
                let _ = cx.update(|cx| refresh(&told, None, cx));
            })
            .detach();
        });
        WATCH.set(Some(subscription));
    }

    fn refresh(told: &async_channel::Sender<Told>, done: Option<async_channel::Sender<()>>, cx: &mut App) {
        let Some(target) = cx.try_global::<crate::menu::Target>().map(|t| t.window) else { return };
        let built = target.update(cx, |_, window, cx| {
            let menus = crate::menu::live_menus(window, cx);
            build(&menus, window)
        });
        if let Ok((snapshot, actions)) = built {
            ACTIONS.set(actions);
            let _ = told.try_send(Told { snapshot, done });
        }
    }

    pub(super) fn build(menus: &[Menu], window: &Window) -> (Snapshot, Vec<Box<dyn Action>>) {
        let mut out = Snapshot { menus: vec![Vec::new()], actions: Vec::new() };
        let mut actions = Vec::new();
        let mut bar = Vec::new();
        for menu in menus.iter().filter(|m| !m.disabled) {
            let id = add_menu(&menu.items, &mut out, &mut actions, window);
            bar.push(Item { label: Some(gtk_label(&menu.name)), action: None, accel: None, submenu: Some(id), section: None });
        }
        out.menus[0] = bar;
        (out, actions)
    }

    /// A menu of these items, its number; parts between separators are
    /// sections, as GTK draws a line between them.
    fn add_menu(items: &[MenuItem], out: &mut Snapshot, actions: &mut Vec<Box<dyn Action>>, window: &Window) -> u32 {
        let id = out.menus.len() as u32;
        out.menus.push(Vec::new());
        let parts: Vec<&[MenuItem]> = items.split(|i| matches!(i, MenuItem::Separator)).filter(|p| !p.is_empty()).collect();
        let menu = if parts.len() > 1 {
            parts
                .iter()
                .map(|part| {
                    let section = out.menus.len() as u32;
                    out.menus.push(Vec::new());
                    let items = plain(part, out, actions, window);
                    out.menus[section as usize] = items;
                    Item { label: None, action: None, accel: None, submenu: None, section: Some(section) }
                })
                .collect()
        } else {
            plain(parts.first().copied().unwrap_or_default(), out, actions, window)
        };
        out.menus[id as usize] = menu;
        id
    }

    fn plain(items: &[MenuItem], out: &mut Snapshot, actions: &mut Vec<Box<dyn Action>>, window: &Window) -> Vec<Item> {
        let mut menu = Vec::new();
        for item in items {
            match item {
                MenuItem::Action { name, action, checked, disabled, .. } => {
                    let n = actions.len();
                    actions.push(action.boxed_clone());
                    // Checkable or not is not told by an item: one that is checked now has a mark.
                    out.actions.push(State { enabled: !disabled, checked: checked.then_some(true) });
                    menu.push(Item { label: Some(gtk_label(name)), action: Some(n), accel: accel(&**action, window), submenu: None, section: None });
                }
                MenuItem::Submenu(sub) if !sub.disabled => {
                    let id = add_menu(&sub.items, out, actions, window);
                    menu.push(Item { label: Some(gtk_label(&sub.name)), action: None, accel: None, submenu: Some(id), section: None });
                }
                _ => {}
            }
        }
        menu
    }

    fn accel(action: &dyn Action, window: &Window) -> Option<String> {
        let binding = window.highest_precedence_binding_for_action(action)?;
        let [stroke] = binding.keystrokes() else { return None };
        let key = gtk_key(stroke.key())?;
        let m = stroke.modifiers();
        let mut accel = String::new();
        for (on, name) in [(m.control, "<Control>"), (m.shift, "<Shift>"), (m.alt, "<Alt>"), (m.platform, "<Super>")] {
            if on {
                accel.push_str(name);
            }
        }
        accel.push_str(&key);
        Some(accel)
    }

    // ------------------------------------------------------------ the bus

    fn action_name(n: usize) -> String {
        format!("m{n}")
    }

    fn item_dict(item: &Item) -> HashMap<String, OwnedValue> {
        let mut dict = HashMap::new();
        let mut put = |key: &str, value: Value<'_>| {
            if let Ok(value) = OwnedValue::try_from(value) {
                dict.insert(key.to_string(), value);
            }
        };
        if let Some(label) = &item.label {
            put("label", Value::from(label.as_str()));
        }
        if let Some(n) = item.action {
            put("action", Value::from(format!("app.{}", action_name(n))));
        }
        if let Some(accel) = &item.accel {
            put("accel", Value::from(accel.as_str()));
        }
        if let Some(menu) = item.submenu {
            put(":submenu", Value::from((0u32, menu)));
        }
        if let Some(menu) = item.section {
            put(":section", Value::from((0u32, menu)));
        }
        dict
    }

    type Described = (bool, Signature, Vec<OwnedValue>);

    fn describe(state: State) -> Described {
        let checked = state.checked.map(|c| OwnedValue::from(c)).into_iter().collect();
        (state.enabled, Signature::Unit, checked)
    }

    struct Menus {
        shared: Arc<Shared>,
        ask: async_channel::Sender<Ask>,
    }

    #[zbus::interface(name = "org.gtk.Menus")]
    impl Menus {
        async fn start(&self, groups: Vec<u32>) -> Vec<(u32, u32, Vec<HashMap<String, OwnedValue>>)> {
            use futures_lite::FutureExt as _;
            self.shared.subscribed.store(true, Ordering::Relaxed);
            // The menus as they are now, if the window builds them soon; else as last built.
            let (done, built) = async_channel::bounded(1);
            if self.ask.try_send(Ask::Refresh(Some(done))).is_ok() {
                let waited = async {
                    let _ = built.recv().await;
                }
                .or(async {
                    async_io::Timer::after(std::time::Duration::from_millis(500)).await;
                });
                waited.await;
            }
            if !groups.contains(&0) {
                return Vec::new();
            }
            let now = self.shared.now.lock().unwrap_or_else(|e| e.into_inner());
            now.menus.iter().enumerate().map(|(id, items)| (0, id as u32, items.iter().map(item_dict).collect())).collect()
        }

        async fn end(&self, _groups: Vec<u32>) {
            self.shared.subscribed.store(false, Ordering::Relaxed);
        }

        #[zbus(signal)]
        async fn changed(emitter: &SignalEmitter<'_>, changes: Vec<(u32, u32, u32, u32, Vec<HashMap<String, OwnedValue>>)>) -> zbus::Result<()>;
    }

    struct Actions {
        shared: Arc<Shared>,
        ask: async_channel::Sender<Ask>,
    }

    impl Actions {
        fn find(&self, name: &str) -> Option<(usize, State)> {
            let n: usize = name.strip_prefix('m')?.parse().ok()?;
            let now = self.shared.now.lock().unwrap_or_else(|e| e.into_inner());
            now.actions.get(n).map(|s| (n, *s))
        }
    }

    #[zbus::interface(name = "org.gtk.Actions")]
    impl Actions {
        async fn list(&self) -> Vec<String> {
            let now = self.shared.now.lock().unwrap_or_else(|e| e.into_inner());
            (0..now.actions.len()).map(action_name).collect()
        }

        async fn describe(&self, name: String) -> zbus::fdo::Result<Described> {
            self.find(&name).map(|(_, s)| describe(s)).ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no action {name}")))
        }

        async fn describe_all(&self) -> HashMap<String, Described> {
            let now = self.shared.now.lock().unwrap_or_else(|e| e.into_inner());
            now.actions.iter().enumerate().map(|(n, s)| (action_name(n), describe(*s))).collect()
        }

        async fn activate(&self, name: String, _parameter: Vec<OwnedValue>, _platform_data: HashMap<String, OwnedValue>) {
            if let Some((n, state)) = self.find(&name)
                && state.enabled
            {
                let _ = self.ask.try_send(Ask::Activate(n));
            }
        }

        /// A check item's mark set: the app's own action toggles it.
        async fn set_state(&self, name: String, _value: OwnedValue, _platform_data: HashMap<String, OwnedValue>) {
            if let Some((n, state)) = self.find(&name)
                && state.enabled
            {
                let _ = self.ask.try_send(Ask::Activate(n));
            }
        }

        #[zbus(signal)]
        async fn changed(
            emitter: &SignalEmitter<'_>,
            removals: Vec<String>,
            enable_changes: HashMap<String, bool>,
            state_changes: HashMap<String, OwnedValue>,
            additions: HashMap<String, Described>,
        ) -> zbus::Result<()>;
    }

    fn serve(
        bus: &str,
        path: &str,
        menubar: &str,
        shared: Arc<Shared>,
        ask: async_channel::Sender<Ask>,
        told: async_channel::Receiver<Told>,
    ) -> zbus::Result<()> {
        let conn = zbus::blocking::connection::Builder::session()?
            .name(bus)?
            .serve_at(path, Actions { shared: shared.clone(), ask: ask.clone() })?
            .serve_at(menubar, Menus { shared: shared.clone(), ask })?
            .build()?;
        let server = conn.object_server();
        let menus = server.interface::<_, Menus>(menubar)?;
        let actions = server.interface::<_, Actions>(path)?;
        while let Ok(Told { snapshot: next, done }) = told.recv_blocking() {
            let before = std::mem::replace(&mut *shared.now.lock().unwrap_or_else(|e| e.into_inner()), next.clone());
            if let Some(done) = done {
                let _ = done.try_send(());
            }
            let (menu_changes, action_changes) = diff(&before, &next);
            if !menu_changes.is_empty() {
                zbus::block_on(Menus::changed(menus.signal_emitter(), menu_changes))?;
            }
            if let Some((removals, enabled, states, additions)) = action_changes {
                zbus::block_on(Actions::changed(actions.signal_emitter(), removals, enabled, states, additions))?;
            }
        }
        Ok(())
    }

    type MenuChanges = Vec<(u32, u32, u32, u32, Vec<HashMap<String, OwnedValue>>)>;
    type ActionChanges = (Vec<String>, HashMap<String, bool>, HashMap<String, OwnedValue>, HashMap<String, Described>);

    /// What changed, as the two Changed signals tell it: a menu that
    /// differs is replaced whole; an action that gained or lost its check
    /// mark is removed and added again.
    pub(super) fn diff(before: &Snapshot, after: &Snapshot) -> (MenuChanges, Option<ActionChanges>) {
        let mut menus = Vec::new();
        for id in 0..before.menus.len().max(after.menus.len()) {
            let (old, new) = (before.menus.get(id), after.menus.get(id));
            if old != new {
                let removed = old.map_or(0, |o| o.len()) as u32;
                let added = new.map(|n| n.iter().map(item_dict).collect()).unwrap_or_default();
                menus.push((0, id as u32, 0, removed, added));
            }
        }
        let (mut removals, mut enabled, mut states, mut additions) = (Vec::new(), HashMap::new(), HashMap::new(), HashMap::new());
        for n in 0..before.actions.len().max(after.actions.len()) {
            match (before.actions.get(n), after.actions.get(n)) {
                (Some(_), None) => removals.push(action_name(n)),
                (None, Some(new)) => {
                    additions.insert(action_name(n), describe(*new));
                }
                (Some(old), Some(new)) if old.checked.is_some() != new.checked.is_some() => {
                    removals.push(action_name(n));
                    additions.insert(action_name(n), describe(*new));
                }
                (Some(old), Some(new)) => {
                    if old.enabled != new.enabled {
                        enabled.insert(action_name(n), new.enabled);
                    }
                    if let (Some(a), Some(b)) = (old.checked, new.checked)
                        && a != b
                    {
                        states.insert(action_name(n), OwnedValue::from(b));
                    }
                }
                (None, None) => {}
            }
        }
        let actions = (!(removals.is_empty() && enabled.is_empty() && states.is_empty() && additions.is_empty())).then_some((removals, enabled, states, additions));
        (menus, actions)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn item(label: &str, action: Option<usize>) -> Item {
            Item { label: Some(label.into()), action, accel: None, submenu: None, section: None }
        }

        #[test]
        fn only_what_changed_is_told() {
            let before = Snapshot { menus: vec![vec![item("File", None)], vec![item("New", Some(0)), item("Quit", Some(1))]], actions: vec![State { enabled: true, checked: None }, State { enabled: true, checked: Some(true) }] };
            assert_eq!(diff(&before, &before.clone()).0.len(), 0);
            assert!(diff(&before, &before).1.is_none());
            let mut after = before.clone();
            after.menus[1][0].label = Some("New task".into());
            after.actions[0].enabled = false;
            let (menus, actions) = diff(&before, &after);
            assert_eq!(menus.iter().map(|m| (m.1, m.3, m.4.len())).collect::<Vec<_>>(), [(1, 2, 2)], "the menu that changed, replaced whole");
            let (removals, enabled, states, additions) = actions.unwrap();
            assert_eq!((removals.len(), enabled.get("m0"), states.len(), additions.len()), (0, Some(&false), 0, 0));
            // A check mark gone: no mark is no state, so the action is told again.
            let mut unchecked = before.clone();
            unchecked.actions[1].checked = None;
            let (removals, _, _, additions) = diff(&before, &unchecked).1.unwrap();
            assert_eq!((removals, additions.contains_key("m1")), (vec!["m1".to_string()], true));
        }

        #[test]
        fn an_item_tells_its_parts() {
            let dict = item_dict(&Item { label: Some("Open_file".into()), action: Some(3), accel: Some("<Control>o".into()), submenu: Some(4), section: None });
            assert_eq!(dict.get("action").and_then(|v| String::try_from(v.clone()).ok()).as_deref(), Some("app.m3"));
            assert!(dict.contains_key(":submenu") && dict.contains_key("accel") && !dict.contains_key(":section"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_labels_as_gtk_reads_them() {
        assert_eq!(gtk_key("n").as_deref(), Some("n"));
        assert_eq!(gtk_key("N").as_deref(), Some("n"));
        assert_eq!(gtk_key("f12").as_deref(), Some("F12"));
        assert_eq!(gtk_key(",").as_deref(), Some("comma"));
        assert_eq!(gtk_key("pageup").as_deref(), Some("Page_Up"));
        assert_eq!(gtk_key("é"), None);
        assert_eq!(gtk_key("f"), Some("f".into()));
        assert_eq!(gtk_label("Save_as"), "Save__as");
    }
}
