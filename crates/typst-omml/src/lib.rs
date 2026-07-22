//! Lowers Typst's resolved math IR ([`MathItem`]) to OMML — the native,
//! *editable* Office math format (`m:` namespace) that both Word and PowerPoint
//! consume.
//!
//! This is the Typst→OMML *policy* layer: it decides which OMML construct a
//! given Typst construct becomes. The spelling mechanics it builds on (the
//! [`Omml`] fragment builder, combining-accent and n-ary character tables) live
//! in `typst_ooxml_core::omml_build`, which holds no Typst knowledge at all.
//!
//! Reusing `crates/typst-html/src/mathml.rs` verbatim is impractical here: that
//! code emits `HtmlElem`/`Content` trees (MathML), and the Office exporters must
//! not depend on `typst-html`. Instead we walk the `MathItem` IR directly,
//! mirroring `mathml.rs::handle_realized`'s `match &comp.kind { … }` arms but
//! emitting OMML elements. The `MathItem` IR has already done all the
//! structural grouping (fractions, radicals, scripts, fences, tables, …), so
//! each [`MathKind`] variant maps to exactly one OMML construct and the
//! recursion composes — e.g. `binom(n,k)` arrives as a `Fenced` wrapping a
//! no-bar `Fraction`, and `mat(delim: "[")` as a `Fenced` wrapping a `Table`,
//! both of which fall out of the per-variant handlers for free.
//!
//! [`lower_equation`] is the entry point. It returns a bare `<m:oMath>`
//! fragment, which an inline equation can drop straight into a paragraph
//! alongside runs; a block equation additionally wraps it with
//! [`omath_para`]. Where that fragment is *placed*, and how a numbered
//! equation's number is rendered next to it, is the caller's business.
//!
//! OOXML correctness invariants honored here (the Word-"repair" triggers, see
//! `research/omml_math.md` §15):
//! - Every property value is the namespaced `m:val`, never bare `val`.
//! - Child order is the schema sequence; the `*Pr` block is always first.
//! - `m:rad` emits the degree *before* the radicand (opposite of MathML).
//! - `m:nary` always sets `m:chr` (else Word defaults it to `∫`) and emits
//!   `m:sub`/`m:sup`/`m:e` even when empty (hiding unused limits).
//! - `m:d` sets explicit beg/end chars for non-paren fences.
//! - Accent characters are emitted as their *combining* codepoints.
//!
//! See `research/omml_math.md` and `map/math_repr.md`.

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::introspection::Tag;
use typst_library::math::ir::{
    AccentItem, ExternalItem, FencedItem, FractionItem, GlyphItem, MathComponent,
    MathItem, MathKind, MultilineItem, NumberItem, Position, RadicalItem, ScriptsItem,
    SkewedFractionItem, TableItem, TextItem,
};
use typst_ooxml_core::color::composite_rgb_on_white;
use typst_ooxml_core::omml_build::{
    Omml, is_integral_char, is_nary_operator, to_combining,
};

use unicode_math_class::MathClass;

// ===========================================================================
// Caller-supplied services.
// ===========================================================================

/// The engine-side services the OMML lowering cannot perform for itself.
///
/// Every method has a no-op default, so a caller that has no engine at hand can
/// pass [`NoHooks`] and still get correct OMML for everything that does not
/// need one. Each default documents exactly what it costs — read it before
/// leaving a method unimplemented; there is no other signal at the call site.
pub trait MathHooks {
    /// Hands the caller an introspection tag found *inside* the equation body.
    ///
    /// OMML has nowhere to put a tag, but it must still reach the introspector
    /// so that labels and refs inside the equation resolve — e.g. a per-line
    /// label `#<eqa>`. The lowering cannot do that itself, because where a tag
    /// may be re-attached depends on the enclosing document model (`typst-docx`
    /// defers it onto the surrounding paragraph).
    ///
    /// The default drops the tag. The cost is precisely that: a `#ref` to a
    /// label declared inside an equation will not resolve. The emitted math is
    /// unaffected.
    fn defer_tag(&mut self, _tag: &Tag) {}
}

/// A [`MathHooks`] implementation that provides nothing — every hook takes its
/// documented default. Correct for a caller with no engine-side services; see
/// each [`MathHooks`] method for what is given up.
pub struct NoHooks;

impl MathHooks for NoHooks {}

// ===========================================================================
// Entry points.
// ===========================================================================

/// The outcome of lowering one equation.
///
/// Capability planning is atomic at the logical equation boundary: a native
/// OMML subtree may not simply omit one unsupported descendant, because that
/// changes the equation's meaning while leaving a plausible-looking result. So
/// an equation is either lowered whole or refused whole, and the refusal is a
/// value the caller must handle rather than a silently dropped child.
pub enum Lowered {
    /// A complete `<m:oMath>…</m:oMath>` fragment.
    Native(String),
    /// Nothing was emitted: the equation contains a construct with no faithful
    /// OMML form. The caller must represent the *whole* equation some other way
    /// — `typst-docx` rasterizes it, falling back to its alternate text.
    Unsupported(UnsupportedMath),
}

/// Lowers a resolved equation body to a bare `<m:oMath>` fragment.
///
/// Runs the whole-equation capability preflight first; see [`Lowered`].
pub fn lower_equation(
    item: &MathItem,
    hooks: &mut dyn MathHooks,
) -> SourceResult<Lowered> {
    if let Some(unsupported) = first_unsupported_math(item) {
        return Ok(Lowered::Unsupported(unsupported));
    }

    let mut emitter = Emitter::new(hooks);
    emitter.buf.open("m:oMath").children();
    emitter.emit_row(item)?;
    emitter.buf.close();
    Ok(Lowered::Native(emitter.buf.into_string()))
}

