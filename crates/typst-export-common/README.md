# typst-export-common

Internal, format-neutral infrastructure shared by Typst exporters.

The crate currently owns raster-fallback rendering: measuring the ink drawn by
a laid-out frame, rendering it safely, optionally cropping transparent margins,
and preserving the resulting logical offset and size. DOCX, PPTX, and Pandoc
all need this operation even though only two of them emit OOXML, so it does not
belong in `typst-ooxml-core`.

Format-specific capability decisions stay in the exporter crates. This crate
only implements target-independent mechanics.
