//! The mutable conversion context [`PandocCtx`] and the inline/block flow.
//!
//! Mirrors `typst_docx::DocxCtx`, but only the *target-independent* services:
//! the engine/locator borrow helpers, the `rasterize` pipeline (steps 1–6 of
//! the DOCX version, with step 7 replaced by a self-contained data-URI image
//! instead of an OPC media part), the `deferred_tags` convergence discipline,
//! and the smart-quote state. There is no numbering registry, no OPC media/rels,
//! no bookmarks, no sections — Pandoc has its own structural nodes and never
//! re-numbers.

use ecow::EcoString;
use typst_library::diag::{SourceResult, warning};
use typst_library::engine::Engine;
use typst_library::foundations::{Content, StyleChain};
use typst_library::introspection::{Locator, SplitLocator, Tag};
use typst_library::layout::{Abs, Frame, FrameItem, Size};
use typst_library::text::{SmartQuoter};
use typst_syntax::Span;

use crate::ast::{Attr, Inline};

/// The mutable state accumulated during the post-realize walk.
pub struct PandocCtx<'a, 'e> {
    pub(crate) engine: &'a mut Engine<'e>,
    pub(crate) locator: &'a mut SplitLocator<'e>,

    /// Introspection tags harvested from rasterized content (see
    /// [`Self::rasterize`]) and from run-only contexts, so labels/refs inside an
    /// element we rendered to an image (or inside a table cell / footnote body)
    /// remain present in the introspector. Load-bearing for convergence.
    pub(crate) deferred_tags: Vec<Tag>,

    /// The finite width to give content that we rasterize. Width-relative
    /// content (`layout(size => ..)`, `width: 100%`, gradients sized to the
    /// container) must lay out against a real page width: laying it out under an
    /// *infinite* width makes such a closure produce pathologically wide output.
    /// Set from the page geometry in [`crate::document::pandoc_document`].
    pub(crate) raster_width: Abs,

    /// Smart-quote state, threaded through inline runs.
    pub(crate) quoter: SmartQuoter,
    /// The last character emitted into a text run, for smart quoting.
    pub(crate) last_char: Option<char>,
}

impl<'a, 'e> PandocCtx<'a, 'e> {
    /// Creates a fresh context.
    pub fn new(engine: &'a mut Engine<'e>, locator: &'a mut SplitLocator<'e>) -> Self {
        Self {
            engine,
            locator,
            deferred_tags: Vec::new(),
            // A sane finite default (~A4 text width); the document driver may
            // override from real page geometry before any conversion happens.
            raster_width: Abs::pt(450.0),
            quoter: SmartQuoter::new(),
            last_char: None,
        }
    }

    // -- Borrowing helpers --------------------------------------------------