/// Wraps a lowered `<m:oMath>` fragment in a centered `<m:oMathPara>`, the form
/// a *block* equation takes. The result must be a direct child of the
/// paragraph, never nested inside a run.
pub fn omath_para(omath: &str) -> String {
    let mut para = Omml::new();
    para.open("m:oMathPara").children();
    para.open("m:oMathParaPr").children();
    para.open("m:jc").attr("m:val", "center").empty();
    para.close(); // m:oMathParaPr
    para.raw(omath);
    para.close(); // m:oMathPara
    para.into_string()
}

// ===========================================================================
// Capability preflight.
// ===========================================================================

/// One unsupported descendant found by the whole-equation capability preflight.
#[derive(Debug, Copy, Clone)]
pub struct UnsupportedMath {
    /// What kind of construct was refused.
    pub kind: UnsupportedMathKind,
    /// Where it came from, for the caller's warning.
    pub span: typst_syntax::Span,
}

/// The kinds of math construct that native OMML cannot carry.
#[derive(Debug, Copy, Clone)]
pub enum UnsupportedMathKind {
    /// An inline `box(..)` inside math: arbitrary laid-out content.
    Box,
    /// External (package-built) content that is not a plain arrow character.
    External,
}

impl UnsupportedMathKind {
    /// A human-readable name for a diagnostic.
    pub fn label(self) -> &'static str {
        match self {
            Self::Box => "inline box",
            Self::External => "external content",
        }
    }
}

/// Finds the first descendant that cannot be represented in native OMML.
fn first_unsupported_math(item: &MathItem) -> Option<UnsupportedMath> {
    let MathItem::Component(comp) = item else { return None };
    let span = comp.props.span;
    match &comp.kind {
        MathKind::Box(_) => {
            Some(UnsupportedMath { kind: UnsupportedMathKind::Box, span })
        }
        MathKind::External(item) if external_arrow_char(item).is_none() => {
            Some(UnsupportedMath { kind: UnsupportedMathKind::External, span })
        }
        MathKind::External(_) => None,
        MathKind::Group(group) => group.items.iter().find_map(first_unsupported_math),
        MathKind::Multiline(multi) => multi
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .find_map(first_unsupported_math),
        MathKind::Radical(rad) => {
            [Some(&rad.radicand), rad.index.as_ref(), Some(&rad.sqrt)]
                .into_iter()
                .flatten()
                .find_map(first_unsupported_math)
        }
        MathKind::Fenced(fenced) => {
            [fenced.open.as_ref(), fenced.close.as_ref(), Some(&*fenced.body)]
                .into_iter()
                .flatten()
                .find_map(first_unsupported_math)
        }
        MathKind::Fraction(frac) => [&frac.numerator, &frac.denominator]
            .into_iter()
            .find_map(first_unsupported_math),
        MathKind::SkewedFraction(frac) => {
            [&frac.numerator, &frac.denominator, &frac.slash]
                .into_iter()
                .find_map(first_unsupported_math)
        }
        MathKind::Table(table) => table
            .cells
            .iter()
            .flatten()
            .flat_map(|cell| cell.iter())
            .find_map(first_unsupported_math),
        MathKind::Scripts(scripts) => [
            Some(&scripts.base),
            scripts.top.as_ref(),
            scripts.bottom.as_ref(),
            scripts.top_left.as_ref(),
            scripts.bottom_left.as_ref(),
            scripts.top_right.as_ref(),
            scripts.bottom_right.as_ref(),
        ]
        .into_iter()
        .flatten()
        .find_map(first_unsupported_math),
        MathKind::Accent(accent) => [&accent.base, &accent.accent]
            .into_iter()
            .find_map(first_unsupported_math),
        MathKind::Cancel(cancel) => first_unsupported_math(&cancel.base),
        MathKind::Line(line) => first_unsupported_math(&line.base),
        MathKind::Mathml(item) => item.body.as_ref().and_then(first_unsupported_math),
        MathKind::Glyph(_)
        | MathKind::Number(_)
        | MathKind::Text(_)
        | MathKind::Primes(_) => None,
    }
}

fn math_item_external_arrow_char(item: &MathItem) -> Option<char> {
    let MathItem::Component(comp) = item else { return None };
    let MathKind::External(item) = &comp.kind else { return None };
    external_arrow_char(item)
}

fn external_arrow_char(item: &ExternalItem) -> Option<char> {
    let text = item.content.plain_text();
    let mut chars = text.chars();
    let chr = chars.next()?;
    (chars.next().is_none() && is_arrow_char(chr)).then_some(chr)
}

fn is_arrow_char(chr: char) -> bool {
    matches!(chr as u32, 0x2190..=0x21FF | 0x27F0..=0x27FF | 0x2900..=0x297F)
}

// ===========================================================================
// IR → OMML emitter.
// ===========================================================================

/// Walks the [`MathItem`] IR, emitting OMML into a fresh [`Omml`] buffer.
///
/// `hooks` is borrowed only for the rare items that need a caller-side service;
/// the structural transform itself is pure.
struct Emitter<'h> {
    hooks: &'h mut dyn MathHooks,
    buf: Omml,
    /// The current run colour (a non-default solid `text(fill:)` on the enclosing
    /// math component), applied to emitted runs via a `w:rPr`. `None` = default.
    color: Option<[u8; 3]>,
}

impl<'h> Emitter<'h> {
    fn new(hooks: &'h mut dyn MathHooks) -> Self {
        Self { hooks, buf: Omml::new(), color: None }
    }

