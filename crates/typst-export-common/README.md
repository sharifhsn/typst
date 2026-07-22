# typst-export-common

Internal, format-neutral infrastructure shared by Typst exporters.

The crate owns raster-fallback rendering: measuring the ink drawn by a laid-out
frame, rendering it safely, optionally cropping transparent margins, and
preserving the resulting logical offset and size. It also extracts stable,
owned physical-region facts from converged paged frames. The first geometry
consumer is DOCX table/grid lowering, which reads layout's hidden
`GridCellRegion` tags instead of re-solving flexible tracks from page width.
PPTX reads the same tags, and takes their `ResolvedCellStyle` as well: the
resolver's final fill, stroke, alignment, and inset, so that neither exporter
has to re-resolve a cell under a style chain it does not own.
DOCX, PPTX, and Pandoc can share these mechanics even though only two emit
OOXML, so they do not belong in `typst-ooxml-core`.

Format-specific capability decisions stay in the exporter crates. This crate
only implements target-independent mechanics.
