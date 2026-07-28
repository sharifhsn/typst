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
use typst_export_common::raster;
use typst_library::diag::{SourceResult, warning};
use typst_library::engine::Engine;
use typst_library::foundations::{Content, StyleChain};
use typst_library::introspection::{Locator, SplitLocator, Tag};
use typst_library::layout::{Abs, Frame, FrameItem, Size};
use typst_library::text::SmartQuoter;
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
    /// Set from the document's page geometry (page width minus horizontal
    /// margins) by [`crate::document::pandoc_document`]; only when the page has
    /// no finite width (`set page(width: auto)`) does it keep the [`Self::new`]
    /// fallback.
    pub(crate) raster_width: Abs,

    /// Smart-quote state, threaded through inline runs.
    pub(crate) quoter: SmartQuoter,
    /// The last character emitted into a text run, for smart quoting.
    pub(crate) last_char: Option<char>,

    /// Maps a bibliography entry's anchor id (the `ref-<hash>` / label form an
    /// in-text citation `Link` targets) to its citation key. Populated once, up
    /// front, from the document's `Works`. Used by the post-lowering
    /// normalization pass to recover the cite key behind a
    /// realized in-text citation and emit a structured `Cite` node that
    /// `pandoc --citeproc` can re-resolve. Empty when the document has no
    /// bibliography.
    pub(crate) cite_anchors: std::collections::HashMap<EcoString, EcoString>,

    /// Overrides [`Self::anchor_id`] for bibliography entries whose citation key
    /// is already a valid identifier: maps the entry's derived anchor id (the
    /// `ref-<hash>` form) to `ref-<citation key>`, the id `pandoc --citeproc`
    /// itself reads and writes. Consulted at that single choke point, so the
    /// entry's own `Attr` and every in-text cite `Link` pick the same id up
    /// automatically. Empty when the document has no bibliography, and missing
    /// an entry whose key fails the safety guard in
    /// [`Self::load_cite_anchors`].
    pub(crate) bib_ids: std::collections::HashMap<EcoString, EcoString>,
}

impl<'a, 'e> PandocCtx<'a, 'e> {
    /// Creates a fresh context.
    pub fn new(engine: &'a mut Engine<'e>, locator: &'a mut SplitLocator<'e>) -> Self {
        Self {
            engine,
            locator,
            deferred_tags: Vec::new(),
            // A sane finite default (~A4 text width), used only as a last resort
            // when the page has no finite width. `pandoc_document` overrides this
            // from the document's real page geometry (width minus horizontal
            // margins) before any conversion happens.
            raster_width: Abs::pt(450.0),
            quoter: SmartQuoter::new(),
            last_char: None,
            cite_anchors: std::collections::HashMap::new(),
            bib_ids: std::collections::HashMap::new(),
        }
    }