    /// Emits a sequence of items (the children of an `m:e`/`m:num`/…) by
    /// flattening groups and dispatching each leaf.
    fn emit_row(&mut self, item: &MathItem) -> SourceResult<()> {
        self.emit_items(item.as_slice())
    }

    /// Emits a run of items, but when an n-ary operator (∑ ∫ ∏ …) is reached,
    /// the items that follow it up to the next relation (`=`, `<`, …) or the end
    /// of the run become its integrand/summand and go *inside* the `m:nary`'s
    /// `m:e`. Typst juxtaposes the operator and its operand as siblings; OOXML
    /// expects the operand nested, so without this the operand renders outside a
    /// spurious empty box.
    fn emit_items(&mut self, items: &[MathItem]) -> SourceResult<()> {
        let mut i = 0;
        while i < items.len() {
            // An n-ary operator WITH limits: a `Scripts` whose base is a large
            // operator (∑/∫/∏/…). The bounds become sub/sup of an `m:nary`.
            if let MathItem::Component(comp) = &items[i]
                && let MathKind::Scripts(scripts) = &comp.kind
                && let Some(chr) = nary_operator_char(&scripts.base)
                && scripts.top_left.is_none()
                && scripts.bottom_left.is_none()
            {
                let upper = scripts.top.as_ref().or(scripts.top_right.as_ref());
                let lower = scripts.bottom.as_ref().or(scripts.bottom_right.as_ref());
                let lim = scripts.top.is_some() || scripts.bottom.is_some();
                let j = operand_end(items, i + 1);
                self.emit_nary(chr, lower, upper, lim, &items[i + 1..j])?;
                i = j;
            }
            // A BARE large operator with no limits (`∫ f dif x`, `∑ a_i`): still an
            // n-ary operator — typeset it as a stretchy `m:nary` (bounds hidden)
            // binding the following operand, not as a small literal glyph.
            else if let Some(chr) = nary_operator_char(&items[i]) {
                let j = operand_end(items, i + 1);
                self.emit_nary(chr, None, None, false, &items[i + 1..j])?;
                i = j;
            } else {
                self.emit_item(&items[i])?;
                i += 1;
            }
        }
        Ok(())
    }

    /// Emits a single math item.
    fn emit_item(&mut self, item: &MathItem) -> SourceResult<()> {
        let comp = match item {
            MathItem::Component(comp) => comp,
            // A regular inter-atom space, or explicit spacing (`quad`, `#h(..)`):
            // approximate with a space run. Word recomputes most math spacing
            // during buildup, so a single space is a safe, near-lossless choice.
            MathItem::Space | MathItem::Spacing(..) => {
                self.text_run(" ", false);
                return Ok(());
            }
            // Introspection tags carry no rendered output in OMML, but must
            // still reach the introspector so labels/refs *inside* the equation
            // resolve — e.g. a per-line label `#<eqa>`. Hand them to the caller,
            // which knows where they may be re-attached.
            MathItem::Tag(tag) => {
                self.hooks.defer_tag(tag);
                return Ok(());
            }
        };

        self.emit_kind(comp)
    }

    /// Emits the OMML for a single resolved component, dispatching on its kind.
    /// Mirrors `mathml.rs::handle_realized`'s kind match.
    fn emit_kind(&mut self, comp: &MathComponent) -> SourceResult<()> {
        // Carry the component's text colour onto the runs it emits (a colored
        // equation, `$ #text(red)[x] $`). Inherited via the style chain, so each
        // component refreshes it; default/black emits no colour.
        let prev_color = self.color;
        self.color = component_color(comp);
        let r = self.emit_kind_inner(comp);
        self.color = prev_color;
        r
    }

    fn emit_kind_inner(&mut self, comp: &MathComponent) -> SourceResult<()> {
        match &comp.kind {
            MathKind::Group(group) => {
                // A group is a transparent horizontal run of items.
                self.emit_items(&group.items)
            }
            MathKind::Glyph(glyph) => self.emit_glyph(glyph),
            MathKind::Number(num) => self.emit_number(num),
            MathKind::Text(text) => self.emit_text(text),
            MathKind::Primes(primes) => {
                let s: EcoString = std::iter::repeat_n('′', primes.count).collect();
                self.text_run(&s, false);
                Ok(())
            }
            MathKind::Fraction(frac) => self.emit_fraction(frac),
            MathKind::SkewedFraction(frac) => self.emit_skewed_fraction(frac),
            MathKind::Radical(rad) => self.emit_radical(rad),
            MathKind::Scripts(scripts) => self.emit_scripts(scripts),
            MathKind::Accent(acc) => self.emit_accent(acc),
            MathKind::Line(line) => self.emit_bar(&line.base, line.position),
            MathKind::Fenced(fenced) => self.emit_fenced(fenced),
            MathKind::Table(table) => self.emit_table(table),
            MathKind::Multiline(multi) => self.emit_multiline(multi),

            // `cancel(..)` → `m:borderBox` with a diagonal strike — the standard
            // OMML representation (Word renders it; some viewers ignore the
            // strike but still show the base). This is a faithful mapping, not a
            // degradation, so it does not warn.
            MathKind::Cancel(item) => self.emit_borderbox(&item.base, item.cross),
            MathKind::External(item) => {
                if let Some(chr) = external_arrow_char(item) {
                    self.text_run(&chr.to_string(), true);
                }
                Ok(())
            }
            MathKind::Box(_) => {
                unreachable!("unsupported math must be caught by equation preflight")
            }
            MathKind::Mathml(item) => {
                // The `Mathml` variant only carries content for the HTML target;
                // for OOXML its `body` is the resolved IR (if any), so recurse.
                if let Some(body) = &item.body {
                    self.emit_item(body)?;
                }
                Ok(())
            }
        }
    }

