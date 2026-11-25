//! The menu bar: macOS keeps it at the top of the screen; on Linux and
//! Windows it is in the title bar, behind ☰ or always shown, drawn from the
//! same menus. Kept short: the tasks, their git and the window, not the
//! editor's commands. The keys shown are
//! those in force. An item is grey where it would do nothing: no handler on
//! the focus path, nothing for the task or git to do, a dialog up. The menus
//! are made once; what each item says, and whether it is grey or checked, is
//! told when its menu opens.

use gpui_kit::component::{WindowExt as _, input, menu::AppMenuBar};
use gpui_kit::{App, Context, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, Window, actions};

use crate::commands::{GroupCommand, InHand, ProjectCommand, TaskCommand, git_name};
use crate::task::{FrontTab, GitCommand, RightPane};
use crate::workspace::Workspace;
use crate::{keymap, state};

#[derive(Clone, Default)]
struct Shown {
    /// A dialog is up: what would open another, or act under it, is grey.
    dialog: bool,
    /// The switches of Settings the View menu has, as they are.
    switches: Switches,
    power_saving: bool,
    presenting: bool,
    focus_mode: bool,
    projects: bool,
    project: bool,
    /// Variants in hand: the task's, or those side by side in the window.
    group: bool,
    showing_group: bool,
    /// An agent of the group runs; every one has finished.
    group_settled: bool,
    judging: bool,
    task: Option<InHand>,
    /// Find and Replace has an editor to act in.
    replace: bool,
    spaces: Vec<String>,
    space: Option<String>,
    /// What the focused view takes, which AppKit tells by itself on macOS:
    /// Edit's items, Save, Find.
    edit: bool,
    save: bool,
    find: bool,
    /// What only the in-window menu bar shows: macOS has its own for these.
    zoomed: bool,
    fullscreen: bool,
    maximized: bool,
    /// Pinned to the dash; none where there is no dash to pin to.
    /// Where there is a GNOME dash: whether the app is pinned to it. And, where an app has to be listed to be found: whether it is.
    pinned: Option<bool>,
    listed: Option<bool>,
}

#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = ideae, no_json)]
pub struct OnTask(pub TaskCommand);

#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = ideae, no_json)]
pub struct OnProject(pub ProjectCommand);

#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = ideae, no_json)]
pub struct OnGroup(pub GroupCommand);

#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = ideae, no_json)]
pub struct Git(pub GitCommand);

#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = ideae, no_json)]
pub struct GoToSpace(pub Option<String>);

/// A setting turned on or off from the View menu.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Switch {
    TypeHints,
    EditPrediction,
    BlameAtLineEnd,
    IgnoreWhitespace,
    DiffFiles,
}

#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = ideae, no_json)]
pub struct Toggle(pub Switch);

#[derive(Clone, Copy, Default)]
struct Switches {
    type_hints: bool,
    edit_prediction: bool,
    blame_at_line_end: bool,
    ignore_whitespace: bool,
    diff_files: bool,
}

impl Switches {
    fn now() -> Self {
        state::with_settings(|s| Switches {
            type_hints: s.inlay_hints,
            edit_prediction: s.edit_prediction == Some(true),
            blame_at_line_end: s.blame_at_line_end,
            ignore_whitespace: s.diff_ignore_whitespace,
            diff_files: s.diff_files,
        })
    }
}

/// The setting flipped as Settings would: what it changes changes at once
/// (hints, the prediction server, diffs read again).
fn toggle(what: Switch, w: &mut Workspace, cx: &mut Context<Workspace>) {
    let mut new = state::settings();
    match what {
        Switch::TypeHints => new.inlay_hints = !new.inlay_hints,
        // Turned on here is consent to its download, as the offer's "Turn on".
        Switch::EditPrediction => new.edit_prediction = Some(new.edit_prediction != Some(true)),
        Switch::BlameAtLineEnd => new.blame_at_line_end = !new.blame_at_line_end,
        Switch::IgnoreWhitespace => new.diff_ignore_whitespace = !new.diff_ignore_whitespace,
        Switch::DiffFiles => new.diff_files = !new.diff_files,
    }
    w.apply_settings(new, cx);
    cx.refresh_windows();
}

impl Workspace {
    fn shown(&self, dialog: bool, window: &Window, cx: &App) -> Shown {
        Shown {
            dialog,
            switches: Switches::now(),
            replace: window.is_action_available(&input::Replace, cx),
            spaces: self.spaces.iter().map(|s| s.name.clone()).collect(),
            space: self.space.clone(),
            power_saving: crate::lsp::POWER_SAVING.load(std::sync::atomic::Ordering::Relaxed),
            presenting: self.is_presenting(),
            focus_mode: self.active_view().is_some_and(|t| t.read(cx).focus_mode()),
            projects: self.project_in_hand().or(self.first_project()).is_some(),
            project: self.project_in_hand().is_some(),
            group: self.group_in_hand().is_some(),
            showing_group: self.active_group.is_some(),
            judging: self.group_in_hand().is_some_and(|g| self.is_judging(g)),
            group_settled: self.group_in_hand().is_some_and(|g| self.group_settled(g, cx)),
            task: self.in_hand(cx),
            edit: window.is_action_available(&input::Copy, cx),
            save: keymap::find("save").is_some_and(|s| window.is_action_available(s.action().as_ref(), cx)),
            find: window.is_action_available(&input::Search, cx),
            zoomed: crate::zoom::any_set(cx),
            fullscreen: window.is_fullscreen(),
            maximized: window.is_maximized(),
            pinned: self.desk.dash.then_some(self.desk.pinned),
            listed: crate::desktop::can_list().then_some(self.desk.listed),
        }
    }

    /// The task takes the keyboard for an item sent where the workspace is
    /// not on the way: focus on nothing, or on a view no longer drawn. Not
    /// from a dialog, which keeps it.
    fn focus_task_unless_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.has_active_dialog(cx)
            && let Some(view) = self.active_view()
        {
            view.update(cx, |t, cx| t.focus_active(window, cx));
        }
    }
}

actions!(
    ideae,
    [
        FindAndReplace, AddProject, AddRemoteProject, PresentationMode, PowerSaving, Restart, CheckForUpdates, KeyboardShortcuts, About, BringAllToFront, CloseWindow, Documentation, Hide, HideOthers, ShowAll,
        Minimize, ZoomWindow, FullScreen, ResetTextSize, PinToDash, ListApp, OpenMenuBar
    ]
);

