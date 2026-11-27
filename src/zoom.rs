//! Text size per kind of view, shared by every view of that kind in every
//! task, open or not, and kept in the state file. Ctrl+wheel over a view
//! changes the size of its kind.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// File tabs.
    Editor,
    /// The Changes tab's diff.
    Changes,
    /// Agent and shell tabs.
    Terminal,
    /// The sidebar: its sizes grow with its text, as a browser zooms a page.
    Sidebar,
}

pub const KINDS: [Kind; 4] = [Kind::Editor, Kind::Changes, Kind::Terminal, Kind::Sidebar];

/// Sizes in points; `None` is the theme's code size, or for the sidebar
/// the size of a task's name it was drawn for.
#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Debug)]
#[serde(default)]
pub struct Zoom {
    pub editor: Option<f32>,
    pub changes: Option<f32>,
    pub terminal: Option<f32>,
    pub sidebar: Option<f32>,
}

impl Global for Zoom {}

pub const MIN: f32 = 6.;
pub const MAX: f32 = 48.;
/// Touchpad scroll, in pixels, that makes one step of one point.
const PIXELS_PER_STEP: f32 = 40.;

impl Zoom {
    pub fn slot(&mut self, kind: Kind) -> &mut Option<f32> {
        match kind {
            Kind::Editor => &mut self.editor,
            Kind::Changes => &mut self.changes,
            Kind::Terminal => &mut self.terminal,
            Kind::Sidebar => &mut self.sidebar,
        }
    }

    pub fn get(&self, kind: Kind) -> Option<f32> {
        match kind {
            Kind::Editor => self.editor,
            Kind::Changes => self.changes,
            Kind::Terminal => self.terminal,
            Kind::Sidebar => self.sidebar,
        }
    }
}

pub fn init(saved: Zoom, cx: &mut App) {
    cx.set_global(saved);
}

/// A factor on every kind's size while it is set: text made larger for
/// an audience, the sizes chosen left as they are for afterwards.
#[derive(Clone, Copy)]
struct Scale(f32);
impl Global for Scale {}

pub fn scale(cx: &App) -> f32 {
    cx.try_global::<Scale>().map_or(1., |scale| scale.0)
}

pub fn set_scale(factor: f32, cx: &mut App) {
    if scale(cx) == factor {
        return;
    }
    cx.set_global(Scale(factor));
    // Those that follow the sizes follow this too.
    let zoom = current(cx);
    cx.set_global(zoom);
    cx.refresh_windows();
}

/// The size of a task's name in the sidebar, the one its lengths are given for.
pub const SIDEBAR: f32 = 13.;

/// The size of `kind` none was chosen for: the theme's code size, or the
/// sidebar's own.
pub fn usual(kind: Kind, code_size: f32) -> f32 {
    if kind == Kind::Sidebar { SIDEBAR } else { code_size }
}

/// The size chosen for `kind`, the factor not applied.
fn chosen(kind: Kind, cx: &App) -> f32 {
    current(cx).get(kind).unwrap_or_else(|| usual(kind, f32::from(cx.theme().mono_font_size)))
}

/// How much larger than its usual size the sidebar is drawn. Not by a
/// presentation's factor, which is for what is shown: code and terminals.
pub fn sidebar_factor(cx: &App) -> f32 {
    chosen(Kind::Sidebar, cx) / SIDEBAR
}

pub fn current(cx: &App) -> Zoom {
    cx.try_global::<Zoom>().copied().unwrap_or_default()
}

/// The size a view of `kind` draws code with.
pub fn size(kind: Kind, cx: &App) -> Pixels {
    px((chosen(kind, cx) * scale(cx)).round())
}

/// Sets the size for every view of `kind`; `None` goes back to its usual one.
pub fn set(kind: Kind, size: Option<f32>, cx: &mut App) {
    let size = size.map(|s| s.round().clamp(MIN, MAX));
    if current(cx).get(kind) == size {
        return;
    }
    let mut zoom = current(cx);
    *zoom.slot(kind) = size;
    cx.set_global(zoom);
    cx.refresh_windows();
}

/// Whether any kind has a size of its own, for "back to the usual size".
pub fn any_set(cx: &App) -> bool {
    let zoom = current(cx);
    KINDS.into_iter().any(|kind| zoom.get(kind).is_some())
}

/// Every kind back to its usual size.
pub fn reset_all(cx: &mut App) {
    for kind in KINDS {
        set(kind, None, cx);
    }
}

/// Steps the size of `kind` by `points`.
pub fn step(kind: Kind, points: f32, cx: &mut App) {
    set(kind, Some(chosen(kind, cx) + points), cx);
}

/// Scroll left over from touchpad deltas too small to make a step.
#[derive(Default)]
struct Pending(f32);
impl Global for Pending {}

/// Points to step for one wheel event: a wheel notch is a point, a touchpad
/// accumulates pixels.
pub fn steps(delta: &ScrollDelta, cx: &mut App) -> f32 {
    match delta {
        ScrollDelta::Lines(lines) if lines.y > 0. => 1.,
        ScrollDelta::Lines(lines) if lines.y < 0. => -1.,
        ScrollDelta::Lines(_) => 0.,
        ScrollDelta::Pixels(pixels) => {
            let pending = cx.default_global::<Pending>();
            pending.0 += f32::from(pixels.y);
            let steps = (pending.0 / PIXELS_PER_STEP).trunc();
            pending.0 -= steps * PIXELS_PER_STEP;
            steps
        }
    }
}

/// An overlay for a `relative()` parent: takes Ctrl+wheel over the parent's
/// bounds before the views inside scroll with it, and zooms `kind`. It has no
/// hitbox, so the mouse still reaches what is under it.
///
/// It goes first among the parent's children: capture listeners run in paint
/// order, and the terminal takes the wheel in the capture phase as well.
pub fn wheel(kind: Kind) -> impl IntoElement {
    canvas(
        // Hovered, not just under the pointer: a view drawn over this one (the
        // sidebar out over a file) takes the wheel for its own size.
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || !event.modifiers.control || !hitbox.is_hovered(window) {
                    return;
                }
                cx.stop_propagation();
                // Up, away from you, makes the text bigger, as in browsers.
                let points = steps(&event.delta, cx);
                if points != 0. {
                    step(kind, points, cx);
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// `child` with every size in rems `factor` times its own: its text,
/// paddings and icons. A length that is to grow with it is given in rems.
pub fn rem_scaled(factor: f32, child: impl IntoElement) -> RemScaled {
    RemScaled { factor, child: child.into_any_element() }
}

pub struct RemScaled {
    factor: f32,
    child: AnyElement,
}

impl RemScaled {
    fn rem(&self, window: &Window) -> Option<Pixels> {
        (self.factor != 1.).then(|| window.rem_size() * self.factor)
    }
}

impl IntoElement for RemScaled {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for RemScaled {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let rem = self.rem(window);
        (window.with_rem_size(rem, |window| self.child.request_layout(window, cx)), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        let rem = self.rem(window);
        window.with_rem_size(rem, |window| {
            self.child.prepaint(window, cx);
        });
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        let rem = self.rem(window);
        window.with_rem_size(rem, |window| self.child.paint(window, cx));
    }
}