    // -- Leaves -------------------------------------------------------------

    /// A single glyph. Typst has ALREADY applied any italic styling by remapping
    /// the letter to its Plane-1 math-alphanumeric codepoint (e.g. `x` → 𝑥
    /// U+1D465, `A` → 𝐴 U+1D434), so a glyph that reaches here is meant to render
    /// as-is. We therefore always emit `m:nor` to stop Word from applying its OWN
    /// default math italic on top — which would (a) double-slant the already-italic
    /// Plane-1 glyphs and (b) wrongly italicize the letters Typst deliberately left
    /// plain/upright: uppercase Greek (Γ, Δ), `upright(..)`, and the differential
    /// `d` of `dif`. Word renders a Plane-1 glyph in upright mode as the italic
    /// glyph it already is, so italic variables still look italic.
    fn emit_glyph(&mut self, glyph: &GlyphItem) -> SourceResult<()> {
        self.text_run(glyph.text.as_str(), true);
        Ok(())
    }

    /// A number: upright digits.
    fn emit_number(&mut self, num: &NumberItem) -> SourceResult<()> {
        self.text_run(num.text.as_str(), true);
        Ok(())
    }

    /// A text string (`"…"`, operator names like `sin`): always upright.
    fn emit_text(&mut self, text: &TextItem) -> SourceResult<()> {
        self.text_run(text.text.as_str(), true);
        Ok(())
    }

    /// Emits a math run `<m:r>(<m:rPr><m:nor/></m:rPr>)?<m:t>text</m:t></m:r>`.
    /// `upright` adds `<m:nor/>` to suppress the default math italic.
    fn text_run(&mut self, text: &str, upright: bool) {
        if text.is_empty() {
            return;
        }
        self.buf.open("m:r").children();
        if upright {
            self.buf.open("m:rPr").children();
            self.buf.leaf("m:nor");
            self.buf.close();
        }
        // A non-default colour rides on a regular `w:rPr` inside the math run
        // (this is how Word colours math), placed after `m:rPr`, before `m:t`.
        if let Some([r, g, b]) = self.color {
            self.buf.open("w:rPr").children();
            self.buf
                .open("w:color")
                .attr("w:val", &format!("{r:02X}{g:02X}{b:02X}"))
                .empty();
            self.buf.close();
        }
        // Preserve significant whitespace.
        self.buf.open("m:t");
        if text.starts_with(' ') || text.ends_with(' ') {
            self.buf.attr("xml:space", "preserve");
        }
        self.buf.children().text(text);
        self.buf.close(); // m:t
        self.buf.close(); // m:r
    }

    // -- Fractions ----------------------------------------------------------

    fn emit_fraction(&mut self, frac: &FractionItem) -> SourceResult<()> {
        self.buf.open("m:f").children();
        // Omit `m:fPr` for a normal bar (the default); set `noBar` for a
        // line-less stack (e.g. `binom`'s inner fraction).
        if !frac.line {
            self.buf.open("m:fPr").children();
            self.buf.open("m:type").attr("m:val", "noBar").empty();
            self.buf.close();
        }
        self.wrap("m:num", &frac.numerator)?;
        self.wrap("m:den", &frac.denominator)?;
        self.buf.close();
        Ok(())
    }

    fn emit_skewed_fraction(&mut self, frac: &SkewedFractionItem) -> SourceResult<()> {
        // OMML can express a skewed fraction natively (`m:type="skw"`); unlike
        // the MathML backend we don't need to bail out.
        self.buf.open("m:f").children();
        self.buf.open("m:fPr").children();
        self.buf.open("m:type").attr("m:val", "skw").empty();
        self.buf.close();
        self.wrap("m:num", &frac.numerator)?;
        self.wrap("m:den", &frac.denominator)?;
        self.buf.close();
        Ok(())
    }

    // -- Radicals -----------------------------------------------------------

    fn emit_radical(&mut self, rad: &RadicalItem) -> SourceResult<()> {
        self.buf.open("m:rad").children();
        match &rad.index {
            None => {
                // Square root: hide the (mandatory) empty degree.
                self.buf.open("m:radPr").children();
                self.buf.open("m:degHide").attr("m:val", "on").empty();
                self.buf.close();
                self.buf.open("m:deg").empty();
            }
            Some(index) => {
                // nth root: the degree comes BEFORE the radicand (the reverse of
                // MathML's `<mroot>`).
                self.wrap("m:deg", index)?;
            }
        }
        self.wrap("m:e", &rad.radicand)?;
        self.buf.close();
        Ok(())
    }

    // -- Scripts / n-ary ----------------------------------------------------

    fn emit_scripts(&mut self, scripts: &ScriptsItem) -> SourceResult<()> {
        if let Some(chr) = math_item_external_arrow_char(&scripts.base)
            && scripts.top_left.is_none()
            && scripts.bottom_left.is_none()
            && scripts.top_right.is_none()
            && scripts.bottom_right.is_none()
        {
            return self.emit_stretchy_arrow(
                chr,
                scripts.top.as_ref(),
                scripts.bottom.as_ref(),
            );
        }

        // If the base is a single large operator (∑ ∫ ∏ ⋃ …) carrying only
        // top/bottom or sub/sup limits, this is an n-ary, not a scripted box.
        if let Some(chr) = nary_operator_char(&scripts.base)
            && scripts.top_left.is_none()
            && scripts.bottom_left.is_none()
        {
            let upper = scripts.top.as_ref().or(scripts.top_right.as_ref());
            let lower = scripts.bottom.as_ref().or(scripts.bottom_right.as_ref());
            let lim_under_over = scripts.top.is_some() || scripts.bottom.is_some();
            // Reached outside a row context (e.g. a wrapped sub-expression): no
            // following operand is available, so the `m:e` stays empty.
            return self.emit_nary(chr, lower, upper, lim_under_over, &[]);
        }

        // Otherwise: ordinary scripts. Apply post/pre-scripts to the base, then
        // wrap that with under/over limits, nesting outwards exactly as the
        // MathML backend does.
        let base = self.scripts_attach_horizontal(scripts)?;
        self.scripts_attach_vertical(scripts, base)
    }

