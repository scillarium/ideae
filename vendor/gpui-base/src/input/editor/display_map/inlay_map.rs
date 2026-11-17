//! ideae patch: text that is shown in the editor but is not in the buffer
//! (a type after a name, a parameter's name before an argument, who last
//! changed the line at its end).
//!
//! An inlay takes no bytes: every position the editor keeps, moves, selects
//! or copies stays a buffer offset, and an inlay only adds width at one of
//! them. That keeps the cursor out of it by construction (left and right step
//! over it as they step over any one character boundary) and copying never
//! sees it. What has to know about inlays is everything that turns an offset
//! into a width or an x and back: the soft wrap (an inlay takes room on its
//! row), the shaped rows (`InputLine`), and the hit test, which reports a
//! click on an inlay rather than putting the cursor inside it.
use std::ops::Range;

use gpui::{Font, FontStyle, Hsla, Pixels, SharedString, px};

/// Which text an inlay belongs to when it sits between two characters. That
/// decides where the cursor at its offset is drawn, where text typed there
/// goes, and on which row the inlay stays when a soft wrap falls at its
/// offset. Several inlays at one offset are drawn `Left` ones first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InlayBias {
    /// Hangs on the text before it: a type after a name, a note at the end
    /// of a line. The cursor at its offset is drawn before it, text typed
    /// there goes before it (it moves on with the end of that text), and a
    /// wrap at its offset leaves it at the end of the upper row.
    #[default]
    Left,
    /// Hangs on the text after it: a parameter's name before an argument.
    /// The cursor at its offset is drawn after it, text typed there goes
    /// after it, and a wrap at its offset takes it to the lower row.
    Right,
}

/// How an inlay looks. Margins are outside its background, in widths of
/// the editor font's space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InlayStyle {
    /// None draws it in the editor's muted foreground.
    pub color: Option<Hsla>,
    pub background: Option<Hsla>,
    pub italic: bool,
    pub margin_left: f32,
    pub margin_right: f32,
}

/// Text shown at a buffer offset. With an `id`, a click on it is reported
/// as [`crate::input::InputEvent::InlayClick`] instead of moving the cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct Inlay {
    pub offset: usize,
    pub bias: InlayBias,
    pub text: SharedString,
    pub style: InlayStyle,
    pub id: Option<SharedString>,
}

impl Inlay {
    pub fn new(offset: usize, text: impl Into<SharedString>) -> Self {
        Self {
            offset,
            bias: InlayBias::Left,
            text: text.into(),
            style: InlayStyle::default(),
            id: None,
        }
    }

    pub fn bias(mut self, bias: InlayBias) -> Self {
        self.bias = bias;
        self
    }

    pub fn style(mut self, style: InlayStyle) -> Self {
        self.style = style;
        self
    }

    pub fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = Some(id.into());
        self
    }
}

impl InlayStyle {
    /// The font its text is drawn in, from the editor's.
    pub(crate) fn font(&self, base: &Font) -> Font {
        let mut font = base.clone();
        if self.italic {
            font.style = FontStyle::Italic;
        }
        font
    }