    /// Populates `Self::cite_anchors` from the document's `Works`, mapping each
    /// bibliography entry's anchor id to its citation key. The anchor ids are
    /// derived through [`Self::anchor_id`] from the entries' backlink locations —
    /// the exact ids in-text citation `Link`s target — so the post-walk pass can
    /// match a cite link to its key. A no-op when the document has no
    /// bibliography.
    pub fn load_cite_anchors(
        &mut self,
        entry_keys: &[(typst_library::introspection::Location, EcoString)],
    ) {
        // The anchor id each entry would get on its own (the `ref-<hash>` form,
        // since a synthesized entry body carries no label). Computed before any
        // override is installed, because that is the key `bib_ids` is looked up
        // by.
        let raw: Vec<(EcoString, EcoString)> = entry_keys
            .iter()
            .map(|(loc, key)| (self.anchor_id(*loc), key.clone()))
            .collect();

        // Prefer pandoc's own reference-id convention, `ref-<citation key>`:
        // that is exactly what citeproc reads and writes, so the emitted ids
        // interoperate with `--citeproc` and with a bibliography the user
        // supplies separately — an opaque hash interoperates with nothing.
        //
        // Only when the key needs no sanitizing, though. A key containing
        // anything outside `[A-Za-z0-9_:-]` (`ä`, `.`, `+`, a space — all legal
        // in BibTeX/hayagriva) would have to be rewritten to be a valid
        // LaTeX/HTML id, and a rewritten key is no longer the key citeproc
        // looks for; worse, two distinct keys can sanitize to the *same* id
        // (`a.b` and `a-b`), which would silently merge two entries' anchors.
        // Those keys keep the collision-free hash instead.
        for (anchor, key) in &raw {
            if !key.is_empty() && key.chars().all(is_id_char) {
                self.bib_ids.insert(anchor.clone(), ecow::eco_format!("ref-{key}"));
            }
        }

        // Finally record anchor → key under the *effective* id, so the
        // post-walk pass can still recover the key behind a cite `Link`.
        for (anchor, key) in raw {
            let effective = self.bib_ids.get(&anchor).cloned().unwrap_or(anchor);
            self.cite_anchors.insert(effective, key);
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
        use typst_library::foundations::{Target, TargetElem};
        use typst_library::layout::{Axes, Region};

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

        // Use the shared exporter raster path, but keep Pandoc's historical
        // full-frame semantics rather than ink-cropping: relative whitespace
        // is part of an inline image's layout in downstream writers.
        let Some(rendered) = raster::render_full_frame_to_png(frame, 2.0) else {
            return Ok(None);
        };

        Ok(Some((self.add_image(&rendered.png, "png"), rendered.size)))
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

    /// Allocates a stable, deterministic Pandoc `Attr` id for a `Location`. The
    /// id must match what the heading/figure/equation mappers emit and what an
    /// internal `#id`-`Link` targets — the shared id namespace is load-bearing
    /// (every site goes through this one function, so consistency is automatic).
    ///
    /// Two cases:
    ///
    /// * **The element carries an explicit Typst label** (`= Intro <intro>`):
    ///   derive the id from the label name, sanitized to a valid Pandoc/LaTeX id
    ///   (see `sanitize_label_id`). Labels are unique per document, so the
    ///   sanitized ids are collision-free among labeled elements, and the id is
    ///   stable across edits that don't rename the label — far more useful than
    ///   an opaque hash for downstream tooling and human-readable output.
    ///
    /// * **No label** (the common case for headings/figures without `<…>`, and
    ///   for synthesized bibliography entry bodies): fall back to a deterministic
    ///   `ref-<hash128(loc)>`. The hash is over the `Location` only — no map
    ///   iteration order, no addresses — so it is byte-stable across runs.
    ///
    /// ## Namespacing / collision handling
    ///
    /// The whole `ref-` prefix is the exporter's: the hash fallback writes
    /// `ref-<hash>`, and a bibliography entry writes `ref-<citation key>`. To keep
    /// a user label from ever colliding with that namespace — or producing an id
    /// that LaTeX/HTML reject — `sanitize_label_id` prefixes a label-derived id
    /// with `L-` whenever it would be empty, start with a digit, or fall inside
    /// the reserved prefix. In the common case (`<intro>`) the id is just
    /// `intro`, verbatim.
    ///
    /// A bibliography entry additionally goes through the `Self::bib_ids`
    /// override, which swaps the hash for pandoc's `ref-<citation key>`
    /// convention wherever the key allows it.
    pub fn anchor_id(&self, loc: typst_library::introspection::Location) -> EcoString {
        use typst_library::foundations::Selector;
        use typst_library::introspection::Introspector;

        // Look the element up by location and read its explicit label, if any.
        // This is a pure read of the (shared) introspector — no sink side effect,
        // so it does not perturb convergence detection. During an early
        // introspection-stabilization iteration the element may be absent; that
        // simply yields the hash fallback, and the final stabilized pass replaces
        // it with the label-derived id (both the anchor and the targeting Link
        // resolve through this function, so they stay in lock-step).
        let introspector = self
            .engine
            .introspector
            .access("reading an element's label to derive its anchor id is a pure query");
        let label = introspector
            .query_first(&Selector::Location(loc))
            .and_then(|content| content.label());

        let id = anchor_id_from(label, loc);
        self.bib_ids.get(&id).cloned().unwrap_or(id)
    }
}

/// The anchor id an introspection [`Tag`] stands for, or `None` for a tag that
/// marks no anchorable position (an `End` tag, or a `Start` without a location).
///
/// This is the *positional* counterpart to [`PandocCtx::anchor_id`]: the tag is
/// the only thing left at the place of an element whose own lowering carries no
/// [`Attr`] — a `show heading:` rule replaces the `HeadingElem` with arbitrary
/// content, and a label may sit on a bare text run. The id comes from the tag's
/// own content rather than an introspector query, which is both cheaper (this
/// runs for every tag in the document) and identical: the introspector is built
/// from exactly these tags, so querying the location returns this very content.
///
/// Note this deliberately does *not* apply the [`PandocCtx::bib_ids`] override.
/// A bibliography entry is lowered by the citation mapper, which anchors it
/// through `anchor_id`; the tag position would only ever duplicate that id, and
/// [`crate::normalize`] drops a synthesized anchor whose id a real node already
/// carries.
pub(crate) fn tag_anchor(tag: &Tag) -> Option<EcoString> {
    let Tag::Start(content, _) = tag else { return None };
    let loc = content.location()?;
    Some(anchor_id_from(content.label(), loc))
}

/// Derives an anchor id from an element's label (if any) and its location — the
/// shared rule behind [`PandocCtx::anchor_id`] and [`tag_anchor`], which must
/// agree byte for byte or a link would target an id that nothing carries.
fn anchor_id_from(
    label: Option<typst_library::foundations::Label>,
    loc: typst_library::introspection::Location,
) -> EcoString {
    match label {
        Some(label) => sanitize_label_id(&label.resolve()),
        None => ecow::eco_format!("ref-{:016x}", typst_utils::hash128(&loc)),
    }
}

/// Whether `ch` may appear in a Pandoc/LaTeX/HTML identifier unchanged.
fn is_id_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | ':' | '-')
}