    /// Emits a package-built extensible arrow as native OMML. A hidden phantom
    /// carrying the wider limit gives `m:groupChr` the authored arrow width;
    /// the visible limits are then attached above/below in the usual way.
    fn emit_stretchy_arrow(
        &mut self,
        chr: char,
        upper: Option<&MathItem>,
        lower: Option<&MathItem>,
    ) -> SourceResult<()> {
        let upper = self.render_opt(upper)?;
        let lower = self.render_opt(lower)?;
        let seed = upper.as_deref().or(lower.as_deref()).unwrap_or("");

        let mut base = Omml::new();
        base.open("m:groupChr").children();
        base.open("m:groupChrPr").children();
        base.open("m:chr").attr("m:val", &chr.to_string()).empty();
        base.open("m:pos").attr("m:val", "top").empty();
        base.open("m:vertJc").attr("m:val", "bot").empty();
        base.close();
        base.open("m:e").children();
        base.open("m:phant").children();
        base.open("m:phantPr").children();
        base.open("m:show").attr("m:val", "0").empty();
        base.close();
        base.wrap_raw("m:e", seed);
        base.close();
        base.close();
        base.close();
        let base = base.into_string();

        match (upper, lower) {
            (None, None) => self.buf.raw(&base),
            (Some(upper), None) => {
                self.buf.open("m:limUpp").children();
                self.buf.wrap_raw("m:e", &base);
                self.buf.wrap_raw("m:lim", &upper);
                self.buf.close();
            }
            (None, Some(lower)) => {
                self.buf.open("m:limLow").children();
                self.buf.wrap_raw("m:e", &base);
                self.buf.wrap_raw("m:lim", &lower);
                self.buf.close();
            }
            (Some(upper), Some(lower)) => {
                let mut inner = Omml::new();
                inner.open("m:limLow").children();
                inner.wrap_raw("m:e", &base);
                inner.wrap_raw("m:lim", &lower);
                inner.close();
                self.buf.open("m:limUpp").children();
                self.buf.wrap_raw("m:e", &inner.into_string());
                self.buf.wrap_raw("m:lim", &upper);
                self.buf.close();
            }
        }
        Ok(())
    }

    /// Emits the base with its left (pre) and right (post) sub/superscripts,
    /// returning the serialized fragment.
    fn scripts_attach_horizontal(
        &mut self,
        scripts: &ScriptsItem,
    ) -> SourceResult<String> {
        let base = self.render(&scripts.base)?;
        let tr = self.render_opt(scripts.top_right.as_ref())?;
        let br = self.render_opt(scripts.bottom_right.as_ref())?;
        let tl = self.render_opt(scripts.top_left.as_ref())?;
        let bl = self.render_opt(scripts.bottom_left.as_ref())?;

        // Post-scripts (`m:sSup`/`m:sSub`/`m:sSubSup`).
        let post = match (&tr, &br) {
            (None, None) => None,
            (Some(tr), None) => {
                let mut w = Omml::new();
                w.open("m:sSup").children();
                w.wrap_raw("m:e", &base);
                w.wrap_raw("m:sup", tr);
                w.close();
                Some(w.into_string())
            }
            (None, Some(br)) => {
                let mut w = Omml::new();
                w.open("m:sSub").children();
                w.wrap_raw("m:e", &base);
                w.wrap_raw("m:sub", br);
                w.close();
                Some(w.into_string())
            }
            (Some(tr), Some(br)) => {
                let mut w = Omml::new();
                w.open("m:sSubSup").children();
                w.wrap_raw("m:e", &base);
                w.wrap_raw("m:sub", br);
                w.wrap_raw("m:sup", tr);
                w.close();
                Some(w.into_string())
            }
        };

        // Pre-scripts (`m:sPre`): order is sub, sup, e (scripts before the base).
        let mut out = Omml::new();
        match (&tl, &bl) {
            (None, None) => out.raw(&post.unwrap_or(base)),
            (tl, bl) => {
                let base_for_pre = post.unwrap_or(base);
                out.open("m:sPre").children();
                out.wrap_raw("m:sub", bl.as_deref().unwrap_or(""));
                out.wrap_raw("m:sup", tl.as_deref().unwrap_or(""));
                out.wrap_raw("m:e", &base_for_pre);
                out.close();
            }
        }
        Ok(out.into_string())
    }

