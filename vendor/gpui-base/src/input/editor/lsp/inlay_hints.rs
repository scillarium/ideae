//! ideae patch: the language server's inlay hints (`textDocument/inlayHint`)
//! shown as inlays: types after names, parameter names before arguments.

use std::ops::Range;

use anyhow::Result;
use gpui::{App, Context, SharedString, Task, Window};
use instant::Duration;
use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel};
use ropey::Rope;

use crate::input::{EditorMode, Inlay, InlayBias, InlayStyle, InputBaseState, RopeExt};

/// The key the hints are set under (see [`InputBaseState::set_inlays`]).
pub const INLAY_HINTS_KEY: &str = "lsp-hints";

/// How long typing pauses before the hints are asked for again. Longer than
/// a host's wait before it tells the server of the edit, so the server
/// answers for the text it is asked about.
const AFTER_TYPING: Duration = Duration::from_millis(600);
/// How long the view rests after a scroll before the rows it shows are
/// asked for.
const AFTER_SCROLL: Duration = Duration::from_millis(80);

/// A provider of inlay hints, installed on [`crate::input::Lsp`].
pub trait InlayHintsProvider {
    /// textDocument/inlayHint for a byte range of `text`.
    ///
    /// <https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocument_inlayHint>
    fn inlay_hints(
        &self,
        text: &Rope,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<InlayHint>>>;
}

/// What was last asked for: the document's revision and the buffer lines.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HintsAsked {
    revision: u64,
    lines: Range<usize>,
}

/// The lines to ask for when `visible` lines are in view: as many again
/// above and below, so a scroll finds them there.
fn lines_to_ask(visible: &Range<usize>, line_count: usize) -> Range<usize> {
    let margin = visible.len().max(20);
    visible.start.saturating_sub(margin)..(visible.end + margin).min(line_count)
}

/// The hints as inlays: a type hangs on the name before it, a parameter's
/// name on the argument after it. Hints off the text are dropped.
fn hints_to_inlays(text: &Rope, hints: Vec<InlayHint>, style: &InlayStyle) -> Vec<Inlay> {
    hints
        .into_iter()
        .filter_map(|hint| {
            let label: String = match hint.label {
                InlayHintLabel::String(label) => label,
                InlayHintLabel::LabelParts(parts) => parts.into_iter().map(|p| p.value).collect(),
            };
            let label = label.replace(['\n', '\r'], " ");
            if label.trim().is_empty() || hint.position.line as usize >= text.lines_len() {
                return None;
            }
            let bias = if hint.kind == Some(InlayHintKind::PARAMETER) {
                InlayBias::Right
            } else {
                InlayBias::Left
            };
            let mut style = style.clone();
            if hint.padding_left == Some(true) {
                style.margin_left += 0.5;
            }
            if hint.padding_right == Some(true) {
                style.margin_right += 0.5;
            }
            Some(Inlay {
                offset: text.position_to_offset(&hint.position),
                bias,
                text: SharedString::from(label),
                style,
                id: None,
            })
        })
        .collect()
}

impl InputBaseState<EditorMode> {
    /// Asks for the hints again at the next paint, for the whole view: after
    /// the provider was changed, the server learned more, or showing them
    /// was turned on or off. Without a provider they are taken away.
    pub fn refresh_inlay_hints(&mut self, cx: &mut Context<Self>) {
        self.extras.lsp.inlay_hints_asked = None;
        self.extras.lsp._inlay_hints_task = Task::ready(());
        if self.extras.lsp.inlay_hints_provider.is_none() {
            self.clear_inlays(INLAY_HINTS_KEY, cx);
        }
        cx.notify();
    }

