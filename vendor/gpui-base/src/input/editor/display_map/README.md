# Display Mapping System

Layered coordinate conversion and code folding for Editor/Input.

## Architecture

```
Buffer (Rope)              Logical text
    ↓
InlayMap                   Text shown between the buffer's characters (ideae patch)
    ↓
WrapMap                    Soft-wrapping (buffer_line ↔ wrap_row)
    ↓
FoldMap                    Fold projection (wrap_row ↔ display_row)
    ↓
DisplayMap                 Public facade (BufferPos ↔ DisplayPos)
```

## Coordinate Systems

| Type | Fields | Scope | Description |
|------|--------|-------|-------------|
| `BufferPos` | `{ line, col }` | public | Logical line/column in Rope |
| `WrapPos` | `{ row, col }` | internal | Visual row after soft-wrapping |
| `DisplayPos` | `{ row, col }` | public | Final visible row after folding |

## Modules

### `DisplayMap` — Public facade

- `buffer_pos_to_display_pos()` / `display_pos_to_buffer_pos()`
- `set_fold_candidates()`, `toggle_fold()`, `is_folded_at()`, `clear_folds()`
- `on_text_changed()`, `on_layout_changed()`, `set_font()`
- `adjust_folds_for_edit()` — incremental fold/candidate line-delta adjustment
- `update_fold_candidates_for_edit()` — region-scoped candidate extraction after edits

### `WrapMap` — Soft-wrapping layer

Built on `TextWrapper`. Provides buffer ↔ wrap coordinate mapping with prefix sum cache for O(1) line lookups.

### `FoldMap` — Fold projection layer

Stores the projection as sorted, merged runs of hidden wrap rows (`HiddenRows`), each carrying a running `hidden_before` count of the rows the earlier runs hide. Rebuilding costs O(folds), not O(document rows), and every mapping in either direction is a `partition_point` binary search over the runs. When no folds are active, uses identity mapping (wrap_row == display_row) without any runs.

### `InlayMap` — Inlays (ideae patch)

Text that is drawn but is not in the buffer: a type after a name, a
parameter's name before an argument, who last changed the line at its end.
An `Inlay` is a buffer offset, a text, an `InlayStyle` (color, background,
italic, margins in space widths), an optional id, and an `InlayBias`:

- `Left` hangs on the text before it: the caret at its offset is drawn
  before it, text typed there goes before it (it moves on), a soft wrap at
  its offset leaves it at the end of the upper row.
- `Right` hangs on the text after it: the caret at its offset is drawn after
  it, typed text goes after it, a wrap takes it to the lower row.

An inlay has no bytes, so there is no inlay coordinate space: every
position stays a buffer offset, and inlays only add width at offsets. The
caret cannot land inside one, Left and Right step over it as over any
character boundary, copying never sees it. What maps offsets to x does:

- `TextWrapper` measures a row with the inlays on it (`on_row`), so they
  take room when lines wrap. Inlays at a line's very end are not measured:
  they never wrap a line on their own and may run past the right edge.
- `InputLine` (the shaped row) is cut into fragments at the inlays; an
  inlay is a fragment with an empty range. `x_for_index` puts the caret by
  bias, `closest_index_for_x` maps a point on an inlay to its offset,
  `inlay_at_x` is the hit test. Inlays at the line's end are not in the
  row's `content_width`, so they do not widen what scrolls horizontally.
- Edits move inlays (`InlayMap::edit`): one inside replaced text goes with
  it, one at the start of an insertion goes after it when `Left`.

Inlays are set by key ("lsp-hints", "blame") for a range or the whole
document (`InputBaseState::set_inlays`); only the lines whose inlays
changed are wrapped again, and only the rows in view are shaped. A click on
an inlay with an id is reported as `InputEvent::InlayClick`. Folding works
on wrap rows, so folded lines hide their inlays with them.

### `FoldRange` / `folding` — Fold extraction

- `extract_fold_ranges(tree)` — full tree traversal (initial load only)
- `extract_fold_ranges_in_range(tree, byte_range)` — region-scoped traversal for edits, skips subtrees outside range