pub fn init(cx: &mut App) {
    if cfg!(target_os = "macos") {
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    } else {
        // Alt pressed and let go alone: gpui tells it as a keystroke of its own,
        // so Alt with another key is still that key's.
        cx.bind_keys([alt_alone()]);
        cx.on_action(|_: &OpenMenuBar, cx| toggle_bar(cx));
        cx.on_action(|_: &FullScreen, cx| in_window(cx, |_, window, _| window.toggle_fullscreen()));
        cx.on_action(|_: &ResetTextSize, cx| crate::zoom::reset_all(cx));
        cx.on_action(|_: &PinToDash, cx| in_window(cx, |w, window, cx| w.toggle_pin(window, cx)));
        cx.on_action(|_: &ListApp, cx| in_window(cx, |w, window, cx| w.list_app(window, cx)));
    }
    // The window's own handlers are out of reach while a dialog holds focus,
    // nothing does, or the window is in the Dock: these still have to work.
    cx.on_action(|_: &crate::Quit, cx| {
        // Not over a dialog, the question to quit among them: the item is grey there.
        if dialog_up(cx) {
            return cx.propagate();
        }
        in_window(cx, |w, window, cx| {
            // It may ask first: not in a window that is in the Dock. Over a
            // dialog, which stays as it was when the quit is not confirmed.
            window.activate_window();
            w.request_quit(window, cx)
        })
    });
    cx.on_action(|_: &Restart, cx| in_window(cx, |w, window, cx| w.restart(window, cx)));
    cx.on_action(|_: &CheckForUpdates, cx| in_window(cx, |w, window, cx| w.check_update(true, window, cx)));
    cx.on_action(|_: &Minimize, cx| in_window(cx, |_, window, _| window.minimize_window()));
    cx.on_action(|_: &ZoomWindow, cx| in_window(cx, |_, window, _| window.zoom_window()));
    cx.on_action(|_: &About, cx| {
        if cfg!(target_os = "macos") {
            about_panel()
        } else {
            in_window(cx, |_, window, cx| {
                let told = format!("{}\n{COPYRIGHT}", crate::build_watch::version());
                window.push_notification(gpui_kit::component::notification::Notification::info(told).title(state::APP_ID), cx)
            })
        }
    });
    cx.on_action(|_: &Documentation, cx| cx.open_url(DOCS));
    // The window's ×: the agents keep running.
    cx.on_action(|_: &CloseWindow, cx| {
        // The window in front may be a panel of AppKit's own: About, Add Project's.
        if !close_panel() {
            in_window(cx, |w, window, cx| w.close_or_quit(window, cx))
        }
    });
    cx.on_action(|_: &BringAllToFront, cx| in_window(cx, |_, window, _| window.activate_window()));
    // What opens a dialog of its own, where no task is open and nothing holds
    // focus; not over a dialog that is already there.
    fn opens(cx: &mut App, f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static) {
        // Its keys go on to the dialog's field.
        if dialog_up(cx) {
            return cx.propagate();
        }
        in_window(cx, move |w, window, cx| {
            // Out of the Dock first: what opens would open unseen.
            window.activate_window();
            if !window.has_active_dialog(cx) {
                f(w, window, cx);
            }
        });
    }
    // What opens nothing: done whatever is open.
    fn runs(cx: &mut App, f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static) {
        in_window(cx, move |w, window, cx| {
            window.activate_window();
            f(w, window, cx);
        });
    }
    cx.on_action(|_: &crate::NewTask, cx| opens(cx, |w, window, cx| w.new_task(window, cx)));
    cx.on_action(|_: &crate::AskQuestion, cx| opens(cx, |w, window, cx| w.ask_question(window, cx)));
    cx.on_action(|_: &AddProject, cx| opens(cx, |w, window, cx| w.ask_for_project(window, cx)));
    cx.on_action(|_: &AddRemoteProject, cx| opens(cx, |w, window, cx| w.ask_for_remote_project(window, cx)));
    cx.on_action(|_: &crate::OpenSettings, cx| opens(cx, |w, window, cx| w.open_settings(window, cx)));
    cx.on_action(|_: &KeyboardShortcuts, cx| opens(cx, |w, window, cx| w.open_settings_at(crate::settings_ui::KEYS_PAGE, window, cx)));
    cx.on_action(|_: &crate::SwitchTheme, cx| opens(cx, |w, window, cx| w.theme_picker(window, cx)));
    cx.on_action(|_: &crate::palette::CommandPalette, cx| opens(cx, |w, window, cx| w.command_palette(window, cx)));
    cx.on_action(|_: &crate::palette::RecentPlaces, cx| opens(cx, |w, window, cx| w.recent_tasks(window, cx)));
    cx.on_action(|_: &crate::palette::NextWaiting, cx| opens(cx, |w, window, cx| w.next_waiting(window, cx)));
    cx.on_action(|_: &PresentationMode, cx| opens(cx, |w, window, cx| w.toggle_presentation(window, cx)));
    cx.on_action(|a: &OnTask, cx| {
        let what = a.0;
        let run = move |w: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| w.task_command(what, window, cx);
        if what.opens() { opens(cx, run) } else { runs(cx, run) }
    });
    cx.on_action(|a: &OnProject, cx| {
        let what = a.0;
        opens(cx, move |w, window, cx| w.project_command(what, window, cx))
    });
    cx.on_action(|a: &OnGroup, cx| {
        let what = a.0;
        opens(cx, move |w, window, cx| w.group_command(what, window, cx))
    });
    cx.on_action(|a: &Git, cx| {
        let what = a.0;
        runs(cx, move |w, window, cx| w.git_command(what, window, cx))
    });
    for id in REACHED {
        if let Some(shortcut) = keymap::find(id) {
            (shortcut.reach)(cx);
        }
    }
    cx.on_action(|a: &GoToSpace, cx| {
        let to = a.0.clone();
        opens(cx, move |w, window, cx| w.switch_space(to, window, cx))
    });
    cx.on_action(|_: &PowerSaving, cx| in_window(cx, |w, _, cx| w.toggle_power_saving(cx)));
    cx.on_action(|a: &Toggle, cx| {
        let what = a.0;
        in_window(cx, move |w, _, cx| toggle(what, w, cx))
    });
    cx.on_action(|_: &FindAndReplace, cx| in_window(cx, |_, window, cx| window.dispatch_action(Box::new(input::Replace), cx)));
    cx.on_action(|a: &Unkeyed, cx| {
        if let Some(shortcut) = keymap::find(a.0) {
            let action = shortcut.action();
            // Sent as its keys would be: where nothing on the way handles it, `reach` does.
            in_window(cx, move |_, window, cx| {
                window.activate_window();
                window.dispatch_action(action, cx);
            });
        }
    });
    // Context menus, and the menu bar where it is the app's own.
    gpui_kit::component::menu::on_action_chosen(crate::learn::chosen);
    #[cfg(target_os = "macos")]
    {
        let app = cx.to_async();
        gpui_macos::on_menu_item_chosen(move |action| {
            let action = action.boxed_clone();
            let _ = app.update(|cx| in_window(cx, move |_, window, cx| crate::learn::chosen(&*action, window, cx)));
        });
        let app = cx.to_async();
        gpui_macos::set_menu_hooks(
            move || app.update(refresh),
            |action| {
                LIVE.with_borrow(|live| {
                    live.iter().find(|(a, _)| a.partial_eq(action)).map(|(_, (enabled, checked, title))| gpui_macos::MenuItemState {
                        enabled: *enabled,
                        checked: *checked,
                        title: title.clone(),
                    })
                })
            },
        );
    }
    bind_system_keys(cx);
}

