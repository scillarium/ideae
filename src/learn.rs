//! Learn the keys (Settings → Keys), as JetBrains' Key Promoter: a command
//! that has keys, chosen with the pointer (a menu, the palette, a button),
//! says at the bottom which keys would have done it.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui_kit::{Action, App, Window, component::{WindowExt as _, notification::Notification}};

use crate::{keymap, state};

struct LearnKeys;

thread_local! {
    /// How often each shortcut was gone round, this run.
    static TIMES: RefCell<HashMap<&'static str, u32>> = RefCell::default();
}

/// Told by the menus of an action they sent: it is a shortcut's, or the
/// menu's `Unkeyed` that stands for one.
pub fn chosen(action: &dyn Action, window: &mut Window, cx: &mut App) {
    if !state::with_settings(|s| s.learn_keys) {
        return;
    }
    if let Some(id) = shortcut_of(action) {
        used(id, window, cx);
    }
}

fn shortcut_of(action: &dyn Action) -> Option<&'static str> {
    match action.as_any().downcast_ref::<crate::menu::Unkeyed>() {
        Some(unkeyed) => Some(unkeyed.0),
        None => keymap::SHORTCUTS.iter().find(|s| s.action().partial_eq(action)).map(|s| s.id),
    }
}

/// The shortcut `id` was done without its keys.
pub fn used(id: &'static str, window: &mut Window, cx: &mut App) {
    if !state::with_settings(|s| s.learn_keys) {
        return;
    }
    let Some(shortcut) = keymap::find(id) else { return };
    let keys = keymap::label(id);
    if keys.is_empty() {
        return;
    }
    let times = TIMES.with_borrow_mut(|times| {
        let n = times.entry(id).or_default();
        *n += 1;
        *n
    });
    let mut note = Notification::info(format!("{keys}  {}", shortcut.name)).title("Learn the keys").id::<LearnKeys>().placement(gpui_kit::Anchor::BottomRight);
    if times > 1 {
        note = note.message(format!("{keys}  {}\nWithout its keys {times} times", shortcut.name));
    }
    window.push_notification(note, cx);
}

#[cfg(test)]
mod tests {
    use super::shortcut_of;
    use crate::{menu, task};

    #[test]
    fn a_menu_s_action_is_told_by_its_shortcut() {
        assert_eq!(shortcut_of(&menu::Git(task::GitCommand::Commit)), Some("commit"));
        assert_eq!(shortcut_of(&menu::Git(task::GitCommand::Push)), Some("push"));
        assert_eq!(shortcut_of(&menu::Git(task::GitCommand::Stash)), None);
        assert_eq!(shortcut_of(&menu::Unkeyed("back")), Some("back"));
        assert_eq!(shortcut_of(&task::ShowFileInFiles), Some("show_in_files"));
    }
}