    /// Called after every paint: asks for the hints of the lines in view
    /// when the text changed since they were asked for, or the view moved
    /// past them. Costs a comparison when neither did.
    pub(crate) fn inlay_hints_after_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.extras.lsp.inlay_hints_provider.is_none() {
            return;
        }
        let Some(asked) = self.extras.lsp.inlay_hints_asked.as_ref() else {
            return self.request_inlay_hints(true, Duration::ZERO, window, cx);
        };
        let Some(visible) = self.last_layout.as_ref().map(|l| l.visible_range.clone()) else {
            return;
        };
        if asked.revision != self.document_revision {
            self.request_inlay_hints(true, AFTER_TYPING, window, cx);
        } else if visible.start < asked.lines.start || visible.end > asked.lines.end {
            self.request_inlay_hints(false, AFTER_SCROLL, window, cx);
        }
    }

    /// After `delay`, asks for the hints of the lines in view and around
    /// them. An answer for a text that has changed since is dropped. A new
    /// text's hints replace all of them; a scroll's only those of its lines.
    fn request_inlay_hints(
        &mut self,
        text_changed: bool,
        delay: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.extras.lsp.inlay_hints_provider.clone() else {
            return;
        };
        let Some(visible) = self.last_layout.as_ref().map(|l| l.visible_range.clone()) else {
            return;
        };
        let revision = self.document_revision;
        let lines = lines_to_ask(&visible, self.text.lines_len());
        self.extras.lsp.inlay_hints_asked = Some(HintsAsked {
            revision,
            lines: lines.clone(),
        });
        let text = self.text.clone();
        let range = text.line_start_offset(lines.start)
            ..text.line_end_offset(lines.end.saturating_sub(1).max(lines.start));
        self.extras.lsp._inlay_hints_task = cx.spawn_in(window, async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Ok(task) =
                cx.update(|window, cx| provider.inlay_hints(&text, range.clone(), window, cx))
            else {
                return;
            };
            let Ok(hints) = task.await else {
                return;
            };
            let inlays = cx
                .background_executor()
                .spawn({
                    let text = text.clone();
                    async move {
                        let style = InlayStyle::default();
                        hints_to_inlays(&text, hints, &style)
                    }
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                if state.document_revision != revision {
                    return;
                }
                let tint = state.editor_style.muted_foreground;
                let inlays = inlays
                    .into_iter()
                    .map(|mut inlay| {
                        inlay.style.background = Some(tint.opacity(0.1));
                        inlay
                    })
                    .collect();
                state.set_inlays(
                    INLAY_HINTS_KEY,
                    (!text_changed).then_some(range),
                    inlays,
                    cx,
                );
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use lsp_types::{InlayHintLabelPart, Position};

    use super::*;

    fn hint(line: u32, character: u32, label: &str, kind: Option<InlayHintKind>) -> InlayHint {
        InlayHint {
            position: Position::new(line, character),
            label: InlayHintLabel::String(label.into()),
            kind,
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        }
    }

    #[test]
    fn hints_become_inlays_by_kind_with_their_padding() {
        let text = Rope::from("let x = f(1);\nlet 日本 = 2;");
        let mut parts = hint(1, 6, "", Some(InlayHintKind::TYPE));
        parts.label = InlayHintLabel::LabelParts(vec![
            InlayHintLabelPart {
                value: ": ".into(),
                ..Default::default()
            },
            InlayHintLabelPart {
                value: "i32".into(),
                ..Default::default()
            },
        ]);
        let mut padded = hint(0, 10, "a:", Some(InlayHintKind::PARAMETER));
        padded.padding_right = Some(true);
        let inlays = hints_to_inlays(
            &text,
            vec![
                hint(0, 5, ": i32", Some(InlayHintKind::TYPE)),
                padded,
                parts,
                hint(9, 0, "off the text", None),
                hint(0, 1, " ", None),
            ],
            &InlayStyle::default(),
        );
        let got: Vec<_> = inlays
            .iter()
            .map(|i| (i.offset, i.text.to_string(), i.bias, i.style.margin_right))
            .collect();
        assert_eq!(
            got,
            vec![
                (5, ": i32".to_string(), InlayBias::Left, 0.),
                (10, "a:".to_string(), InlayBias::Right, 0.5),
                // Columns count characters: after the two CJK ones.
                (14 + 4 + 6, ": i32".to_string(), InlayBias::Left, 0.),
            ]
        );
    }

    #[test]
    fn the_lines_asked_for_reach_past_the_view() {
        assert_eq!(lines_to_ask(&(100..140), 1000), 60..180);
        assert_eq!(lines_to_ask(&(0..10), 1000), 0..30);
        assert_eq!(lines_to_ask(&(990..1000), 1000), 970..1000);
    }
}
