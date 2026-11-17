//! ideae patch: setting the inlays of an editor (text shown that is not in
//! the buffer, see `display_map/inlay_map.rs`), and the pointer over them.

use std::ops::Range;

use gpui::{Context, Pixels, Point, SharedString, point, px};

use super::{InputBaseState, InputEvent, kind::InputModeKind};
use crate::input::Inlay;

impl<M: InputModeKind> InputBaseState<M> {
    /// Replaces the inlays set under `key` whose offset is in `range` (both
    /// ends included, so a line's end is its own), or all of them without a
    /// range, by `inlays`. Only the lines whose inlays changed are wrapped
    /// again; what is drawn is shaped for the rows in view only. Multi-line
    /// editors draw them; a single-line input keeps them but does not.
    pub fn set_inlays(
        &mut self,
        key: impl Into<SharedString>,
        range: Option<Range<usize>>,
        inlays: Vec<Inlay>,
        cx: &mut Context<Self>,
    ) {
        if self.display_map.set_inlays(&key.into(), range, inlays, cx) {
            if self.mode.is_auto_grow() {
                self.mode.update_auto_grow(&self.display_map);
            }
            // Found again at the next move of the pointer.
            self.hovered_inlay = None;
            cx.notify();
        }
    }

    /// Removes the inlays set under `key`.
    pub fn clear_inlays(&mut self, key: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.set_inlays(key, None, Vec::new(), cx);
    }

    /// Every inlay, in the order they are drawn.
    pub fn inlays(&self) -> impl Iterator<Item = &Inlay> {
        self.display_map.inlays()
    }

    /// The id of the clickable inlay under `position` (window coordinates).
    pub(super) fn inlay_at_mouse_position(&self, position: Point<Pixels>) -> Option<SharedString> {
        if !self.is_multi_line() || !self.display_map.has_clickable_inlays() {
            return None;
        }
        let (bounds, last_layout) = (self.last_bounds.as_ref()?, self.last_layout.as_ref()?);
        let inner = position - bounds.origin - point(last_layout.line_number_width, px(0.));
        let mut top = last_layout.visible_top;
        for line in last_layout.lines.iter() {
            let height = line.size(last_layout.line_height).height;
            if inner.y >= top && inner.y < top + height {
                return line
                    .inlay_at_position(inner - point(px(0.), top), last_layout)?
                    .id
                    .clone();
            }
            top += height;
        }
        None
    }

    /// Follows the pointer over clickable inlays: the one under it is drawn
    /// underlined and takes the pointing hand.
    pub(super) fn inlay_hover(&mut self, position: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let hovered = position.and_then(|p| self.inlay_at_mouse_position(p));
        if hovered != self.hovered_inlay {
            self.hovered_inlay = hovered;
            cx.notify();
        }
    }