/// Sanitizes a Typst label name into a valid Pandoc/LaTeX/HTML id.
///
/// Rules (documented on [`PandocCtx::anchor_id`]):
/// * Allowed characters are `[A-Za-z0-9_:-]`; every other character (including
///   `.`, which Typst labels permit but LaTeX `\label` mangles) is replaced with
///   `-`. This is byte-deterministic — a pure function of the label string.
/// * The id is then prefixed with `L-` if it would otherwise be empty, start
///   with a digit (invalid as a LaTeX/CSS id), or start with the reserved
///   `ref-<hex>` namespace owned by the hash fallback and bibliography anchors.
///   This keeps label ids collision-free against that namespace while leaving the
///   common case (`intro` → `intro`) untouched.
fn sanitize_label_id(name: &str) -> EcoString {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if is_id_char(ch) {
            out.push(ch);
        } else {
            out.push('-');
        }
    }

    // Guard the reserved/invalid leading forms. `starts_with_reserved_ref`
    // matches the exact `ref-<16 lowercase hex>` shape the hash fallback emits,
    // so an innocuous label like `reference` is *not* caught.
    let needs_prefix = out.is_empty()
        || out.starts_with(|c: char| c.is_ascii_digit())
        || starts_with_reserved_ref(&out);
    if needs_prefix {
        let mut prefixed = String::with_capacity(out.len() + 2);
        prefixed.push_str("L-");
        prefixed.push_str(&out);
        out = prefixed;
    }

    out.into()
}

/// Whether `s` begins with the reserved `ref-` prefix.
///
/// The whole prefix is the exporter's, not just the hash form: `ref-<hash>` is
/// the unlabelled fallback and `ref-<citation key>` is what a bibliography entry
/// gets (see [`PandocCtx::load_cite_anchors`]). Reserving only the hash shape
/// would leave a document that both writes `<ref-netwok>` and cites `netwok`
/// with two nodes claiming one id — so a label is pushed out of the namespace
/// whatever follows the prefix. Labels that merely *start* like it (`reference`,
/// `refs`) are untouched: they have no `-`.
fn starts_with_reserved_ref(s: &str) -> bool {
    s.starts_with("ref-")
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

#[cfg(test)]
mod tests {
    use super::{sanitize_label_id, starts_with_reserved_ref};

    /// A plain label (`= Intro <intro>`) becomes a readable, verbatim id — this
    /// is the value `anchor_id` stamps onto the heading's `Attr` *and* the value
    /// an `@intro` cross-reference `Link` targets as `#intro` (both go through
    /// `anchor_id`, so they stay identical).
    #[test]
    fn plain_label_is_verbatim() {
        assert_eq!(sanitize_label_id("intro"), "intro");
        assert_eq!(sanitize_label_id("my-figure"), "my-figure");
        assert_eq!(sanitize_label_id("eq:euler"), "eq:euler");
        assert_eq!(sanitize_label_id("sec_1"), "sec_1");
        // `reference` must NOT be mistaken for the reserved `ref-<hex>` form.
        assert_eq!(sanitize_label_id("reference"), "reference");
    }

    /// Disallowed characters (Typst permits `.` in labels; LaTeX/CSS do not)
    /// become `-`, deterministically.
    #[test]
    fn special_chars_become_dash() {
        assert_eq!(sanitize_label_id("fig.1"), "fig-1");
        assert_eq!(sanitize_label_id("a b c"), "a-b-c");
        // 'π' and '/' each map to '-'; the result starts with '-' (not a digit,
        // not the reserved `ref-` form), so no `L-` prefix is added.
        assert_eq!(sanitize_label_id("π/2"), "--2");
    }

    /// An id may not start with a digit (invalid LaTeX/CSS id); guard with `L-`.
    #[test]
    fn leading_digit_is_guarded() {
        assert_eq!(sanitize_label_id("1st"), "L-1st");
        assert_eq!(sanitize_label_id("42"), "L-42");
    }

    /// An empty (fully-stripped) label still yields a valid id.
    #[test]
    fn empty_is_guarded() {
        assert_eq!(sanitize_label_id(""), "L-");
    }

    /// A user label inside the reserved `ref-` namespace is pushed out of it
    /// with `L-`, so it can never collide with the hash fallback or with a
    /// bibliography entry's `ref-<citation key>` anchor.
    #[test]
    fn reserved_ref_form_is_guarded() {
        assert_eq!(sanitize_label_id("ref-00000000deadbeef"), "L-ref-00000000deadbeef");
        // A label spelling a citation key's anchor is guarded just the same.
        assert_eq!(sanitize_label_id("ref-netwok"), "L-ref-netwok");
        assert!(starts_with_reserved_ref("ref-0123456789abcdef"));
        // Names that merely start like the prefix keep their own id.
        assert!(!starts_with_reserved_ref("refs")); // the bib container id
        assert!(!starts_with_reserved_ref("reference"));
        assert_eq!(sanitize_label_id("reference"), "reference");
    }
}
