//! Production connections in the UI: the question asked before anything
//! runs on one, and locking again on time (`db::guard`).

use super::*;
use crate::db::{config, guard};

pub type Then = Rc<dyn Fn(&mut Window, &mut App)>;

/// Runs `then` once production is unlocked: at once, or after being asked.
/// Enter does not answer: unlocking takes the pointer on the button.
pub fn production_gate(then: Then, window: &mut Window, cx: &mut App) {
    // Never at once, from inside whatever asked: it may be in the middle of an update.
    if guard::is_open() {
        let handle = window.window_handle();
        return cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| then(window, cx));
        });
    }
    let count = config::list().iter().filter(|c| c.production).count();
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let then = then.clone();
        dialog
            .title("Unlock production?")
            .w(px(440.))
            .content(move |content, _, _| {
                content.child(div().text_sm().text_color(muted).child(format!(
                    "Statements will run on production for {} minutes, then it locks again. {count} production connection{}.",
                    config::production_minutes(),
                    if count == 1 { "" } else { "s" }
                )))
            })
            .footer(h_flex().justify_end().gap_2().child(Button::new("prod-unlock-cancel").primary().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                Button::new("prod-unlock").danger().label("Unlock production").on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    guard::unlock();
                    relock_later(cx);
                    super::reload_locked(cx);
                    cx.refresh_windows();
                    // After the question is gone: what runs may ask its own.
                    let (then, handle) = (then.clone(), window.window_handle());
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, cx| then(window, cx));
                    });
                }),
            ))
    });
}

/// When the unlocked time is up, every window shows production locked. The
/// timer stands still while the machine sleeps: it looks again often, and
/// `guard` asks the wall clock.
fn relock_later(cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Some(left) = guard::left() {
            cx.background_executor().timer(left.min(std::time::Duration::from_secs(20)) + std::time::Duration::from_millis(200)).await;
        }
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

pub fn lock_now(cx: &mut App) {
    guard::lock();
    cx.refresh_windows();
}