    /// A press on a clickable inlay is told to the app and goes no further.
    pub(super) fn inlay_press(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.inlay_at_mouse_position(position) else {
            return false;
        };
        cx.emit(InputEvent::InlayClick { id });
        true
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{
        AppContext as _, ClipboardItem, Entity, IntoElement, ParentElement as _, Render,
        Styled as _, TestAppContext, VisualTestContext, Window, div,
    };

    use super::*;
    use crate::input::{
        Copy, EditorMode, EditorState, InlayBias, InlayStyle, MoveDown, MoveLeft, MoveRight,
        MoveUp, RopeExt as _,
    };

    struct Root(Entity<InputBaseState<EditorMode>>, Pixels);

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(self.1).h(px(400.)).child(self.0.clone())
        }
    }

    /// An editor `width` wide, with `text`, drawn once.
    fn editor(
        cx: &mut TestAppContext,
        width: f32,
        text: &str,
    ) -> (Entity<InputBaseState<EditorMode>>, VisualTestContext) {
        cx.update(crate::init);
        let mut input = None;
        let text = text.to_string();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.set_global(crate::theme::Theme::default());
                let state = cx.new(|cx| {
                    let mut state = EditorState::new(window, cx).soft_wrap(true);
                    state.set_value(text, window, cx);
                    state
                });
                input = Some(state.clone());
                cx.new(|_| Root(state, px(width)))
            })
            .unwrap()
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        draw(&mut cx);
        (input.unwrap(), cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn set(
        cx: &mut VisualTestContext,
        input: &Entity<InputBaseState<EditorMode>>,
        key: &str,
        inlays: Vec<Inlay>,
    ) {
        let key = key.to_string();
        cx.update(|_, cx| input.update(cx, |state, cx| state.set_inlays(key, None, inlays, cx)));
        draw(cx);
    }

    /// The width of one character: the test text system's font is monospaced.
    fn char_width(
        input: &Entity<InputBaseState<EditorMode>>,
        cx: &mut VisualTestContext,
    ) -> Pixels {
        input.read_with(cx, |state, _| {
            state.last_layout.as_ref().unwrap().space_width
        })
    }

    /// Where the caret at `offset` is drawn: x from the text's left edge and
    /// the visual row, counted over all the visible lines.
    fn caret(
        input: &Entity<InputBaseState<EditorMode>>,
        cx: &mut VisualTestContext,
        offset: usize,
    ) -> (Pixels, usize) {
        input.read_with(cx, |state, _| {
            let layout = state.last_layout.as_ref().unwrap();
            let row = state.text.offset_to_point(offset).row;
            let vi = layout
                .visible_buffer_lines
                .iter()
                .position(|&r| r == row)
                .unwrap();
            let rows_above: usize = layout.lines[..vi]
                .iter()
                .map(|l| l.wrapped_lines.len())
                .sum();
            let local = offset - layout.visible_line_byte_offsets[vi];
            let p = layout.lines[vi]
                .position_for_index(local, layout, false)
                .unwrap();
            (
                p.x,
                rows_above + (p.y / layout.line_height).round() as usize,
            )
        })
    }

    /// The window position of `x` on visual row `row`, both as `caret` gives them.
    fn at(
        input: &Entity<InputBaseState<EditorMode>>,
        cx: &mut VisualTestContext,
        x: Pixels,
        row: usize,
    ) -> Point<Pixels> {
        input.read_with(cx, |state, _| {
            let layout = state.last_layout.as_ref().unwrap();
            state.last_bounds.unwrap().origin
                + point(
                    layout.line_number_width + x,
                    layout.visible_top + layout.line_height * (row as f32 + 0.5),
                )
        })
    }

    #[track_caller]
    fn assert_near(actual: Pixels, expected: Pixels) {
        assert!(
            (actual - expected).abs() < px(0.01),
            "{actual:?} != {expected:?}"
        );
    }

    #[track_caller]
    fn assert_caret(
        input: &Entity<InputBaseState<EditorMode>>,
        cx: &mut VisualTestContext,
        offset: usize,
        x: Pixels,
        row: usize,
    ) {
        let (actual_x, actual_row) = caret(input, cx, offset);
        assert_eq!(actual_row, row, "row of {offset}");
        assert_near(actual_x, x);
    }

    fn hint(offset: usize, text: &str, bias: InlayBias) -> Inlay {
        Inlay::new(offset, text.to_string()).bias(bias)
    }

    #[gpui::test]
    fn inlays_shift_the_text_after_them_and_the_caret_sits_by_their_text(cx: &mut TestAppContext) {
        let (input, mut cx) = editor(cx, 800., "let x = 5;\nfoo(1, 2);");
        let w = char_width(&input, &mut cx);
        set(
            &mut cx,
            &input,
            "hints",
            vec![
                // ": i32" hangs on `x`, "a: " and "b: " on the arguments.
                hint(5, ": i32", InlayBias::Left),
                hint(15, "a: ", InlayBias::Right),
                hint(18, "b: ", InlayBias::Right),
            ],
        );
        // Before the type nothing moved; after it everything moved by it.
        assert_caret(&input, &mut cx, 4, w * 4., 0);
        assert_caret(&input, &mut cx, 5, w * 5., 0);
        assert_caret(&input, &mut cx, 6, w * 11., 0);
        // The caret at a parameter hint is drawn after it, by its argument.
        assert_caret(&input, &mut cx, 14, w * 3., 1);
        assert_caret(&input, &mut cx, 15, w * 7., 1);
        assert_caret(&input, &mut cx, 18, w * 13., 1);
        assert_caret(&input, &mut cx, 21, w * 16., 1);

        // Several at one offset: left ones first, the caret between them.
        set(
            &mut cx,
            &input,
            "hints",
            vec![
                hint(5, "R", InlayBias::Right),
                hint(5, "L", InlayBias::Left),
            ],
        );
        assert_caret(&input, &mut cx, 5, w * 5., 0);
        assert_caret(&input, &mut cx, 6, w * 8., 0);
        let order: Vec<_> =
            input.read_with(&cx, |s, _| s.inlays().map(|i| i.text.to_string()).collect());
        assert_eq!(order, vec!["L", "R"]);
    }

    #[gpui::test]
    fn the_caret_steps_over_an_inlay_and_never_lands_in_it(cx: &mut TestAppContext) {
        let (input, mut cx) = editor(cx, 800., "abcdef\nabcdef");
        let w = char_width(&input, &mut cx);
        set(
            &mut cx,
            &input,
            "hints",
            vec![hint(2, "xyz", InlayBias::Right)],
        );
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.move_to(1, None, cx);
                state.right(&MoveRight, window, cx);
                assert_eq!(state.cursor(), 2);
                state.right(&MoveRight, window, cx);
                assert_eq!(state.cursor(), 3);
                state.left(&MoveLeft, window, cx);
                state.left(&MoveLeft, window, cx);
                assert_eq!(state.cursor(), 1);
            })
        });
        draw(&mut cx);
        assert_caret(&input, &mut cx, 2, w * 5., 0);
        // Up into the inlay lands on its offset; down from after it keeps
        // the x it was drawn at.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.move_to(7 + 4, None, cx);
                state.update_preferred_column();
                state.up(&MoveUp, window, cx);
                assert_eq!(state.cursor(), 2);
                state.move_to(4, None, cx);
                state.update_preferred_column();
                state.down(&MoveDown, window, cx);
                assert_eq!(state.cursor(), 7 + 6);
            })
        });
    }

    #[gpui::test]
    fn a_click_on_an_inlay_is_its_own_and_beside_it_places_the_caret(cx: &mut TestAppContext) {
        let (input, mut cx) = editor(cx, 800., "let x = 5;\nsecond");
        let w = char_width(&input, &mut cx);
        let clicks = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let clicks = clicks.clone();
            cx.subscribe(&input, move |_, event: &InputEvent, _| {
                if let InputEvent::InlayClick { id } = event {
                    clicks.borrow_mut().push(id.to_string());
                }
            })
            .detach();
        });
        set(
            &mut cx,
            &input,
            "hints",
            vec![
                hint(5, ": i32", InlayBias::Left),
                Inlay::new(10, "Ann, 2 days ago")
                    .style(InlayStyle {
                        margin_left: 3.,
                        ..Default::default()
                    })
                    .id("abc123"),
            ],
        );
        let none = gpui::Modifiers::default();
        // On a type hint: the caret goes to its offset.
        let p = at(&input, &mut cx, w * 7.5, 0);
        cx.simulate_click(p, none);
        assert_eq!(input.read_with(&cx, |s, _| s.cursor()), 5);
        // On the note at the end of the line: told to the app, the caret stays.
        let p = at(&input, &mut cx, w * (15. + 3. + 4.), 0);
        cx.simulate_mouse_move(p, None, none);
        assert_eq!(
            input.read_with(&cx, |s, _| s.hovered_inlay.clone()),
            Some("abc123".into())
        );
        cx.simulate_click(p, none);
        assert_eq!(*clicks.borrow(), vec!["abc123".to_string()]);
        assert_eq!(input.read_with(&cx, |s, _| s.cursor()), 5);
        // Past it the caret goes to the line's end.
        let p = at(&input, &mut cx, w * 60., 0);
        cx.simulate_click(p, none);
        assert_eq!(input.read_with(&cx, |s, _| s.cursor()), 10);
        assert_eq!(clicks.borrow().len(), 1);
        // Off it, the pointer is no longer on it.
        let p = at(&input, &mut cx, w * 2., 1);
        cx.simulate_mouse_move(p, None, none);
        assert_eq!(input.read_with(&cx, |s, _| s.hovered_inlay.clone()), None);
    }

    #[gpui::test]
    fn copying_and_selecting_take_only_the_text(cx: &mut TestAppContext) {
        let (input, mut cx) = editor(cx, 800., "let x = 5;");
        let w = char_width(&input, &mut cx);
        set(
            &mut cx,
            &input,
            "hints",
            vec![hint(5, ": i32", InlayBias::Left)],
        );
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_selected_range(4..7, cx);
                state.copy(&Copy, window, cx);
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("x =".to_string())
                );
                state.select_all(window, cx);
                state.copy(&Copy, window, cx);
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("let x = 5;".to_string())
                );
                cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
            })
        });
        draw(&mut cx);
        // A selection ending at the hint's offset ends before the hint (it
        // hangs on the text before).
        let bounds = input.read_with(&cx, |s, _| s.range_to_bounds(&(2..5)).unwrap());
        assert!((bounds.size.width - w * 3.).abs() < px(0.5), "{bounds:?}");
        // One across it covers it.
        let bounds = input.read_with(&cx, |s, _| s.range_to_bounds(&(4..6)).unwrap());
        assert!((bounds.size.width - w * 7.).abs() < px(0.5), "{bounds:?}");
    }

    #[gpui::test]
    fn inlays_take_room_when_lines_wrap_but_one_at_the_end_does_not(cx: &mut TestAppContext) {
        let (input, mut cx) = editor(cx, 800., "a");
        let w = char_width(&input, &mut cx);
        // A text exactly as wide as the room for it.
        let room = input.read_with(&cx, |s, _| {
            s.last_layout.as_ref().unwrap().wrap_width.unwrap()
        });
        let chars = (room / w).floor() as usize;
        let line = "a".repeat(chars - 9) + " bbb cccc";
        cx.update(|window, cx| input.update(cx, |s, cx| s.set_value(line.clone(), window, cx)));
        draw(&mut cx);
        let rows =
            |cx: &mut VisualTestContext| input.read_with(cx, |s, _| s.display_map.wrap_row_count());
        assert_eq!(rows(&mut cx), 1);

        // At the end, a note does not wrap the line.
        set(
            &mut cx,
            &input,
            "blame",
            vec![hint(line.len(), "a long note at the end", InlayBias::Left)],
        );
        assert_eq!(rows(&mut cx), 1);

        // In the middle, a hint takes room: the last word goes down.
        set(
            &mut cx,
            &input,
            "hints",
            vec![hint(3, ": u8", InlayBias::Left)],
        );
        assert_eq!(rows(&mut cx), 2);
        let cccc = line.len() - 4;
        assert_caret(&input, &mut cx, cccc, px(0.), 1);
        assert_caret(&input, &mut cx, 4, w * 8., 0);
        // The note went down with the end of the line.
        assert_caret(&input, &mut cx, line.len(), w * 4., 1);

        // A hint at the wrap stays on the row of its text: a right one
        // starts the lower row.
        set(
            &mut cx,
            &input,
            "hints",
            vec![
                hint(1, "....", InlayBias::Left),
                hint(cccc, "<", InlayBias::Right),
            ],
        );
        assert_eq!(rows(&mut cx), 2);
        assert_caret(&input, &mut cx, cccc, w, 1);
        set(&mut cx, &input, "hints", vec![]);
        assert_eq!(rows(&mut cx), 1);
    }

    #[gpui::test]
    fn edits_move_inlays_with_their_text(cx: &mut TestAppContext) {
        let (input, mut cx) = editor(cx, 800., "let x = 5;\nfoo(1);");
        set(
            &mut cx,
            &input,
            "hints",
            vec![
                hint(5, ": i32", InlayBias::Left),
                hint(15, "a: ", InlayBias::Right),
            ],
        );
        let offsets = |cx: &mut VisualTestContext| {
            input.read_with(cx, |s, _| s.inlays().map(|i| i.offset).collect::<Vec<_>>())
        };
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.move_to(5, None, cx);
            })
        });
        // Typing at the end of the name: the type follows it.
        cx.simulate_input("yz");
        assert_eq!(offsets(&mut cx), vec![7, 17]);
        // Typing at the argument: its name stays before.
        cx.update(|_, cx| input.update(cx, |s, cx| s.move_to(17, None, cx)));
        cx.simulate_input("0");
        assert_eq!(offsets(&mut cx), vec![7, 17]);
        assert_eq!(
            input.read_with(&cx, |s, _| s.value().to_string()),
            "let xyz = 5;\nfoo(01);"
        );
        // Replacing the text around an inlay takes it away.
        cx.update(|_, cx| input.update(cx, |s, cx| s.set_selected_range(16..18, cx)));
        cx.simulate_input("x");
        assert_eq!(offsets(&mut cx), vec![7]);
        // A new text drops them all.
        cx.update(|window, cx| input.update(cx, |s, cx| s.set_value("other", window, cx)));
        assert!(offsets(&mut cx).is_empty());
    }

    #[gpui::test]
    fn multibyte_text_keeps_its_offsets_and_widths(cx: &mut TestAppContext) {
        let text = "日本語 ok 👍 end";
        let (input, mut cx) = editor(cx, 800., text);
        let ok = text.find("ok").unwrap();
        let end = text.find("end").unwrap();
        let (before_ok, _) = caret(&input, &mut cx, ok);
        let (before_end, _) = caret(&input, &mut cx, end);
        set(
            &mut cx,
            &input,
            "hints",
            vec![
                hint(ok, "<<", InlayBias::Right),
                // Inside a character: dropped.
                hint(1, "bad", InlayBias::Left),
                hint(end, ">", InlayBias::Right),
            ],
        );
        assert_eq!(input.read_with(&cx, |s, _| s.inlays().count()), 2);
        let w = char_width(&input, &mut cx);
        assert_near(caret(&input, &mut cx, ok - 1).0, before_ok - w);
        assert_near(caret(&input, &mut cx, ok).0, before_ok + w * 2.);
        assert_near(caret(&input, &mut cx, end).0, before_end + w * 3.);
        // A click on the second hint lands on its offset.
        let p = at(&input, &mut cx, before_end + w * 2.5, 0);
        cx.simulate_click(p, gpui::Modifiers::default());
        assert_eq!(input.read_with(&cx, |s, _| s.cursor()), end);
    }

    #[gpui::test]
    fn folded_lines_hide_their_inlays_and_the_rest_stay_in_place(cx: &mut TestAppContext) {
        let text = "fn a() {\n    let x = 1;\n}\nlet y = 2;";
        let (input, mut cx) = editor(cx, 800., text);
        let w = char_width(&input, &mut cx);
        let x = text.find("x =").unwrap() + 1;
        let y = text.find("y =").unwrap() + 1;
        set(
            &mut cx,
            &input,
            "hints",
            vec![
                hint(x, ": i32", InlayBias::Left),
                hint(y, ": i32", InlayBias::Left),
            ],
        );
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                state
                    .display_map
                    .set_fold_candidates(vec![crate::input::FoldRange {
                        start_line: 0,
                        end_line: 2,
                    }]);
                state.display_map.set_folded(0, true);
                cx.notify();
            })
        });
        draw(&mut cx);
        let visible = input.read_with(&cx, |s, _| {
            s.last_layout.as_ref().unwrap().visible_buffer_lines.clone()
        });
        assert!(!visible.contains(&1), "{visible:?}");
        let (x_after, row) = caret(&input, &mut cx, y + 1);
        assert_near(x_after, w * 11.);
        assert_eq!(row, visible.iter().position(|&r| r == 3).unwrap());
    }

    type Asked = Rc<
        RefCell<
            Vec<(
                Range<usize>,
                futures::channel::oneshot::Sender<Vec<lsp_types::InlayHint>>,
            )>,
        >,
    >;

    /// A server that answers when the test says so.
    struct Server(Asked);

    impl crate::input::InlayHintsProvider for Server {
        fn inlay_hints(
            &self,
            _: &crate::input::Rope,
            range: Range<usize>,
            _: &mut Window,
            cx: &mut gpui::App,
        ) -> gpui::Task<anyhow::Result<Vec<lsp_types::InlayHint>>> {
            let (tx, rx) = futures::channel::oneshot::channel();
            self.0.borrow_mut().push((range, tx));
            cx.background_executor()
                .spawn(async move { Ok(rx.await.unwrap_or_default()) })
        }
    }

    fn type_hint(line: u32, character: u32, label: &str) -> lsp_types::InlayHint {
        lsp_types::InlayHint {
            position: lsp_types::Position::new(line, character),
            label: lsp_types::InlayHintLabel::String(label.into()),
            kind: Some(lsp_types::InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        }
    }

    #[gpui::test]
    fn hints_are_asked_for_the_view_and_a_stale_answer_is_dropped(cx: &mut TestAppContext) {
        let (input, mut cx) = editor(cx, 800., "let x = 5;\nlet y = 6;");
        let asked: Asked = Rc::default();
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                state.lsp_mut().inlay_hints_provider = Some(Rc::new(Server(asked.clone())));
                cx.notify();
            })
        });
        draw(&mut cx);
        cx.run_until_parked();
        let texts = |cx: &mut VisualTestContext| {
            input.read_with(cx, |s, _| {
                s.inlays()
                    .map(|i| (i.offset, i.text.to_string()))
                    .collect::<Vec<_>>()
            })
        };
        // The first paint asks at once, for every line.
        let (range, answer) = asked.borrow_mut().remove(0);
        assert_eq!(range, 0..21);
        answer.send(vec![type_hint(0, 5, ": i32")]).unwrap();
        cx.run_until_parked();
        assert_eq!(texts(&mut cx), vec![(5, ": i32".to_string())]);

        // Typing asks again after a pause; what answers meanwhile for the
        // old text is not shown.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.move_to(0, None, cx);
            })
        });
        cx.simulate_input("x");
        draw(&mut cx);
        cx.run_until_parked();
        assert!(asked.borrow().is_empty());
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(700));
        cx.run_until_parked();
        let (_, answer) = asked.borrow_mut().remove(0);
        cx.simulate_input("y");
        // Typing again dropped the request: its answer finds no one.
        let _ = answer.send(vec![type_hint(1, 0, "stale")]);
        cx.run_until_parked();
        assert_eq!(texts(&mut cx), vec![(7, ": i32".to_string())]);

        // The fresh answer replaces the hints.
        draw(&mut cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(700));
        cx.run_until_parked();
        let (_, answer) = asked.borrow_mut().remove(0);
        answer.send(vec![type_hint(1, 5, ": u8")]).unwrap();
        cx.run_until_parked();
        assert_eq!(texts(&mut cx), vec![(18, ": u8".to_string())]);
    }
}