    /// Room before its text and after it, `space` being the width of the
    /// editor font's space: the margins, and inside a background a little
    /// padding.
    pub(crate) fn room(&self, space: Pixels) -> (Pixels, Pixels) {
        let pad = if self.background.is_some() {
            space * 0.3
        } else {
            px(0.)
        };
        (
            space * self.margin_left.max(0.) + pad,
            space * self.margin_right.max(0.) + pad,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InlayEntry {
    pub(crate) key: SharedString,
    pub(crate) inlay: Inlay,
}

/// All the inlays of a document, by the key of whoever set them ("lsp-hints",
/// "blame"), kept in display order: by offset, `Left` before `Right`, then
/// in the order they were given.
#[derive(Default)]
pub(crate) struct InlayMap {
    entries: Vec<InlayEntry>,
}

impl InlayMap {
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn entries(&self) -> &[InlayEntry] {
        &self.entries
    }

    /// Replaces the inlays of `key` whose offset is in `range` (both ends
    /// included, so a line's end is its own), or all of them without a
    /// range, by `inlays`. Returns the offsets of the inlays removed or
    /// added, for the rows to wrap again; empty when nothing changed.
    pub(crate) fn set(
        &mut self,
        key: &SharedString,
        range: Option<Range<usize>>,
        mut inlays: Vec<Inlay>,
    ) -> Vec<usize> {
        let in_range = |offset: usize| {
            range
                .as_ref()
                .is_none_or(|r| offset >= r.start && offset <= r.end)
        };
        let (first, last) = match &range {
            Some(r) => (
                self.entries.partition_point(|e| e.inlay.offset < r.start),
                self.entries.partition_point(|e| e.inlay.offset <= r.end),
            ),
            None => (0, self.entries.len()),
        };
        inlays.sort_by_key(|inlay| (inlay.offset, inlay.bias));
        let old: Vec<&Inlay> = self.entries[first..last]
            .iter()
            .filter(|e| &e.key == key && in_range(e.inlay.offset))
            .map(|e| &e.inlay)
            .collect();
        if old.len() == inlays.len() && old.iter().zip(&inlays).all(|(a, b)| *a == b) {
            return Vec::new();
        }
        let mut changed: Vec<usize> = old.iter().map(|i| i.offset).collect();
        changed.extend(inlays.iter().map(|i| i.offset));
        // Only the window of offsets the range covers is rebuilt; the new
        // inlays may lie outside it, so they are merged in by a sort that
        // finds the rest already in order.
        let mut window: Vec<InlayEntry> = self
            .entries
            .drain(first..last)
            .filter(|e| &e.key != key || !in_range(e.inlay.offset))
            .collect();
        window.extend(inlays.into_iter().map(|inlay| InlayEntry {
            key: key.clone(),
            inlay,
        }));
        self.entries.splice(first..first, window);
        self.entries.sort_by_key(|e| (e.inlay.offset, e.inlay.bias));
        changed.sort_unstable();
        changed.dedup();
        changed
    }

    /// Follows an edit that replaced `range` of the old text with `new_len`
    /// bytes. An inlay strictly inside the replaced text goes with it; one at
    /// its start stays before the new text when it is `Right` and goes after
    /// it when it is `Left`, as typing at a cursor drawn there would put it;
    /// one at its end stays after the new text.
    pub(crate) fn edit(&mut self, range: &Range<usize>, new_len: usize) {
        if self.entries.is_empty() {
            return;
        }
        let first = self
            .entries
            .partition_point(|e| e.inlay.offset < range.start);
        if first == self.entries.len() {
            return;
        }
        let new_end = range.start + new_len;
        let mut moved = false;
        let mut dropped = false;
        for e in &mut self.entries[first..] {
            let offset = &mut e.inlay.offset;
            if *offset == range.start {
                if e.inlay.bias == InlayBias::Left && new_len > 0 {
                    *offset = new_end;
                    moved = true;
                }
            } else if *offset < range.end {
                *offset = usize::MAX;
                dropped = true;
            } else {
                *offset = *offset - range.end + new_end;
                moved |= *offset == new_end;
            }
        }
        if dropped {
            self.entries.retain(|e| e.inlay.offset != usize::MAX);
        }
        if moved {
            self.entries.sort_by_key(|e| (e.inlay.offset, e.inlay.bias));
        }
    }

    /// The inlays whose offset is in `range`, both ends included.
    pub(crate) fn in_range(&self, range: Range<usize>) -> &[InlayEntry] {
        let first = self
            .entries
            .partition_point(|e| e.inlay.offset < range.start);
        let last = self
            .entries
            .partition_point(|e| e.inlay.offset <= range.end);
        &self.entries[first..last.max(first)]
    }

    pub(crate) fn has_clickable(&self) -> bool {
        self.entries.iter().any(|e| e.inlay.id.is_some())
    }
}

/// Whether an inlay `offset` bytes into a line is drawn on the visual row
/// `row` (a byte range of that line). Inside a row it is that row's; at a
/// soft wrap boundary its bias picks the row; at the line's start and end
/// it is always on the first and the last row.
pub(crate) fn on_row(
    offset: usize,
    bias: InlayBias,
    row: &Range<usize>,
    first_row: bool,
    last_row: bool,
) -> bool {
    if offset > row.start && offset < row.end {
        return true;
    }
    let at_start = offset == row.start && (first_row || bias == InlayBias::Right);
    let at_end = offset == row.end && (last_row || bias == InlayBias::Left);
    at_start || at_end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inlay(offset: usize, text: &str, bias: InlayBias) -> Inlay {
        Inlay::new(offset, text.to_string()).bias(bias)
    }

    fn offsets(map: &InlayMap) -> Vec<(usize, &str)> {
        map.entries()
            .iter()
            .map(|e| (e.inlay.offset, e.inlay.text.as_ref()))
            .collect()
    }

    #[test]
    fn inlays_are_kept_in_display_order_by_key() {
        let mut map = InlayMap::default();
        let hints = SharedString::from("hints");
        let blame = SharedString::from("blame");
        map.set(
            &hints,
            None,
            vec![
                inlay(9, "c", InlayBias::Left),
                inlay(4, "b", InlayBias::Right),
                inlay(4, "a", InlayBias::Left),
            ],
        );
        map.set(&blame, None, vec![inlay(4, "x", InlayBias::Left)]);
        assert_eq!(offsets(&map), vec![(4, "a"), (4, "x"), (4, "b"), (9, "c")]);

        // Setting a key again replaces only that key's inlays.
        let changed = map.set(&hints, None, vec![inlay(1, "d", InlayBias::Left)]);
        assert_eq!(changed, vec![1, 4, 9]);
        assert_eq!(offsets(&map), vec![(1, "d"), (4, "x")]);

        // The same set again changes nothing.
        assert!(
            map.set(&hints, None, vec![inlay(1, "d", InlayBias::Left)])
                .is_empty()
        );
        map.set(&blame, None, vec![]);
        assert_eq!(offsets(&map), vec![(1, "d")]);
    }

    #[test]
    fn a_range_replaces_only_the_inlays_inside_it_ends_included() {
        let mut map = InlayMap::default();
        let key = SharedString::from("hints");
        let other = SharedString::from("other");
        map.set(
            &key,
            None,
            (0..10)
                .map(|i| inlay(i * 10, "h", InlayBias::Left))
                .collect(),
        );
        map.set(&other, None, vec![inlay(35, "o", InlayBias::Left)]);
        let changed = map.set(&key, Some(30..50), vec![inlay(41, "n", InlayBias::Left)]);
        assert_eq!(changed, vec![30, 40, 41, 50]);
        assert_eq!(
            offsets(&map),
            vec![
                (0, "h"),
                (10, "h"),
                (20, "h"),
                (35, "o"),
                (41, "n"),
                (60, "h"),
                (70, "h"),
                (80, "h"),
                (90, "h")
            ]
        );
        assert_eq!(map.in_range(35..60).len(), 3);
        assert_eq!(map.in_range(36..40).len(), 0);
    }

    #[test]
    fn edits_before_an_inlay_move_it_and_inside_drop_it() {
        let key = SharedString::from("k");
        let make = || {
            let mut map = InlayMap::default();
            map.set(
                &key,
                None,
                vec![
                    inlay(5, "L", InlayBias::Left),
                    inlay(5, "R", InlayBias::Right),
                    inlay(10, "far", InlayBias::Left),
                ],
            );
            map
        };

        // Typing before: both move.
        let mut map = make();
        map.edit(&(2..2), 3);
        assert_eq!(offsets(&map), vec![(8, "L"), (8, "R"), (13, "far")]);

        // Typing at the offset: the left one goes after the text, the right one stays.
        let mut map = make();
        map.edit(&(5..5), 2);
        assert_eq!(offsets(&map), vec![(5, "R"), (7, "L"), (12, "far")]);

        // Deleting the character before: both follow the end of the deletion.
        let mut map = make();
        map.edit(&(4..5), 0);
        assert_eq!(offsets(&map), vec![(4, "L"), (4, "R"), (9, "far")]);

        // Replacing text around the offset drops them.
        let mut map = make();
        map.edit(&(3..7), 1);
        assert_eq!(offsets(&map), vec![(7, "far")]);

        // Edits after leave them be.
        let mut map = make();
        map.edit(&(6..9), 0);
        assert_eq!(offsets(&map), vec![(5, "L"), (5, "R"), (7, "far")]);

        // Deleting from the offset on keeps them where the deletion starts.
        let mut map = make();
        map.edit(&(5..8), 0);
        assert_eq!(offsets(&map), vec![(5, "L"), (5, "R"), (7, "far")]);
    }

    #[test]
    fn a_soft_wrap_at_an_inlay_leaves_it_on_the_row_of_its_text() {
        let first = 0..10;
        let second = 10..20;
        // Inside a row.
        assert!(on_row(5, InlayBias::Left, &first, true, false));
        assert!(!on_row(5, InlayBias::Left, &second, false, true));
        // At the wrap: a left inlay ends the upper row, a right one starts the lower.
        assert!(on_row(10, InlayBias::Left, &first, true, false));
        assert!(!on_row(10, InlayBias::Left, &second, false, true));
        assert!(!on_row(10, InlayBias::Right, &first, true, false));
        assert!(on_row(10, InlayBias::Right, &second, false, true));
        // The line's start and end are always on the first and last row.
        assert!(on_row(0, InlayBias::Left, &first, true, false));
        assert!(on_row(20, InlayBias::Right, &second, false, true));
        // An empty line has one row holding them all.
        assert!(on_row(0, InlayBias::Right, &(0..0), true, true));
    }
}
