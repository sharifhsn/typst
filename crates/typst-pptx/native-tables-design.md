# PPTX Native Tables Design Checkpoint

Date: 2026-07-08

## Decision

Use the tag-based approach. The grid/table resolver already has the exact
logical structure before layout lowers it into ordinary frame items:
`CellGrid` stores non-gutter rows/columns, `Entry::Cell` versus
`Entry::Merged`, spans, resolved fill, resolved stroke, and header/footer row
ranges. The missing part for a post-layout PPTX consumer is physical cell
geometry. Emitting hidden cell-region tags during grid layout is cleaner than
reconstructing a grid from fill rectangles and rule-line geometry.

The tags should use the existing `FrameItem::Tag(Tag::Start/End)` mechanism,
not a new visible rendering primitive. They are internal, have
`TagFlags { introspectable: false, tagged: false }`, and are ignored by normal
introspection and non-PPTX exporters. The tag content carries the resolved cell
body plus row, column, rowspan, colspan, width, and height. Whether the cell
came from a semantic table or grid can be inferred from that body. The tag
placement supplies the cell origin in the laid-out slide frame.

## Mapping

During the PPTX slide walk:

- When a hidden table-cell-region tag starts, open an active capture with its
  origin, size, row/column, spans, fill/stroke, and source kind.
- Capture ordinary frame items until the matching region end tag. Text inside
  the capture is converted with the existing PPTX text-run/paragraph machinery;
  unsupported nested items can force a fallback for that cell/table.
- Group captured cells by table instance and contiguous row/column lattice.

During encoding:

- Emit one `SlideShape::TableBox` as `<p:graphicFrame><a:graphic><a:graphicData
  uri=".../table"><a:tbl>`.
- Emit `<a:tblGrid>` with one `<a:gridCol w="...">` per non-gutter column,
  derived from the captured physical cell regions.
- Emit `<a:tr h="...">` and `<a:tc>` for origin cells.
- `colspan > 1` maps to `<a:tc gridSpan="N">`.
- `rowspan > 1` maps to `<a:tc rowSpan="N">` for the origin and
  `<a:tc vMerge="1">` placeholders in continuation rows.
- Covered horizontal slots are represented with `<a:tc hMerge="1">`
  placeholders, matching DrawingML's table-merge model.
- Cell fill maps to `<a:tcPr><a:solidFill>...`.
- Basic per-cell borders map to `<a:lnL>`, `<a:lnR>`, `<a:lnT>`,
  and `<a:lnB>` in `<a:tcPr>`.

## Fallbacks And Scope

The first implementation should preserve existing shape-based rendering when a
table cannot be represented faithfully enough as an editable PowerPoint table.
Fallback cases include non-similarity transforms, clipped/skewed groups,
complex non-text cell content that cannot be lowered to text, and tables split
across multiple slide regions. Nested tables inside table cells are explicitly
out of scope for this round.

## Go / No-Go

Go. Grid layout already knows both logical cell structure and physical cell
dimensions at the point where each cell frame is pushed. Hidden region tags are
small, local to grid layout, and can be ignored by all existing non-PPTX
consumers. This avoids the fragile geometric reconstruction path.
