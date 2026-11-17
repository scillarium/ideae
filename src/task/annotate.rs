use super::*;

use gpui_kit::base::input::{GutterEvent, GutterNote, GutterNotes};

// ---------------------------------------------------------------- annotate

actions!(ideae_file, [ToggleBlame]);

/// The column's width in characters: a date, a space, a name.
const WIDTH: usize = 20;
const NAME: usize = 9;

/// "2026-11-03 Vjaceslavs": the day of the commit and the author's first
/// name, cut short.
pub(super) fn blame_label(line: &git::BlameLine) -> String {
    let name: String = line.author.split_whitespace().next().unwrap_or("").chars().take(NAME).collect();
    format!("{} {name}", day(line.time))
}

/// The day of a Unix time, as 2026-11-03 (UTC).
pub(super) fn day(time: i64) -> String {
    // Howard Hinnant's days-to-civil.
    let z = time.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A note by each line that a commit has: its label on the first line of
/// a run of the same commit, and a tint that is stronger the newer it is.
pub(super) fn blame_notes(blame: &[Option<Arc<git::BlameLine>>], text: Hsla, tint: Hsla) -> GutterNotes {
    let times = || blame.iter().flatten().filter(|l| !l.uncommitted()).map(|l| l.time);
    let (oldest, newest) = (times().min().unwrap_or(0), times().max().unwrap_or(0));
    let span = (newest - oldest).max(1) as f32;
    let mut previous: Option<&str> = None;
    let mut label = SharedString::default();
    let uncommitted = SharedString::from("not committed");
    let rows = blame
        .iter()
        .map(|line| {
            let Some(line) = line.as_ref() else {
                previous = None;
                return None;
            };
            let first = previous != Some(line.hash.as_str());
            previous = Some(&line.hash);
            if line.uncommitted() {
                return Some(GutterNote { text: if first { uncommitted.clone() } else { SharedString::default() }, run: uncommitted.clone(), color: text.opacity(0.6), tint: None });
            }
            if first {
                label = blame_label(line).into();
            }
            let age = (line.time - oldest) as f32 / span;
            Some(GutterNote {
                text: if first { label.clone() } else { SharedString::default() },
                run: label.clone(),
                color: text,
                tint: Some(tint.opacity(0.04 + 0.18 * age)),
            })
        })
        .collect();
    GutterNotes { chars: WIDTH, rows }
}

/// The key the blame of the cursor's line is set under, as an inlay.
const LINE_BLAME: &str = "blame";
/// What starts the id of that inlay, before the commit's hash.
pub(super) const LINE_BLAME_ID: &str = "blame:";

/// The blame of a line as told at its end: who, when, why; or that it is
/// not committed yet.
pub(super) fn line_blame_text(line: &git::BlameLine) -> String {
    if line.uncommitted() {
        return "Not committed yet".into();
    }
    const SUMMARY: usize = 60;
    let summary = line.summary.trim();
    let summary = match summary.char_indices().nth(SUMMARY) {
        Some((cut, _)) => format!("{}…", summary[..cut].trim_end()),
        None => summary.to_string(),
    };
    format!("{}, {} · {summary}", line.author, git::ago(line.time))
}

impl FileView {
    /// Who last changed the cursor's line, at its end: from the blame read
    /// when the file was, nothing per move but the one inlay. Not while the
    /// text has unsaved edits (the lines moved), nor when Settings put it
    /// in the header.
    pub(super) fn show_line_blame(&mut self, cx: &mut Context<Self>) {
        let line = self.cursor_line as usize;
        let info = self
            .blame
            .get(line)
            .cloned()
            .flatten()
            .filter(|_| !self.dirty && crate::state::with_settings(|s| s.blame_at_line_end));
        let color = cx.theme().muted_foreground.opacity(0.75);
        self.editor.update(cx, |state, cx| {
            let inlays = info
                .filter(|_| line < state.text().lines_len())
                .map(|info| {
                    let inlay = Inlay::new(state.text().line_end_offset(line), line_blame_text(&info))
                        .style(InlayStyle { color: Some(color), margin_left: 4., ..Default::default() });
                    if info.uncommitted() { inlay } else { inlay.id(format!("{LINE_BLAME_ID}{}", info.hash)) }
                })
                .into_iter()
                .collect();
            state.set_inlays(LINE_BLAME, None, inlays, cx);
        });
    }

    /// Annotate with Git Blame, or close the annotations.
    pub(super) fn toggle_blame(&mut self, cx: &mut Context<Self>) {
        self.annotate = !self.annotate;
        self.blame_hover = None;
        self.annotations = None;
        self.show_annotations(cx);
        cx.notify();
    }

    /// Puts the annotations in the gutter as they are now: the column stays
    /// while there are unsaved edits, empty, since the lines have moved.
    pub(super) fn show_annotations(&mut self, cx: &mut Context<Self>) {
        let notes = if !self.annotate {
            None
        } else if self.dirty {
            Some(Rc::new(GutterNotes { chars: WIDTH, rows: Vec::new() }))
        } else {
            let (text, tint) = (cx.theme().muted_foreground, cx.theme().primary);
            let blame = self.blame.clone();
            Some(self.annotations.get_or_insert_with(|| Rc::new(blame_notes(&blame, text, tint))).clone())
        };
        self.editor.update(cx, |state, cx| state.set_gutter_notes(notes, cx));
    }

    pub(super) fn on_gutter(&mut self, event: &GutterEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GutterEvent::Hover(at) => {
                self.blame_hover = *at;
                cx.notify();
            }
            GutterEvent::Change(row) => self.show_change(*row, cx),
            GutterEvent::Click(row) => {
                let Some(line) = self.blame.get(*row).cloned().flatten().filter(|l| !l.uncommitted()) else { return };
                // Once this file is no longer being updated: the task reads it.
                let task = self.task.clone();
                window.defer(cx, move |window, cx| {
                    let _ = task.update(cx, |t, cx| t.show_commit(&line.hash, window, cx));
                });
            }
        }
    }

    /// Who changed the line under the pointer, when and why.
    pub(super) fn render_blame_hover(&self, cx: &App) -> Option<AnyElement> {
        let (row, at) = self.blame_hover.filter(|_| self.annotate && !self.dirty)?;
        let line = self.blame.get(row)?.clone().filter(|l| !l.uncommitted())?;
        Some(
            deferred(
                anchored().position(at + point(px(12.), px(16.))).snap_to_window().child(
                    v_flex()
                        .max_w(px(520.))
                        .gap_1()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().popover)
                        .text_color(cx.theme().popover_foreground)
                        .text_xs()
                        .shadow_md()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(line.summary.clone()))
                        .child(format!("{} · {} ({})", line.author, day(line.time), git::ago(line.time)))
                        .child(div().text_color(cx.theme().muted_foreground).child(format!("{} · click to open", &line.hash[..line.hash.len().min(10)]))),
                ),
            )
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{WIDTH, blame_label, blame_notes, day, line_blame_text};
    use crate::git;
    use std::sync::Arc;

    fn line(hash: &str, author: &str, time: i64) -> Option<Arc<git::BlameLine>> {
        Some(Arc::new(git::BlameLine { hash: hash.into(), author: author.into(), time, summary: String::new() }))
    }

    #[test]
    fn a_label_is_the_day_and_the_first_name() {
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(1_709_164_800), "2024-02-29");
        assert_eq!(day(1_793_664_000), "2026-11-03");
        let l = line("abc", "Maximilian Mustermann", 1_793_664_000).unwrap();
        assert_eq!(blame_label(&l), "2026-11-03 Maximilia");
        assert!(blame_label(&l).chars().count() <= WIDTH);
    }

    #[test]
    fn the_blame_at_a_line_end_is_who_when_and_why_cut_short() {
        let mut l = git::BlameLine { hash: "abc".into(), author: "Ann Lee".into(), time: 0, summary: "Fix the thing".into() };
        let text = line_blame_text(&l);
        assert!(text.starts_with("Ann Lee, ") && text.ends_with(" · Fix the thing"), "{text}");
        l.summary = "word ".repeat(30);
        let text = line_blame_text(&l);
        assert!(text.ends_with("word…") && text.chars().count() < 100, "{text}");
        l.hash = "0".repeat(40);
        assert_eq!(line_blame_text(&l), "Not committed yet");
    }

    #[test]
    fn a_run_of_one_commit_is_labelled_once() {
        let zero = "0".repeat(40);
        let blame = vec![line("a", "Ann", 100), line("a", "Ann", 100), line("b", "Bob", 200), line(&zero, "Not Committed Yet", 300), None, line("a", "Ann", 100)];
        let notes = blame_notes(&blame, gpui_kit::black(), gpui_kit::black());
        let texts: Vec<Option<String>> = notes.rows.iter().map(|n| n.as_ref().map(|n| n.text.to_string())).collect();
        assert_eq!(
            texts,
            vec![
                Some("1970-01-01 Ann".into()),
                Some(String::new()),
                Some("1970-01-01 Bob".into()),
                Some("not committed".into()),
                None,
                Some("1970-01-01 Ann".into()),
            ]
        );
        // Each row knows its run's label, for the top row in view.
        assert_eq!(notes.rows[1].as_ref().map(|n| n.run.to_string()), Some("1970-01-01 Ann".into()));
        // The newer commit is tinted more.
        let tint = |ix: usize| notes.rows[ix].as_ref().unwrap().tint.unwrap().a;
        assert!(tint(2) > tint(0));
    }
}