/// macOS's own keys for its own items, with the names Settings tells a
/// clash with: not in the keymap, they are not the app's to change.
pub const SYSTEM_KEYS: &[(&str, &str)] = &[("cmd-h", "Hide (macOS)"), ("alt-cmd-h", "Hide Others (macOS)"), ("cmd-m", "Minimize (macOS)")];

/// Bound again after Settings rebinds: the keys a shortcut lets go of are
/// bound to nothing, which would take these too.
pub fn bind_system_keys(cx: &mut App) {
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new(SYSTEM_KEYS[0].0, Hide, None),
            KeyBinding::new(SYSTEM_KEYS[1].0, HideOthers, None),
            KeyBinding::new(SYSTEM_KEYS[2].0, Minimize, None),
        ]);
    }
}

/// The standard About window, with the version the binary was built as:
/// a binary run outside a bundle has no Info.plist to tell it.
fn about_panel() {
    #[cfg(target_os = "macos")]
    {
        use objc2::{AnyThread as _, MainThreadMarker, rc::Retained, runtime::AnyObject};
        use objc2_app_kit::{
            NSAboutPanelOptionApplicationIcon, NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionVersion, NSApplication,
            NSImage,
        };
        use objc2_foundation::{NSData, NSDictionary, NSString};
        let Some(mtm) = MainThreadMarker::new() else { return };
        // A binary run outside a bundle has no icon of its own to show.
        let icon = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(include_bytes!("../assets/icon/ideae.icns")));
        let copyright = NSString::from_str("Copyright");
        let mut values: Vec<Retained<AnyObject>> = vec![
            NSString::from_str(state::APP_ID).into(),
            NSString::from_str(&crate::build_watch::version()).into(),
            NSString::from_str("").into(),
            NSString::from_str(COPYRIGHT).into(),
        ];
        // SAFETY: AppKit's own keys, and "Copyright", which it reads too, each
        // given what the panel expects: strings, and an image for the icon.
        unsafe {
            let mut keys = vec![NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionVersion, &*copyright];
            if let Some(icon) = icon {
                keys.push(NSAboutPanelOptionApplicationIcon);
                values.push(icon.into());
            }
            let values: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
            let options = NSDictionary::from_slices(&keys, &values);
            NSApplication::sharedApplication(mtm).orderFrontStandardAboutPanelWithOptions(&options);
        }
    }
}

const COPYRIGHT: &str = "Mozilla Public License 2.0 · github.com/scillarium/ideae";

/// AppKit adds Start Dictation and Emoji & Symbols to any menu named Edit;
/// in an editor of code and a terminal they are of no use. Read as the app
/// launches: told before it does.
pub fn before_launch() {
    #[cfg(target_os = "macos")]
    {
        use objc2::{rc::Retained, runtime::AnyObject};
        use objc2_foundation::{NSDictionary, NSNumber, NSString, NSUserDefaults};
        let keys = [NSString::from_str("NSDisabledDictationMenuItem"), NSString::from_str("NSDisabledCharacterPaletteMenuItem")];
        let yes: Retained<AnyObject> = NSNumber::new_bool(true).into();
        let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let defaults = NSDictionary::from_slices(&keys, &[&*yes, &*yes]);
        // The registration domain: nothing is written to the app's preferences.
        // SAFETY: a dictionary of strings to numbers, which defaults can hold.
        unsafe { NSUserDefaults::standardUserDefaults().registerDefaults(&defaults) };
    }
}

/// What AppKit added to Edit, taken out: Emoji & Symbols and Start
/// Dictation where the defaults are paid no heed, AutoFill (contacts and
/// passwords for a form) and Writing Tools, which no default turns off.
fn strip_edit() {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSMenuItem};
        use objc2_foundation::NSString;
        let Some(mtm) = MainThreadMarker::new() else { return };
        let Some(edit) = NSApplication::sharedApplication(mtm).mainMenu().and_then(|m| m.itemWithTitle(&NSString::from_str("Edit"))).and_then(|i| i.submenu()) else {
            return;
        };
        // By what they do: their titles are in the system's language.
        fn added(item: &NSMenuItem) -> bool {
            let action = item.action().map(|a| a.name().to_string_lossy().to_lowercase()).unwrap_or_default();
            ["orderfrontcharacterpalette:", "startdictation:", "showwritingtools:"].contains(&action.as_str())
                || action.contains("autofill")
                || action.starts_with("_handleinsertfrom")
                || item.submenu().is_some_and(|menu| (0..menu.numberOfItems()).filter_map(|i| menu.itemAtIndex(i)).any(|i| added(&i)))
        }
        for i in (0..edit.numberOfItems()).rev() {
            if edit.itemAtIndex(i).is_some_and(|item| added(&item)) {
                edit.removeItemAtIndex(i);
            }
        }
        // Their separator: Edit's own items end with Find.
        while edit.numberOfItems() > 0 && edit.itemAtIndex(edit.numberOfItems() - 1).is_some_and(|item| item.isSeparatorItem()) {
            edit.removeItemAtIndex(edit.numberOfItems() - 1);
        }
    }
}

/// Closes the key window when it is a panel: the main window stays main
/// under one, so gpui still calls the main window the active one.
fn close_panel() -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2::{ClassType as _, MainThreadMarker, runtime::NSObjectProtocol as _};
        use objc2_app_kit::{NSApplication, NSPanel};
        let Some(mtm) = MainThreadMarker::new() else { return false };
        if let Some(window) = NSApplication::sharedApplication(mtm).keyWindow().filter(|w| w.isKindOfClass(NSPanel::class())) {
            window.performClose(None);
            return true;
        }
    }
    false
}

/// Help gets the search field over all items: AppKit is told which menu it is.
fn help_menu() {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;
        use objc2_foundation::NSString;
        let Some(mtm) = MainThreadMarker::new() else { return };
        let app = NSApplication::sharedApplication(mtm);
        if let Some(item) = app.mainMenu().and_then(|menu| menu.itemWithTitle(&NSString::from_str("Help"))) {
            app.setHelpMenu(item.submenu().as_deref());
        }
    }
}

thread_local! {
    static AGAIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REACHING: std::cell::RefCell<std::collections::HashSet<std::any::TypeId>> = Default::default();
}

/// The shortcuts the menu bar offers for the task in view, which only the
/// workspace handles: out of its reach while nothing holds focus, as after
/// the focused tab closed, or the window is in the Dock. A test holds the
/// menus to this list.
const REACHED: &[&str] = &[
    "agent", "changes", "shell", "history", "changes_panel", "files_panel", "structure_panel", "run_panel", "database_panel", "focus_mode", "close_tab",
    "reopen_tab", "back", "forward", "last_edit", "branches", "go_to_file", "go_to_symbol", "find_in_files", "replace_in_files",
];