    /// Wraps an already-serialized base fragment with under/over limits.
    fn scripts_attach_vertical(
        &mut self,
        scripts: &ScriptsItem,
        base: String,
    ) -> SourceResult<()> {
        let t = self.render_opt(scripts.top.as_ref())?;
        let b = self.render_opt(scripts.bottom.as_ref())?;
        match (t, b) {
            (None, None) => self.buf.raw(&base),
            (Some(t), None) => {
                // Over-limit: `m:limUpp` (limit above a base).
                self.buf.open("m:limUpp").children();
                self.buf.wrap_raw("m:e", &base);
                self.buf.wrap_raw("m:lim", &t);
                self.buf.close();
            }
            (None, Some(b)) => {
                self.buf.open("m:limLow").children();
                self.buf.wrap_raw("m:e", &base);
                self.buf.wrap_raw("m:lim", &b);
                self.buf.close();
            }
            (Some(t), Some(b)) => {
                // Both: nest limLow inside limUpp (upper outermost).
                let mut inner = Omml::new();
                inner.open("m:limLow").children();
                inner.wrap_raw("m:e", &base);
                inner.wrap_raw("m:lim", &b);
                inner.close();
                self.buf.open("m:limUpp").children();
                self.buf.wrap_raw("m:e", &inner.into_string());
                self.buf.wrap_raw("m:lim", &t);
                self.buf.close();
            }
        }
        Ok(())
    }

    /// Emits an n-ary operator (`m:nary`) with the given operator char, optional
    /// lower/upper limits, and the `integrand` items that go inside `m:e` (the
    /// summand/integrand). An empty `integrand` yields an empty `m:e`.
    fn emit_nary(
        &mut self,
        chr: char,
        lower: Option<&MathItem>,
        upper: Option<&MathItem>,
        lim_under_over: bool,
        integrand: &[MathItem],
    ) -> SourceResult<()> {
        self.buf.open("m:nary").children();
        self.buf.open("m:naryPr").children();
        self.buf.open("m:chr").attr("m:val", &chr.to_string()).empty();
        // Limit location: under/over for ∑∏⋃… (or when the source used
        // under/over), sub/sup for integrals.
        let lim =
            if lim_under_over && !is_integral_char(chr) { "undOvr" } else { "subSup" };
        self.buf.open("m:limLoc").attr("m:val", lim).empty();
        self.buf.open("m:grow").attr("m:val", "1").empty();
        if lower.is_none() {
            self.buf.open("m:subHide").attr("m:val", "on").empty();
        }
        if upper.is_none() {
            self.buf.open("m:supHide").attr("m:val", "on").empty();
        }
        self.buf.close(); // m:naryPr

        // sub, sup, e — all present even when empty.
        match lower {
            Some(item) => self.wrap("m:sub", item)?,
            None => self.buf.open("m:sub").empty(),
        }
        match upper {
            Some(item) => self.wrap("m:sup", item)?,
            None => self.buf.open("m:sup").empty(),
        }
        if integrand.is_empty() {
            self.buf.open("m:e").empty();
        } else {
            self.buf.open("m:e").children();
            self.emit_items(integrand)?;
            self.buf.close(); // m:e
        }
        self.buf.close(); // m:nary
        Ok(())
    }

    // -- Accents and bars ---------------------------------------------------

    fn emit_accent(&mut self, acc: &AccentItem) -> SourceResult<()> {
        // The accent item carries the mark as its own sub-item (a glyph);
        // extract its codepoint and map it to a combining form.
        let chr = accent_char(&acc.accent);

        // A spreader (over/under brace, bracket, paren, shell) must STRETCH across
        // the whole base — that is OMML's `m:groupChr`, not the single-glyph
        // `m:acc`. Spreaders are flagged by `exact_frame_width`; an under-accent
        // (`Position::Below`) likewise has no `m:acc` form. Ordinary over-accents
        // (hat, bar, dot, tilde, vec) use `m:acc`.
        if acc.position == Position::Below || acc.exact_frame_width {
            return self.emit_group_char(&acc.base, chr, acc.position);
        }

        self.buf.open("m:acc").children();
        if let Some(chr) = chr {
            self.buf.open("m:accPr").children();
            self.buf.open("m:chr").attr("m:val", &chr.to_string()).empty();
            self.buf.close();
        }
        self.wrap("m:e", &acc.base)?;
        self.buf.close();
        Ok(())
    }

    /// Emits an over/under bar spanning the whole base (`overline`/`underline`).
    fn emit_bar(&mut self, base: &MathItem, position: Position) -> SourceResult<()> {
        self.buf.open("m:bar").children();
        self.buf.open("m:barPr").children();
        let pos = match position {
            Position::Above => "top",
            Position::Below => "bot",
        };
        self.buf.open("m:pos").attr("m:val", pos).empty();
        self.buf.close();
        self.wrap("m:e", base)?;
        self.buf.close();
        Ok(())
    }

    /// Emits a brace/char grouping (`m:groupChr`), used for under-accents.
    fn emit_group_char(
        &mut self,
        base: &MathItem,
        chr: Option<char>,
        position: Position,
    ) -> SourceResult<()> {
        self.buf.open("m:groupChr").children();
        self.buf.open("m:groupChrPr").children();
        if let Some(chr) = chr {
            self.buf.open("m:chr").attr("m:val", &chr.to_string()).empty();
        }
        let pos = match position {
            Position::Above => "top",
            Position::Below => "bot",
        };
        self.buf.open("m:pos").attr("m:val", pos).empty();
        // Align the base on the opposite side of the char.
        let vjc = match position {
            Position::Above => "bot",
            Position::Below => "top",
        };
        self.buf.open("m:vertJc").attr("m:val", vjc).empty();
        self.buf.close();
        self.wrap("m:e", base)?;
        self.buf.close();
        Ok(())
    }