    /// Borrows the engine for sub-realization / decode / counter display.
    pub fn engine(&mut self) -> &mut Engine<'e> {
        self.engine
    }

    /// Splits a fresh locator for a sub-fragment.
    pub fn next_locator(&mut self, span: Span) -> Locator<'e> {
        self.locator.next(&span)
    }

    /// Emits a non-fatal "X was ignored during Pandoc export" warning.
    pub fn warn_ignored(&mut self, what: &str, span: Span) {
        self.engine
            .sink
            .warn(warning!(span, "{what} was ignored during Pandoc export"));
    }

    // -- Rasterize fallback -------------------------------------------------

    /// Lays out arbitrary content and rasterizes it to a PNG, returning a
    /// self-contained `data:` URI for an `Image` node plus the content's size,
    /// or `None` if the content lays out to nothing. This is the universal
    /// fallback for content that has no idiomatic Pandoc representation (drawn
    /// shapes, SVG/PDF images, externally-rendered figures, cetz canvases, …).
    ///
    /// Mirrors `DocxCtx::rasterize` exactly through the frame-tag harvest and
    /// render; only the final embedding differs — DOCX writes an OPC media part
    /// and returns an rId, whereas Pandoc has no package, so we inline the PNG
    /// bytes as a base64 `data:` URI (self-contained, recoverable by every
    /// pandoc writer).
    pub fn rasterize(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<Option<(EcoString, Size)>> {
        use typst_library::foundations::{Smart, Target, TargetElem};
        use typst_library::layout::{Axes, Region, Sides};

        // Lay out under the paged target: layout rules (shapes, images, …) are
        // only registered for `Target::Paged`, so the content would otherwise be
        // dropped during its own layout.
        let target = TargetElem::target.set(Target::Paged).wrap();
        let styles = styles.chain(&target);

        // Lay the content out against the page's content width (height stays
        // unbounded). A *finite* width is essential (see field doc); the
        // non-expanding region keeps fixed-size content at its natural size.
        let region =
            Region::new(Size::new(self.raster_width, Abs::inf()), Axes::splat(false));
        let loc = self.locator.next(&span);

        // Lay the content out through a sub-engine with a THROWAWAY sink, so any
        // delayed errors the re-layout produces (a margin-note needing page
        // properties, a cetz canvas whose size hasn't stabilized, …) are
        // discarded rather than promoted to fatal at the end of the
        // introspection loop. The shared introspector (reads) and the tag
        // harvest below stay intact, so labels/refs/bibliography convergence is
        // unaffected — only this re-layout's own error reporting is isolated.
        use comemo::Track;
        let layout_frame = self.engine.library.routines.layout_frame;
        let mut throwaway = typst_library::engine::Sink::new();
        let frame = {
            let mut sub = typst_library::engine::Engine {
                world: self.engine.world,
                library: self.engine.library,
                introspector: typst_utils::Protected::from_raw(
                    self.engine.introspector.into_raw(),
                ),
                traced: self.engine.traced,
                sink: throwaway.track_mut(),
                route: typst_library::engine::Route::extend(self.engine.route.track()),
            };
            match layout_frame(&mut sub, content, loc, styles, region) {
                Ok(frame) => frame,
                Err(_) => return Ok(None),
            }
        };

        // Harvest introspection tags from the laid-out frame BEFORE the size
        // check below: content can lay out to a degenerate (zero) size precisely
        // *because* an introspecting element inside it has not yet stabilized; if
        // we dropped such a frame without harvesting, its tags would never reach
        // the introspector and the box would stay zero forever (a convergence
        // deadlock). Harvesting here lets the next iteration render it for real.
        collect_frame_tags(&frame, &mut self.deferred_tags);

        let size = frame.size();
        if !size.x.to_pt().is_finite()
            || !size.y.to_pt().is_finite()
            || size.x <= Abs::zero()
            || size.y <= Abs::zero()
        {
            return Ok(None);
        }

        // Render to a pixmap at 2× for crispness, then PNG-encode.
        let page = typst_layout::Page {
            frame,
            bleed: Sides::splat(Abs::zero()),
            fill: Smart::Custom(None),
            numbering: None,
            supplement: Content::empty(),
            number: 1,
        };
        let options = typst_render::RenderOptions {
            pixel_per_pt: 2.0.into(),
            ..Default::default()
        };
        // The rasterizer can panic on pathological sub-frames (a gradient/tiling
        // that resolves to a zero-dimension pixmap: `tiny-skia` asserts "Canvas
        // length must be != 0"). Such a panic must not abort the whole export —
        // this is a best-effort fallback. Catch it and drop just this one image;
        // the introspection tags were already harvested above.
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            typst_render::render(&page, &options).encode_png()
        }));
        let Ok(Ok(png)) = rendered else { return Ok(None) };

        Ok(Some((self.add_image(&png, "png"), size)))
    }

    /// Embeds image bytes as a self-contained `data:` URI (base64), suitable as
    /// the `url` of a Pandoc `Image` node. Pandoc has no package/media bag at
    /// this layer, so inlining keeps the single-JSON-blob output self-contained;
    /// downstream pandoc writers that need a file (LaTeX, EPUB) extract the
    /// data-URI into a media file themselves.
    pub fn add_image(&mut self, bytes: &[u8], ext: &str) -> EcoString {
        let mime = match ext.to_ascii_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            _ => "application/octet-stream",
        };
        let b64 = base64_encode(bytes);
        ecow::eco_format!("data:{mime};base64,{b64}")
    }

    // -- Property resolvers -------------------------------------------------

    /// Allocates (or reuses) a stable Pandoc `Attr` id for a `Location`. The id
    /// must match what the heading/figure mapper emits and what an internal
    /// `#id`-`Link` targets — the shared id namespace is load-bearing. Derived
    /// deterministically from the location's hash so the same target produces
    /// the same id regardless of visit order.
    pub fn anchor_id(&self, loc: typst_library::introspection::Location) -> EcoString {
        ecow::eco_format!("ref-{:016x}", typst_utils::hash128(&loc))
    }
}

/// Recursively collects introspection tags from a laid-out frame.
fn collect_frame_tags(frame: &Frame, out: &mut Vec<Tag>) {
    for (_, item) in frame.items() {
        match item {
            FrameItem::Group(group) => collect_frame_tags(&group.frame, out),
            FrameItem::Tag(tag) => out.push(tag.clone()),
            _ => {}
        }
    }
}

/// Minimal standard-alphabet base64 encoder (no padding omitted), so we don't
/// add a dependency just for data-URI image embedding.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Builds an empty-or-text [`Inline`] alt sequence (helper for image mappers).
#[allow(dead_code)]
pub(crate) fn alt_inlines(alt: Option<&str>) -> Vec<Inline> {
    match alt {
        Some(s) if !s.is_empty() => vec![Inline::Str(s.to_string())],
        _ => Vec::new(),
    }
}

/// An empty [`Attr`] convenience (re-export of the AST helper).
#[allow(dead_code)]
pub(crate) fn empty_attr() -> Attr {
    crate::ast::empty_attr()
}