/// The item's action, sent again once the window is up and the task has
/// the keyboard. Once for each kind of action, however many shortcuts share it.
pub fn reach<A: gpui_kit::Action + Clone>(_: &A, cx: &mut App) {
    if !REACHING.with_borrow_mut(|seen| seen.insert(std::any::TypeId::of::<A>())) {
        return;
    }
    cx.on_action(|a: &A, cx| {
        // The second time round: what it was sent again for has nothing for it either.
        if AGAIN.replace(false) {
            return;
        }
        // Under a dialog the keys are its field's: Option+3 types # on a
        // British keyboard, Cmd+] indents in the prompt.
        if dialog_up(cx) {
            return cx.propagate();
        }
        let a = a.boxed_clone();
        in_window(cx, move |w, window, cx| {
            window.activate_window();
            w.focus_task_unless_dialog(window, cx);
            AGAIN.set(true);
            window.dispatch_action(a, cx);
            // Handled where it was sent: the next one is a new one.
            cx.defer(|_| AGAIN.set(false));
        })
    });
}

const DOCS: &str = "https://github.com/scillarium/ideae/tree/main/docs";

pub struct Target {
    pub window: gpui_kit::AnyWindowHandle,
    pub workspace: gpui_kit::WeakEntity<Workspace>,
}

impl gpui_kit::Global for Target {}

/// Runs after the action is through: the window cannot be entered while it is the one dispatching.
fn in_window(cx: &mut App, f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static) {
    cx.defer(move |cx| {
        let Some(target) = cx.try_global::<Target>() else { return };
        let (window, workspace) = (target.window, target.workspace.clone());
        let _ = window.update(cx, |_, window, cx| {
            let _ = workspace.update(cx, |w, cx| f(w, window, cx));
        });
    });
}

type ItemState = (bool, bool, gpui_kit::SharedString);

thread_local! {
    /// Each item's action and what it is to be: enabled, checked, its name.
    static LIVE: std::cell::RefCell<Vec<(Box<dyn gpui_kit::Action>, ItemState)>> = Default::default();
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    static REFRESHED: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

thread_local! {
    static DIALOGS: std::cell::RefCell<Option<Box<dyn Fn(&App) -> bool>>> = const { std::cell::RefCell::new(None) };
}

/// Whether a dialog is up, told while an action is dispatched in the window,
/// which cannot be entered then.
fn dialog_up(cx: &App) -> bool {
    DIALOGS.with_borrow(|dialogs| dialogs.as_ref().is_some_and(|up| up(cx)))
}

fn watch_dialogs(cx: &mut App) {
    if DIALOGS.with_borrow(Option::is_some) {
        return;
    }
    let Some(window) = cx.try_global::<Target>().map(|t| t.window) else { return };
    if let Ok(Some(up)) = window.update(cx, |_, window, cx| gpui_kit::component::dialogs_watch(window, cx)) {
        DIALOGS.set(Some(up));
    }
}

fn shown_now(cx: &mut App) -> Option<Shown> {
    let target = cx.try_global::<Target>()?;
    let (window, workspace) = (target.window, target.workspace.clone());
    window
        .update(cx, |_, window, cx| {
            let workspace = workspace.upgrade()?;
            let dialog = window.has_active_dialog(cx);
            Some(workspace.read(cx).shown(dialog, window, cx))
        })
        .ok()
        .flatten()
}

fn keep_live(menus: &[Menu]) {
    fn walk(items: &[MenuItem], live: &mut Vec<(Box<dyn gpui_kit::Action>, ItemState)>) {
        for item in items {
            match item {
                MenuItem::Action { name, action, checked, disabled, .. } => live.push((action.boxed_clone(), (!disabled, *checked, name.clone()))),
                MenuItem::Submenu(menu) => walk(&menu.items, live),
                _ => {}
            }
        }
    }
    let mut live = Vec::new();
    for menu in menus {
        walk(&menu.items, &mut live);
    }
    LIVE.set(live);
}

/// Puts the menus in the bar; again after shortcuts changed, so the keys
/// shown are the ones in force, and after the spaces did.
pub fn install(cx: &mut App) {
    watch_dialogs(cx);
    if cfg!(target_os = "macos") {
        let shown = shown_now(cx).unwrap_or_default();
        let menus = menus(shown);
        keep_live(&menus);
        cx.set_menus(menus);
        help_menu();
        strip_edit();
    }
}

/// The menu bar in the window's title bar, on Linux and Windows.
struct Bar(gpui_kit::Entity<AppMenuBar>);

impl gpui_kit::Global for Bar {}

/// Made once the window is, with its workspace, which the menus are read
/// from as each opens.
pub fn make_bar(cx: &mut App) {
    if cfg!(target_os = "macos") {
        return;
    }
    let Some(window) = cx.try_global::<Target>().map(|t| t.window) else { return };
    if let Ok(bar) = window.update(cx, |_, window, cx| AppMenuBar::with_source(std::rc::Rc::new(bar_menus), window, cx)) {
        cx.set_global(Bar(bar));
        cx.refresh_windows();
    }
}

fn bar_menus(window: &mut Window, cx: &mut App) -> Vec<gpui_kit::OwnedMenu> {
    let dialog = window.has_active_dialog(cx);
    let shown = cx.try_global::<Target>().and_then(|t| t.workspace.upgrade()).map(|w| w.read(cx).shown(dialog, window, cx));
    menus(shown.unwrap_or_default()).into_iter().map(Menu::owned).collect()
}

/// The bar for the title bar: collapsed, a ☰ until a menu opens.
pub fn bar(collapsed: bool, cx: &mut App) -> Option<gpui_kit::Entity<AppMenuBar>> {
    let bar = cx.try_global::<Bar>()?.0.clone();
    if bar.read(cx).collapsed() != collapsed {
        bar.update(cx, |bar, cx| bar.set_collapsed(collapsed, cx));
    }
    Some(bar)
}

/// After the key's dispatch: the window cannot be entered while it is the one dispatching.
/// Alt pressed and let go alone; not in a terminal, whose programs take Alt
/// themselves (readline, mc, vim).
fn alt_alone() -> KeyBinding {
    KeyBinding::new("alt", OpenMenuBar, Some("!Terminal"))
}

fn toggle_bar(cx: &mut App) {
    cx.defer(|cx| {
        let (Some(window), Some(bar)) = (cx.try_global::<Target>().map(|t| t.window), cx.try_global::<Bar>().map(|b| b.0.clone())) else { return };
        let _ = window.update(cx, |_, window, cx| bar.update(cx, |bar, cx| bar.toggle(window, cx)));
    });
}

thread_local! {
    static SOON: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Puts the menus in the bar once nothing has asked again for a moment:
/// a name typed makes them once, not once a letter.
pub fn install_soon(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let asked = SOON.get() + 1;
    SOON.set(asked);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
        if SOON.get() == asked {
            let _ = cx.update(install);
        }
    })
    .detach();
}