    /// A boxed / struck base (`cancel`). Uses `m:borderBox` with the diagonal
    /// strike(s) but no borders.
    fn emit_borderbox(&mut self, base: &MathItem, cross: bool) -> SourceResult<()> {
        self.buf.open("m:borderBox").children();
        self.buf.open("m:borderBoxPr").children();
        self.buf.open("m:hideTop").attr("m:val", "on").empty();
        self.buf.open("m:hideBot").attr("m:val", "on").empty();
        self.buf.open("m:hideLeft").attr("m:val", "on").empty();
        self.buf.open("m:hideRight").attr("m:val", "on").empty();
        // A single line (BLTR) or a cross (both diagonals).
        self.buf.open("m:strikeBLTR").attr("m:val", "on").empty();
        if cross {
            self.buf.open("m:strikeTLBR").attr("m:val", "on").empty();
        }
        self.buf.close();
        self.wrap("m:e", base)?;
        self.buf.close();
        Ok(())
    }

    // -- Delimiters ---------------------------------------------------------

    fn emit_fenced(&mut self, fenced: &FencedItem) -> SourceResult<()> {
        let beg = fenced.open.as_ref().and_then(|x| delimiter_char(x));
        let end = fenced.close.as_ref().and_then(|x| delimiter_char(x));
        let body: &MathItem = &fenced.body;

        self.buf.open("m:d").children();
        // Emit `m:dPr` when the delimiters differ from the `( )` default or a
        // side is missing (so we can emit an explicit empty char for it).
        let needs_pr = beg != Some('(')
            || end != Some(')')
            || fenced.open.is_none()
            || fenced.close.is_none();
        if needs_pr {
            self.buf.open("m:dPr").children();
            self.buf
                .open("m:begChr")
                .attr("m:val", &delim_val(beg, fenced.open.is_some(), '('))
                .empty();
            self.buf
                .open("m:endChr")
                .attr("m:val", &delim_val(end, fenced.close.is_some(), ')'))
                .empty();
            self.buf.open("m:grow").attr("m:val", "1").empty();
            self.buf.close();
        }
        // Body cell.
        self.buf.open("m:e").children();
        self.emit_row(body)?;
        self.buf.close();
        self.buf.close(); // m:d
        Ok(())
    }

    // -- Tables / matrices --------------------------------------------------

    fn emit_table(&mut self, table: &TableItem) -> SourceResult<()> {
        let ncols = table.cells.first().map_or(0, |row| row.len());
        self.buf.open("m:m").children();
        self.buf.open("m:mPr").children();
        self.buf.open("m:baseJc").attr("m:val", "center").empty();
        self.buf.open("m:plcHide").attr("m:val", "on").empty();
        if ncols > 0 {
            self.buf.open("m:mcs").children();
            self.buf.open("m:mc").children();
            self.buf.open("m:mcPr").children();
            self.buf.open("m:count").attr("m:val", &ncols.to_string()).empty();
            self.buf.open("m:mcJc").attr("m:val", "center").empty();
            self.buf.close(); // m:mcPr
            self.buf.close(); // m:mc
            self.buf.close(); // m:mcs
        }
        self.buf.close(); // m:mPr

        for row in &table.cells {
            self.buf.open("m:mr").children();
            for cell in row {
                // A cell is an `AlignedRow` of sub-columns; flatten its columns
                // into a single `m:e` (alignment points inside a matrix cell are
                // not separately expressible).
                self.buf.open("m:e").children();
                for sub in cell.iter() {
                    self.emit_item(sub)?;
                }
                self.buf.close();
            }
            self.buf.close(); // m:mr
        }
        self.buf.close(); // m:m
        Ok(())
    }

    /// A multiline equation body (`align`/`gather` rows).
    ///
    /// With no alignment points (`gather`, a plain multi-line equation) every row
    /// is a single column → a centered `m:eqArr`, which is exactly right. With
    /// alignment points (`a + b &= c \ x &= y`) `m:eqArr` cannot express the
    /// per-column alignment, so emit a borderless matrix whose columns alternate
    /// right/left justification — matching Typst's layout, which right-aligns the
    /// even columns and left-aligns the odd ones (the `Right` alternator). This
    /// vertically aligns the `&` points the way Word's own aligned equations do.
    fn emit_multiline(&mut self, multi: &MultilineItem) -> SourceResult<()> {
        let ncols = multi.rows.iter().map(|r| r.len()).max().unwrap_or(0);

        if ncols <= 1 {
            self.buf.open("m:eqArr").children();
            for row in &multi.rows {
                self.buf.open("m:e").children();
                for col in row.iter() {
                    self.emit_item(col)?;
                }
                self.buf.close();
            }
            self.buf.close();
            return Ok(());
        }

        self.buf.open("m:m").children();
        self.buf.open("m:mPr").children();
        // Hide the dotted placeholder boxes Word draws for the empty cells that
        // pad short rows.
        self.buf.open("m:plcHide").attr("m:val", "1").empty();
        self.buf.open("m:mcs").children();
        for c in 0..ncols {
            let jc = if c % 2 == 0 { "right" } else { "left" };
            self.buf.open("m:mc").children();
            self.buf.open("m:mcPr").children();
            self.buf.open("m:count").attr("m:val", "1").empty();
            self.buf.open("m:mcJc").attr("m:val", jc).empty();
            self.buf.close(); // m:mcPr
            self.buf.close(); // m:mc
        }
        self.buf.close(); // m:mcs
        // No gap at the alignment point so `a + b` and `= c` read continuously.
        self.buf.open("m:cGp").attr("m:val", "0").empty();
        self.buf.close(); // m:mPr

        for row in &multi.rows {
            self.buf.open("m:mr").children();
            for c in 0..ncols {
                self.buf.open("m:e").children();
                if let Some(col) = row.get(c) {
                    self.emit_item(col)?;
                }
                self.buf.close(); // m:e
            }
            self.buf.close(); // m:mr
        }
        self.buf.close(); // m:m
        Ok(())
    }