/// As a menu is about to show: what its items are to be now. AppKit asks for
/// each menu in the bar, and again for a key equivalent: once in a moment does.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn refresh(cx: &mut App) {
    strip_edit();
    let now = std::time::Instant::now();
    if REFRESHED.get().is_some_and(|at| now - at < std::time::Duration::from_millis(100)) {
        return;
    }
    // A change of spaces adds or takes items: `set_spaces` makes the menus again.
    let Some(shown) = shown_now(cx) else { return };
    REFRESHED.set(Some(now));
    keep_live(&menus(shown));
}

fn key(name: impl Into<gpui_kit::SharedString>, id: &str) -> MenuItem {
    let name = name.into();
    match keymap::find(id) {
        Some(shortcut) => MenuItem::Action { name, action: shortcut.action(), os_action: None, checked: false, disabled: false },
        // A test builds every menu, so this is seen before a release: the app still starts.
        None => {
            debug_assert!(false, "no shortcut {id}");
            MenuItem::Action { name, action: Box::new(gpui_kit::NoAction), os_action: None, checked: false, disabled: true }
        }
    }
}

/// A shortcut's item without its keys, where they do something else in the
/// editor: the keys are left as they are, the menu does not promise them.
#[derive(Clone, PartialEq, gpui_kit::Action)]
#[action(namespace = ideae, no_json)]
pub struct Unkeyed(pub &'static str);

/// The item grey: it keeps its action, so its keys are still shown.
fn off(item: MenuItem) -> MenuItem {
    match item {
        MenuItem::Action { name, action, os_action, checked, .. } => MenuItem::Action { name, action, os_action, checked, disabled: true },
        item => item,
    }
}

fn act(name: impl Into<gpui_kit::SharedString>, action: impl gpui_kit::Action) -> MenuItem {
    MenuItem::action(name, action)
}

/// macOS adds items of its own, which are therefore not here: Enter Full
/// Screen to View, the window's tiling to Window, dictation and emoji to
/// Edit, the search over all items to Help. Linux and Windows have no menu
/// named after the app: its items are where their users look, Settings and
/// Exit at the end of File, About in Help.
fn menus(shown: Shown) -> Vec<Menu> {
    let mac = cfg!(target_os = "macos");
    let sep = MenuItem::separator;
    let app = state::APP_ID;
    let Shown {
        dialog, switches, power_saving, presenting, focus_mode, projects, project, task: in_hand, replace, spaces, space, group, showing_group, group_settled, judging, edit, save, find, zoomed,
        fullscreen, maximized, pinned, listed,
    } = shown;
    // Under a dialog only Edit's, the window's and the app's own but Quit do something.
    let free = !dialog;
    let open = free && in_hand.is_some_and(|t| t.open);
    let starred = in_hand.is_some_and(|t| t.starred);
    let when = |on: bool, item: MenuItem| if on && free { item } else { off(item) };
    let task = |what: TaskCommand| when(in_hand.is_some_and(|t| t.allows(what)), act(what.name(starred), OnTask(what)));
    let project_item = |what: ProjectCommand| when(project, act(what.name(), OnProject(what)));
    let variants = |what: GroupCommand, on: bool| when(group && on, act(what.name(), OnGroup(what)));
    let git = |what: GitCommand| when(open && in_hand.is_some_and(|t| t.git(what)), act(git_name(what), Git(what)));
    // The handler on the focus path greys these by itself where they do nothing.
    let in_editor = |name: &'static str, id: &str| when(open && (mac || save), key(name, id));
    let in_task = |name: &'static str, id: &str| {
        debug_assert!(REACHED.contains(&id), "{id} is out of reach with nothing focused");
        when(open, key(name, id))
    };
    let front = in_hand.and_then(|t| t.front);
    let panel = |pane: RightPane| in_hand.is_some_and(|t| t.panels[pane as usize]);
    let has = |name: &'static str, id: &str, has: fn(&InHand) -> bool| {
        debug_assert!(REACHED.contains(&id), "{id} is out of reach with nothing focused");
        when(open && in_hand.is_some_and(|t| has(&t)), key(name, id))
    };
    // In the editor these keys indent and outdent.
    let unkeyed = |name: &'static str, id: &'static str, has: fn(&InHand) -> bool| when(open && in_hand.is_some_and(|t| has(&t)), act(name, Unkeyed(id)));
    let checked = |on: bool, item: MenuItem| match item {
        MenuItem::Action { name, action, os_action, disabled, .. } => MenuItem::Action { name, action, os_action, checked: on, disabled },
        item => item,
    };
    let mut space_items = vec![checked(space.is_none(), when(true, act("All Projects", GoToSpace(None)))), sep()];
    space_items.extend(spaces.iter().map(|name| checked(space.as_ref() == Some(name), when(true, act(name.clone(), GoToSpace(Some(name.clone())))))));
    if !spaces.is_empty() {
        space_items.push(sep());
    }
    space_items.push(when(true, act(ProjectCommand::EditSpaces.name(), OnProject(ProjectCommand::EditSpaces))));
    // AppKit greys what the focused view does not take; in the window, what Copy is not taken by.
    let editing = |item: MenuItem| if mac || edit { item } else { off(item) };
    let mut file = vec![
        when(projects, key("New Task…", "new_task")),
        when(projects, key("Ask a Question…", "ask_question")),
        sep(),
        when(!presenting, act("Add Project…", AddProject)),
        when(!presenting, act("Add Project on Another Machine…", AddRemoteProject)),
        project_item(ProjectCommand::Remove),
        project_item(ProjectCommand::ArchiveMerged),
        sep(),
        in_editor("Save", "save"),
        has("Close Tab", "close_tab", |t| t.closable),
        has("Reopen Closed Tab", "reopen_tab", |t| t.reopen),
        sep(),
        // Where JetBrains has it: of the whole app at work, not of what the window shows.
        checked(power_saving, when(true, act("Power Save Mode", PowerSaving))),
        sep(),
    ];
    let mut view = vec![
        checked(front == Some(FrontTab::Agent), has("Agent Tab", "agent", |t| t.agent)),
        checked(front == Some(FrontTab::Changes), in_task("Changes Tab", "changes")),
        checked(front == Some(FrontTab::Shell), in_task("Shell Tab", "shell")),
        sep(),
        checked(panel(RightPane::Changes), in_task("Changes Panel", "changes_panel")),
        checked(panel(RightPane::Files), in_task("Files Panel", "files_panel")),
        checked(panel(RightPane::Structure), in_task("Structure Panel", "structure_panel")),
        checked(panel(RightPane::Run), in_task("Run Panel", "run_panel")),
        checked(panel(RightPane::Database), in_task("Database Panel", "database_panel")),
        sep(),
        checked(focus_mode, in_task("Focus Mode", "focus_mode")),
        when(true, act(if presenting { "End Presentation" } else { "Presentation Mode…" }, PresentationMode)),
        sep(),
        MenuItem::submenu(Menu::new("Code").items(vec![
            checked(switches.type_hints, when(true, act("Type Hints", Toggle(Switch::TypeHints)))),
            checked(switches.edit_prediction, when(true, act("Edit Prediction", Toggle(Switch::EditPrediction)))),
            checked(switches.blame_at_line_end, when(true, act("Who Changed the Line, at Its End", Toggle(Switch::BlameAtLineEnd)))),
        ])),
        MenuItem::submenu(Menu::new("Diffs").items(vec![
            checked(switches.ignore_whitespace, when(true, act("Ignore Whitespace", Toggle(Switch::IgnoreWhitespace)))),
            checked(switches.diff_files, when(true, act("Changed Files Beside Them", Toggle(Switch::DiffFiles)))),
        ])),
    ];
    if !mac {
        view.push(when(zoomed, act("Text of Its Usual Size", ResetTextSize)));
    }
    view.extend([sep(), MenuItem::submenu(Menu::new("Spaces").items(space_items)), when(true, key("Theme…", "theme"))]);
    let mut window = vec![act("Minimize", Minimize)];
    let mut help = vec![
        when(true, key("Command Palette…", "command_palette")),
        when(true, key("Keyboard Shortcuts…", "keyboard_shortcuts")),
        sep(),
        act(format!("{app} Documentation"), Documentation),
    ];
    if mac {
        file.push(act("Close Window", CloseWindow));
        window.extend([act("Zoom", ZoomWindow), sep(), act("Bring All to Front", BringAllToFront)]);
    } else {
        file.extend([when(true, key("Settings…", "settings")), sep()]);
        // Without a daemon to keep the agents, closing the window is Exit.
        if cfg!(unix) {
            file.push(act("Close Window", CloseWindow));
        }
        file.extend([act("Restart", Restart), when(true, key("Exit", "quit"))]);
        view.extend([sep(), checked(fullscreen, act("Full Screen", FullScreen))]);
        window.push(checked(maximized, act("Maximize", ZoomWindow)));
        // Of the app's place on the desktop, with the window's: not a thing done to a file.
        // A binary run as it was downloaded is in no list: this puts it there. Listed, it can be pinned where there is a dash.
        let desk: Vec<MenuItem> = [
            (listed == Some(false)).then(|| act("Add to Applications", ListApp)),
            pinned.map(|pinned| act(if pinned { "Unpin from Dash" } else { "Pin to Dash" }, PinToDash)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !desk.is_empty() {
            window.push(sep());
            window.extend(desk);
        }
        help.extend([sep(), act("Check for Updates…", CheckForUpdates), act(format!("About {app}"), About)]);
    }
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu::new(app).items([
            act(format!("About {app}"), About),
            act("Check for Updates…", CheckForUpdates),
            sep(),
            when(true, key("Settings…", "settings")),
            sep(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            sep(),
            act(format!("Hide {app}"), Hide),
            act("Hide Others", HideOthers),
            act("Show All", ShowAll),
            sep(),
            act(format!("Restart {app}"), Restart),
            when(true, key(format!("Quit {app}"), "quit")),
        ]));
    }
    menus.extend([
        Menu::new("File").items(file),
        Menu::new("Edit").items([
            editing(MenuItem::os_action("Undo", input::Undo, OsAction::Undo)),
            editing(MenuItem::os_action("Redo", input::Redo, OsAction::Redo)),
            sep(),
            editing(MenuItem::os_action("Cut", input::Cut, OsAction::Cut)),
            editing(MenuItem::os_action("Copy", input::Copy, OsAction::Copy)),
            editing(MenuItem::os_action("Paste", input::Paste, OsAction::Paste)),
            editing(MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll)),
            sep(),
            MenuItem::submenu(Menu::new("Find").items([
                when(mac || find, act("Find…", input::Search)),
                // Its keys, ⇧⌘F in the editor, are Find in Files' everywhere else.
                when(replace, act("Find and Replace…", FindAndReplace)),
                sep(),
                in_task("Find in Files…", "find_in_files"),
                in_task("Replace in Files…", "replace_in_files"),
            ])),
        ]),
        Menu::new("View").items(view),
        Menu::new("Navigate").items([
            in_task("Go to File…", "go_to_file"),
            in_task("Go to Class or Symbol…", "go_to_symbol"),
            sep(),
            unkeyed("Back", "back", |t| t.back),
            unkeyed("Forward", "forward", |t| t.forward),
            in_task("Last Edit Location", "last_edit"),
            sep(),
            when(true, key("Next Task Waiting for You", "next_waiting")),
            when(true, key("Recent Tasks and Files…", "recent")),
        ]),
        Menu::new("Task").items([
            task(TaskCommand::NewTerminal),
            task(TaskCommand::RestartAgent),
            sep(),
            task(TaskCommand::StartServices),
            task(TaskCommand::StopServices),
            sep(),
            task(TaskCommand::Star),
            task(TaskCommand::Rename),
            sep(),
            MenuItem::submenu(Menu::new("Variants").items([
                task(TaskCommand::AnotherVariant),
                variants(GroupCommand::Show, !showing_group),
                sep(),
                variants(GroupCommand::Judge, group_settled && !judging),
                task(TaskCommand::KeepVariant),
                sep(),
                // Also the judge and the group's check, which run with every variant paused.
                variants(GroupCommand::Stop, true),
                variants(GroupCommand::Discard, true),
            ])),
            sep(),
            task(TaskCommand::Pause),
            task(TaskCommand::Archive),
            task(TaskCommand::Defer),
            task(TaskCommand::RemoveSide),
        ]),
        Menu::new("Git").items([
            git(GitCommand::Commit),
            sep(),
            git(GitCommand::Stash),
            git(GitCommand::Unstash),
            sep(),
            git(GitCommand::Fetch),
            git(GitCommand::Pull),
            git(GitCommand::Rebase),
            git(GitCommand::Push),
            git(GitCommand::PullRequest),
            sep(),
            checked(front == Some(FrontTab::History), in_task("History", "history")),
            in_task("Branches…", "branches"),
            task(TaskCommand::CompareWithBranch),
        ]),
        Menu::new("Window").items(window),
        Menu::new("Help").items(help),
    ]);
    menus
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Menu, MenuItem};

    use super::Shown;
    use crate::commands::InHand;
    use crate::task::{FrontTab, GitCommand};

    fn names(menu: &Menu, out: &mut Vec<String>) {
        for item in &menu.items {
            match item {
                MenuItem::Action { name, action, .. } => {
                    assert!(action.as_any().downcast_ref::<gpui_kit::NoAction>().is_none(), "{name} does nothing");
                    out.push(name.to_string());
                }
                MenuItem::Submenu(sub) => names(sub, out),
                _ => {}
            }
        }
    }

    fn of(menus: &[Menu], menu: &str) -> Vec<(String, bool)> {
        let menu = menus.iter().find(|m| m.name.as_ref() == menu).unwrap();
        menu.items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, disabled, .. } => Some((name.to_string(), !disabled)),
                _ => None,
            })
            .collect()
    }

    fn live(menus: &[Menu], menu: &str, item: &str) -> bool {
        of(menus, menu).into_iter().find(|(name, _)| name == item).unwrap_or_else(|| panic!("no {item} in {menu}")).1
    }

    /// A binary run as it was downloaded is in no list of applications:
    /// Window has the way to add it, and where there is a dash, to pin it.
    /// Power Save Mode is the app's, in File.
    #[test]
    fn the_app_is_added_to_the_applications_and_pinned_from_window() {
        let window = |listed: Option<bool>, pinned: Option<bool>| -> Vec<String> {
            let menus = super::menus(Shown { listed, pinned, ..Default::default() });
            of(&menus, "Window").into_iter().map(|(name, _)| name).filter(|name| name.contains("Applications") || name.contains("Dash")).collect()
        };
        if cfg!(target_os = "macos") {
            assert!(window(Some(false), Some(false)).is_empty(), "the .app in Applications is all it takes");
        } else {
            assert_eq!(window(Some(false), None), ["Add to Applications"], "no dash: listed, and no more to offer");
            assert_eq!(window(Some(false), Some(false)), ["Add to Applications", "Pin to Dash"]);
            assert_eq!(window(Some(true), Some(true)), ["Unpin from Dash"]);
            assert!(window(Some(true), None).is_empty() && window(None, None).is_empty());
        }
        let menus = super::menus(Shown::default());
        assert!(of(&menus, "File").iter().any(|(name, _)| name == "Power Save Mode") && !of(&menus, "View").iter().any(|(name, _)| name == "Power Save Mode"));
    }

    /// Every item has an action, and no menu says the same thing twice.
    #[test]
    fn the_menus_are_made() {
        let menus = super::menus(Shown { projects: true, project: true, task: Some(InHand { main: false, remote: false, side: false, ready: true, grouped: true, starred: false, open: true, agent: true, agent_ended: true, closable: true, file: true, back: true, forward: true, reopen: true, services: true, services_running: true, services_waiting: true, git: [true; 9], front: Some(FrontTab::Agent), panels: [false; 5] }), spaces: vec!["Work".into()], ..Default::default() });
        let titles: Vec<_> = menus.iter().map(|m| m.name.as_ref()).collect();
        // Linux and Windows have no menu named after the app: its items are in File and Help.
        assert_eq!(titles[usize::from(cfg!(target_os = "macos"))..], ["File", "Edit", "View", "Navigate", "Task", "Git", "Window", "Help"]);
        for id in super::REACHED {
            assert!(crate::keymap::find(id).is_some(), "no shortcut {id}");
        }
        // An item is told its state by its action: no two may share one.
        let mut actions: Vec<Box<dyn gpui_kit::Action>> = Vec::new();
        fn all(items: &[MenuItem], out: &mut Vec<Box<dyn gpui_kit::Action>>) {
            for item in items {
                match item {
                    MenuItem::Action { name, action, .. } => {
                        assert!(!out.iter().any(|a| a.partial_eq(action.as_ref())), "{name} has another item's action");
                        out.push(action.boxed_clone());
                    }
                    MenuItem::Submenu(sub) => all(&sub.items, out),
                    _ => {}
                }
            }
        }
        for menu in &menus {
            all(&menu.items, &mut actions);
        }
        for menu in &menus {
            let mut seen = Vec::new();
            names(menu, &mut seen);
            let mut once = seen.clone();
            once.sort();
            once.dedup();
            assert_eq!(once.len(), seen.len(), "{} says something twice", menu.name);
        }
    }

    /// A grey shortcut item keeps its action, and so its keys.
    #[test]
    fn a_grey_item_keeps_its_keys() {
        let MenuItem::Action { action, disabled, .. } = super::off(super::key("Save", "save")) else { panic!() };
        assert!(disabled && action.partial_eq(crate::keymap::find("save").unwrap().action().as_ref()));
    }

    /// The task's items are grey where the task does not allow them, and say what they would do.
    #[test]
    fn the_task_s_items_follow_the_task() {
        let none = super::menus(Shown::default());
        assert!(!live(&none, "Task", "Rename Task…") && !live(&none, "Git", "Commit…") && !live(&none, "Task", "Star Task"));
        assert!(!live(&none, "File", "Archive Merged Tasks…") && !live(&none, "File", "New Task…") && !live(&none, "View", "Agent Tab"));

        let task = InHand { main: false, remote: false, side: false, ready: true, grouped: false, starred: true, open: true, agent: true, agent_ended: false, closable: true, file: false, back: false, forward: false, reopen: false, services: true, services_running: false, services_waiting: true, git: [true; 9], front: Some(FrontTab::Agent), panels: [false; 5] };
        let one = super::menus(Shown { task: Some(task), projects: true, project: true, ..Default::default() });
        assert!(live(&one, "File", "New Task…") && live(&one, "View", "Agent Tab") && live(&one, "Task", "Unstar Task") && live(&one, "Task", "Archive Task") && live(&one, "Git", "Pull"));
        assert!(!live(&one, "Task", "Stop All in Run Panel"));

        // What git has nothing to do for, and what a task at work does not allow.
        let mut git = [false; 9];
        git[GitCommand::ALL.iter().position(|w| *w == GitCommand::Fetch).unwrap()] = true;
        let idle = super::menus(Shown { task: Some(InHand { git, ready: false, ..task }), ..Default::default() });
        assert!(live(&idle, "Git", "Fetch") && !live(&idle, "Git", "Commit…") && !live(&idle, "Task", "Archive Task"));

        // Under a dialog only Edit's, the window's and the app's own but Quit do something.
        let dialog = super::menus(Shown { dialog: true, edit: true, task: Some(task), projects: true, ..Default::default() });
        assert!(!live(&dialog, "File", "New Task…") && !live(&dialog, "Git", "Pull") && !live(&dialog, "Task", "Unstar Task") && !live(&dialog, "View", "Agent Tab"));
        assert!(live(&dialog, "Edit", "Copy") && live(&dialog, "Window", "Minimize"));
        // The question to quit is a dialog too: a second Cmd+Q asks nothing again.
        if cfg!(target_os = "macos") {
            assert!(!live(&dialog, crate::state::APP_ID, &format!("Quit {}", crate::state::APP_ID)));
        } else {
            assert!(!live(&dialog, "File", "Exit") && live(&dialog, "File", "Restart"));
        }

        let main = super::menus(Shown { task: Some(InHand { main: true, starred: false, ..task }), ..Default::default() });
        assert!(live(&main, "Task", "Star Task") && live(&main, "Task", "Pause Task"));
        assert!(!live(&main, "Task", "Rename Task…") && !live(&main, "Task", "Archive Task"));

        // A worktree an agent made for itself is removed, not archived, and has no variants.
        let side = super::menus(Shown { task: Some(InHand { side: true, ..task }), ..Default::default() });
        assert!(live(&side, "Task", "Remove Agent's Worktree") && !live(&side, "Task", "Archive Task") && !live(&one, "Task", "Remove Agent's Worktree"));

        let closed = super::menus(Shown { task: Some(InHand { open: false, ..task }), ..Default::default() });
        assert!(!live(&closed, "Task", "Restart Agent") && !live(&closed, "Git", "Push") && live(&closed, "Task", "Archive Task") && !live(&closed, "Task", "Pause Task"));

        let presenting = super::menus(Shown { presenting: true, ..Default::default() });
        assert!(live(&presenting, "View", "End Presentation") && !live(&presenting, "File", "Add Project…"));
    }
}

/// The bar in the title bar, as Linux and Windows have it.
#[cfg(all(test, not(target_os = "macos")))]
mod bar {
    use std::{cell::RefCell, rc::Rc};

    use gpui_kit::component::menu::AppMenuBar;
    use gpui_kit::test::{TestEventExt as _, TestQueryExt as _, TestWindowExt as _};
    use gpui_kit::{
        AppContext as _, Bounds, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyBinding, Menu, Modifiers, MouseButton, ParentElement as _,
        Point, Render, Styled as _, TestAppContext, Window, WindowBounds, WindowOptions, div, px, size,
    };

    use super::{OnTask, OpenMenuBar, Shown};
    use crate::commands::{InHand, TaskCommand};

    struct Root {
        bar: Entity<AppMenuBar>,
        focus: FocusHandle,
    }

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().track_focus(&self.focus).child(div().h(px(32.)).child(self.bar.clone()))
        }
    }

    type Sent = Rc<RefCell<Vec<TaskCommand>>>;

    /// A window with the bar over the given state; the task items it sends, kept.
    fn open(cx: &mut TestAppContext, shown: Shown, collapsed: bool) -> (gpui_kit::AnyWindowHandle, Entity<AppMenuBar>, Sent) {
        cx.update(gpui_kit::init);
        let sent: Sent = Rc::default();
        cx.update(|cx| {
            let kept = sent.clone();
            cx.on_action(move |a: &OnTask, _| kept.borrow_mut().push(a.0));
            let bounds = Bounds { origin: Point::default(), size: size(px(1000.), px(700.)) };
            let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
            let mut bar = None;
            let window = gpui_kit::open_window(options, cx, |window, cx| {
                let made = AppMenuBar::with_source(Rc::new(move |_, _| super::menus(shown.clone()).into_iter().map(Menu::owned).collect()), window, cx);
                made.update(cx, |b, cx| b.set_collapsed(collapsed, cx));
                bar = Some(made.clone());
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    focus.focus(window, cx);
                    Root { bar: made, focus }
                })
            })
            .expect("a test window")
            .0;
            (window, bar.unwrap(), sent)
        })
    }

    fn task() -> InHand {
        InHand { main: true, remote: false, side: false, ready: true, grouped: false, starred: false, open: true, agent: true, agent_ended: false, closable: true, file: false, back: false, forward: false, reopen: false, services: false, services_running: false, services_waiting: false, git: [true; 9], front: None, panels: [false; 5] }
    }

    fn click_label(window: &mut Window, label: &str, cx: &mut gpui_kit::App) {
        let at = window.find_by_label(label).bounds().center();
        window.pointer_move(at, None, cx);
        window.pointer_down(at, MouseButton::Left, cx);
        window.pointer_up(at, MouseButton::Left, cx);
    }

    /// ☰ opens the bar on its first menu; another name hovered opens that one;
    /// an item sends its action and the bar is ☰ again; a grey one does nothing.
    #[gpui_kit::test]
    fn the_bar_opens_and_sends(cx: &mut TestAppContext) {
        let (handle, bar, sent) = open(cx, Shown { task: Some(task()), projects: true, ..Default::default() }, true);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("app-menu-bar").is_none(), "collapsed, only ☰");
            window.click("app-menu", cx);
            window.render_frame(cx);
            assert!(window.try_find("menu-Help").is_some(), "the names, in place of ☰");
            assert!(window.try_find_by_label("New Task…").is_some(), "File is open");

            window.hover("menu-Task", cx);
            window.render_frame(cx);
            assert!(window.try_find_by_label("New Task…").is_none() && window.try_find_by_label("Star Task").is_some(), "Task, by the pointer");

            // The main thread has no name of its own to change.
            click_label(window, "Rename Task…", cx);
            window.render_frame(cx);
            assert!(sent.borrow().is_empty() && bar.read(cx).is_open(), "a grey item does nothing");

            click_label(window, "Star Task", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(*sent.borrow() == [TaskCommand::Star]);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(!bar.read(cx).is_open() && window.try_find("app-menu").is_some(), "back to ☰");
        })
        .unwrap();
    }

    /// Esc closes the menu; with the bar always shown, the names stay.
    #[gpui_kit::test]
    fn escape_closes_and_the_names_stay_when_shown(cx: &mut TestAppContext) {
        let (handle, bar, _) = open(cx, Shown::default(), false);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("app-menu").is_none() && window.try_find("menu-File").is_some(), "shown without a click");
            window.click("menu-Git", cx);
            window.render_frame(cx);
            // Nothing in hand: git has nothing to act on.
            click_label(window, "Push", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert!(bar.read(cx).is_open(), "a grey item leaves the menu open");
            window.press("right", cx);
            window.render_frame(cx);
            assert!(window.try_find_by_label("Minimize").is_some(), "Right moves to Window");
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(!bar.read(cx).is_open() && window.try_find("menu-File").is_some(), "closed, the names still there");
        })
        .unwrap();
    }

    /// Alt pressed and let go alone opens the bar; Alt with a key does not.
    #[gpui_kit::test]
    fn alt_alone_opens_it(cx: &mut TestAppContext) {
        let (handle, bar, _) = open(cx, Shown::default(), true);
        cx.update(|cx| {
            cx.bind_keys([super::alt_alone()]);
            cx.on_action(|_: &OpenMenuBar, cx| super::toggle_bar(cx));
            cx.set_global(super::Bar(bar.clone()));
            cx.set_global(super::Target { window: handle, workspace: gpui_kit::WeakEntity::new_invalid() });
        });
        let alt = Modifiers { alt: true, ..Default::default() };
        cx.update_window(handle, |_, window, cx| {
            window.change_modifiers(alt, cx);
            window.key_down("alt-j", false, cx);
            window.key_up("alt-j", cx);
            window.change_modifiers(Modifiers::default(), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(!cx.read(|cx| bar.read(cx).is_open()), "Alt+J is J's");
        cx.update_window(handle, |_, window, cx| {
            window.change_modifiers(alt, cx);
            window.change_modifiers(Modifiers::default(), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(cx.read(|cx| bar.read(cx).is_open()), "Alt alone opens it");
    }
}

#[cfg(test)]
mod names {
    #[test]
    fn the_palette_says_it_in_sentence_case() {
        assert_eq!(crate::commands::sentence("Keep Only This Variant…"), "Keep only this variant…");
        assert_eq!(crate::commands::sentence("Push"), "Push");
        assert_eq!(crate::commands::sentence("Open the PR Page"), "Open the PR page");
    }
}