    // -- Recursion helpers --------------------------------------------------

    /// Wraps the OMML for `item` inside `<wrapper>…</wrapper>`.
    fn wrap(&mut self, wrapper: &str, item: &MathItem) -> SourceResult<()> {
        let inner = self.render(item)?;
        self.buf.wrap_raw(wrapper, &inner);
        Ok(())
    }

    /// Renders an item to a standalone OMML fragment string (used when a
    /// sub-result must be embedded as a unit, e.g. inside scripts). Reborrows
    /// `hooks` for the duration of the sub-render.
    fn render(&mut self, item: &MathItem) -> SourceResult<String> {
        let mut sub = Emitter::new(&mut *self.hooks);
        sub.emit_row(item)?;
        Ok(sub.buf.into_string())
    }

    /// Renders an optional item; `None` yields `None`.
    fn render_opt(&mut self, item: Option<&MathItem>) -> SourceResult<Option<String>> {
        match item {
            Some(item) => Ok(Some(self.render(item)?)),
            None => Ok(None),
        }
    }
}

// ===========================================================================
// Glyph classification + operator tables (self-contained; no `MathClass`).
// ===========================================================================

/// The component's run colour, if it carries a non-default solid `text(fill:)`
/// (e.g. `#text(red)[$x$]`). Default black returns `None` so ordinary math is
/// byte-identical (no `w:color` noise).
fn component_color(comp: &MathComponent) -> Option<[u8; 3]> {
    use typst_library::visualize::Paint;
    match comp.styles.get_ref(typst_library::text::TextElem::fill) {
        Paint::Solid(c) => {
            let hex = composite_rgb_on_white(c);
            (hex != [0, 0, 0]).then_some(hex)
        }
        _ => None,
    }
}

/// The end index (exclusive) of an n-ary operator's operand, starting at
/// `start`. The operand binds the items that follow the operator up to — but not
/// including — the next *relation* (`=`, `<`, …) or *binary operator* (`+`, `−`,
/// `±`). Stopping at a binary operator keeps `∑_i a_i + ∑_j b_j` as two sibling
/// sums (instead of nesting the second inside the first's operand).
///
/// A following n-ary operator ends the operand too, but only *after* at least
/// one operand item — so `∑_i ∑_j a` still nests (the inner ∑ is the very first
/// operand item), while `∏_i a_i quad ⋃_j b_j` keeps the two big operators as
/// siblings instead of swallowing the second into the first's operand. Ordinary
/// following content (e.g. the `dx` of `∫ f dx`) is not an n-ary, so it stays in
/// the operand as before.
fn operand_end(items: &[MathItem], start: usize) -> usize {
    let mut j = start;
    while j < items.len() {
        if let MathItem::Component(c) = &items[j]
            && matches!(c.props.class, Some(MathClass::Relation | MathClass::Binary))
        {
            break;
        }
        if j > start && item_starts_nary(&items[j]) {
            break;
        }
        j += 1;
    }
    j
}

/// Whether `item` begins an n-ary operator scope — a bare large-operator glyph,
/// or a `Scripts` whose base is one (`∑_i`, `∫_a^b`). Mirrors the detection in
/// `emit_items`, so `operand_end` treats a following *scripted* operator as a
/// sibling boundary too, not only a bare glyph.
fn item_starts_nary(item: &MathItem) -> bool {
    if nary_operator_char(item).is_some() {
        return true;
    }
    matches!(item, MathItem::Component(comp)
        if matches!(&comp.kind, MathKind::Scripts(scripts)
            if nary_operator_char(&scripts.base).is_some()
                && scripts.top_left.is_none()
                && scripts.bottom_left.is_none()))
}

fn nary_operator_char(item: &MathItem) -> Option<char> {
    let comp = match item {
        MathItem::Component(comp) => comp,
        _ => return None,
    };
    let glyph = match &comp.kind {
        MathKind::Glyph(g) => g,
        _ => return None,
    };
    let mut chars = glyph.text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    is_nary_operator(c).then_some(c)
}

/// Extracts the delimiter character from a fence side item (a glyph), if it is a
/// single-character glyph.
fn delimiter_char(item: &MathItem) -> Option<char> {
    let comp = match item {
        MathItem::Component(comp) => comp,
        _ => return None,
    };
    let glyph = match &comp.kind {
        MathKind::Glyph(g) => g,
        _ => return None,
    };
    let mut chars = glyph.text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// Builds the `m:begChr`/`m:endChr` value: the delimiter char, an empty string
/// for a one-sided fence (`present == false`), or the given default.
fn delim_val(chr: Option<char>, present: bool, default: char) -> String {
    if !present {
        // One-sided fence: emit an empty char so Word shows nothing on this side.
        return String::new();
    }
    match chr {
        Some('.') | None => {
            // A `.` delimiter in Typst means "no delimiter"; emit empty.
            if chr == Some('.') { String::new() } else { default.to_string() }
        }
        Some(c) => c.to_string(),
    }
}

/// Extracts the accent mark's character and maps it to its *combining* form
/// (OMML accents require combining codepoints; spacing forms render detached).
fn accent_char(item: &MathItem) -> Option<char> {
    let comp = match item {
        MathItem::Component(comp) => comp,
        _ => return None,
    };
    let glyph = match &comp.kind {
        MathKind::Glyph(g) => g,
        _ => return None,
    };
    let mut chars = glyph.text.chars();
    let c = chars.next()?;
    // Accent glyphs may be multi-codepoint in odd cases; take the first.
    Some(to_combining(c))
}
