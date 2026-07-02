//! Equation mapper: `EquationElem` → native Word equations (OMML).
//!
//! Typst's realized math is lowered to the [`MathItem`] IR via
//! [`resolve_equation`] (the same entry point the paged and HTML backends use),
//! and then transformed directly into OMML (`m:` namespace) — the native,
//! *editable* Word equation format — rather than into MathML or an image.
//!
//! Reusing `crates/typst-html/src/mathml.rs` verbatim is impractical here: that
//! code emits `HtmlElem`/`Content` trees (MathML), and `typst-docx` must not
//! depend on `typst-html`. Instead we walk the `MathItem` IR directly,
//! mirroring `mathml.rs::handle_realized`'s `match &comp.kind { … }` arms but
//! emitting OMML elements. The `MathItem` IR has already done all the
//! structural grouping (fractions, radicals, scripts, fences, tables, …), so
//! each [`MathKind`] variant maps to exactly one OMML construct and the
//! recursion composes — e.g. `binom(n,k)` arrives as a `Fenced` wrapping a
//! no-bar `Fraction`, and `mat(delim: "[")` as a `Fenced` wrapping a `Table`,
//! both of which fall out of the per-variant handlers for free.
//!
//! Inline equations (`block == false`) become a bare `m:oMath` that sits as a
//! sibling of `w:r` runs inside the paragraph; block equations become an
//! `m:oMathPara` (its own paragraph), optionally followed — on the same line,
//! via a right tab — by the rendered equation number.
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

use ecow::{EcoString, eco_format};
use typst_library::diag::SourceResult;
use typst_library::foundations::{NativeElement, Packed, StyleChain};
use typst_library::introspection::{Counter, Locator};
use typst_library::math::EquationElem;
use typst_library::math::ir::{
    AccentItem, FencedItem, FractionItem, GlyphItem, MathComponent, MathItem, MathKind,
    MultilineItem, NumberItem, Position, RadicalItem, ScriptsItem, SkewedFractionItem,
    TableItem, TextItem, resolve_equation,
};
use typst_library::routines::Arenas;

use unicode_math_class::MathClass;

use crate::ctx::DocxCtx;
use crate::dom::{Block, Para, ParaChild, ParaProps, Run, RunProps, TabAlign, TabStop};

/// The result of lowering an equation: inline run or block paragraph(s).
// The inline `Run` is large but the common case; this IR is transient, so boxing
// to shrink the enum isn't worthwhile (see the `Run`/`ParaChild` note in `dom`).
#[allow(clippy::large_enum_variant)]
pub enum EquationOut {
    Inline(Run),
    Block(Vec<Block>),
}

pub fn equation(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<EquationOut> {
    let block = elem.block.get(styles);
    ctx.mark_math();

    // Resolve the equation body to the math IR. The `MathItem` borrows from
    // `arenas`, which lives only for this function — fine, because we serialize
    // it to an OMML `String` immediately and the IR is not needed afterwards.
    //
    // We mirror the HTML backend's `EQUATION_RULE`: `Locator::synthesize` over
    // the element's own location plus a fresh `Arenas`. An equation without a
    // location cannot be resolved this way (it would have been assigned one
    // during realization for any real document element); fall back to alt/empty
    // in that case so export never fails.
    let Some(loc) = elem.location() else {
        return Ok(fallback(elem, styles, block));
    };

    let arenas = Arenas::default();
    let item = resolve_equation(elem, ctx.engine(), Locator::synthesize(loc), &arenas, styles)?;

    // Walk the IR into an `<m:oMath>…</m:oMath>` fragment. `ctx` is reborrowed
    // for the emitter and released when the block ends.
    let omath = {
        let mut emitter = Emitter::new(&mut *ctx);
        emitter.buf.open("m:oMath").children();
        emitter.emit_row(&item)?;
        emitter.buf.close();
        emitter.buf.into_string()
    };

    if !block {
        return Ok(EquationOut::Inline(Run::OmmlInline(omath)));
    }

    // Block equation: wrap the `m:oMath` in an `m:oMathPara` (centered), which
    // must be a direct child of the paragraph (never inside a run).
    let mut para = Omml::new();
    para.open("m:oMathPara").children();
    para.open("m:oMathParaPr").children();
    para.open("m:jc").attr("m:val", "center").empty();
    para.close(); // m:oMathParaPr
    para.raw(&omath);
    para.close(); // m:oMathPara
    let omath_para = para.into_string();

    let mut content = vec![ParaChild::OmmlPara(omath_para)];

    // Append the equation number, if numbered, as a tab + run on the same line.
    // OOXML has no first-class equation-number element; Word's own convention is
    // a right tab stop with the number text, all in the one paragraph. We do not
    // know the page width here, so we use a right-aligned tab stop near the
    // right margin (≈ 6.0" in twips for the default Letter text width).
    let mut props = ParaProps::default();
    if let Some(number) = equation_number(elem, styles, ctx)? {
        content.push(ParaChild::Run(Run::Tab));
        content.extend(number.into_iter().map(ParaChild::Run));
        props.tabs.push(TabStop { val: TabAlign::End, leader: None, pos: 8640 });
    }

    Ok(EquationOut::Block(vec![Block::Para(Para { props, content })]))
}

/// Produces the rendered equation-number runs (e.g. `(1)`), if the equation has
/// a numbering pattern. Mirrors the paged backend's use of
/// `Counter::of(EquationElem::ELEM).display_at(...)`.
fn equation_number(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Vec<Run>>> {
    let Some(numbering) = elem.numbering.get_ref(styles).clone() else {
        return Ok(None);
    };
    let Some(loc) = elem.location() else { return Ok(None) };

    let span = elem.span();
    let number = {
        let result = Counter::of(EquationElem::ELEM)
            .display_at(ctx.engine(), loc, styles, &numbering, span);
        ctx.engine().delay(result).spanned(span)
    };

    // Lower the number content (ordinary inline content) to runs.
    let runs = ctx.inline_runs(&number, styles, RunProps::default())?;
    Ok(Some(runs))
}

/// The graceful fallback when the IR cannot be resolved: emit the equation's
/// `alt` text (or nothing) as a plain run/paragraph so the document still opens.
fn fallback(elem: &Packed<EquationElem>, styles: StyleChain, block: bool) -> EquationOut {
    let alt = elem.alt.get_cloned(styles).unwrap_or_default();
    let run = Run::Text { props: RunProps::default(), text: alt };
    if block {
        EquationOut::Block(vec![Block::Para(Para {
            props: ParaProps::default(),
            content: vec![ParaChild::Run(run)],
        })])
    } else {
        EquationOut::Inline(run)
    }
}

// ===========================================================================
// IR → OMML emitter.
// ===========================================================================

/// Walks the [`MathItem`] IR, emitting OMML into a fresh [`Omml`] buffer.
///
/// `ctx` is borrowed only for the rare items that need a warning fallback; the
/// structural transform itself is pure.
struct Emitter<'c, 'a, 'e> {
    ctx: &'c mut DocxCtx<'a, 'e>,
    buf: Omml,
    /// The current run colour (a non-default solid `text(fill:)` on the enclosing
    /// math component), applied to emitted runs via a `w:rPr`. `None` = default.
    color: Option<[u8; 3]>,
}

impl<'c, 'a, 'e> Emitter<'c, 'a, 'e> {
    fn new(ctx: &'c mut DocxCtx<'a, 'e>) -> Self {
        Self { ctx, buf: Omml::new(), color: None }
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
            // resolve — e.g. a per-line label `#<eqa>`. Defer them (the
            // run-only-context channel), exactly as the inline handler does for
            // tags it cannot position among runs.
            MathItem::Tag(tag) => {
                self.ctx.deferred_tags.push(tag.clone());
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
            MathKind::Box(_) => {
                // Inline boxed content inside math: not expressible as native
                // OMML without laying it out. Drop with a warning rather than
                // corrupt the run.
                //
                // INTEGRATION-NEEDED: a true image fallback for `box(..)` inside
                // an equation would need the box laid out to a frame
                // (typst-layout) + `ctx.add_image`; typst-docx does not depend on
                // typst-layout, so this is deferred to integration.
                self.warn(comp, "inline box in equation");
                Ok(())
            }
            MathKind::External(_) => {
                // External content (e.g. a placed element) cannot be inlined as
                // OMML. See the box note above.
                self.warn(comp, "external content in equation");
                Ok(())
            }
            MathKind::Mathml(item) => {
                // The `Mathml` variant only carries content for the HTML target;
                // for DOCX its `body` is the resolved IR (if any), so recurse.
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

    /// Emits the base with its left (pre) and right (post) sub/superscripts,
    /// returning the serialized fragment.
    fn scripts_attach_horizontal(&mut self, scripts: &ScriptsItem) -> SourceResult<String> {
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
        let lim = if lim_under_over && !is_integral_char(chr) { "undOvr" } else { "subSup" };
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
    /// `ctx` for the duration of the sub-render.
    fn render(&mut self, item: &MathItem) -> SourceResult<String> {
        let mut sub = Emitter::new(&mut *self.ctx);
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

    /// Emits a non-fatal warning that a math construct was degraded.
    fn warn(&mut self, comp: &MathComponent, what: &str) {
        self.ctx
            .warn_ignored(&eco_format!("{what} (in equation)"), comp.props.span);
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
            let hex = crate::props::color_to_hex(c);
            (hex != [0, 0, 0]).then_some(hex)
        }
        _ => None,
    }
}

/// The end index (exclusive) of an n-ary operator's operand, starting at
/// `start`. The operand binds the items that follow the operator up to — but not
/// including — the next *relation* (`=`, `<`, …) or *binary operator* (`+`, `−`,
/// `±`). Stopping at a binary operator keeps `∑_i a_i + ∑_j b_j` as two sibling
/// sums (instead of nesting the second inside the first's operand), while a
/// following n-ary operator is NOT a boundary, so `∑_i ∑_j a` still nests.
fn operand_end(items: &[MathItem], start: usize) -> usize {
    let mut j = start;
    while j < items.len() {
        if let MathItem::Component(c) = &items[j]
            && matches!(c.props.class, Some(MathClass::Relation | MathClass::Binary))
        {
            break;
        }
        j += 1;
    }
    j
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

/// Whether the character is a large (n-ary) operator that takes limits.
fn is_nary_operator(c: char) -> bool {
    is_integral_char(c)
        || matches!(
            c,
            '∑'     // n-ary summation U+2211
            | '∏'   // n-ary product U+220F
            | '∐'   // n-ary coproduct U+2210
            | '⋃'   // n-ary union U+22C3
            | '⋂'   // n-ary intersection U+22C2
            | '⋁'   // n-ary logical or U+22C1
            | '⋀'   // n-ary logical and U+22C0
            | '⨄'   // n-ary union with plus U+2A04
            | '⨃'   // n-ary union with dot U+2A03
            | '⨆'   // n-ary square union U+2A06
            | '⨅'   // n-ary square intersection U+2A05
            | '⨀'   // n-ary circled dot U+2A00
            | '⨁'   // n-ary circled plus U+2A01
            | '⨂'   // n-ary circled times U+2A02
            | '⫿'   // n-ary triple vertical bar U+2AFF
        )
}

/// Whether the character is one of the integral signs (limits as sub/sup).
/// Mirrors `typst_library::math`'s private `is_integral_char`.
fn is_integral_char(c: char) -> bool {
    ('∫'..='∳').contains(&c) || ('⨋'..='⨜').contains(&c)
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

/// Maps a spacing accent character to its combining equivalent. Combining marks
/// (U+0300–U+036F, U+20D0–U+20FF) and characters with no spacing form pass
/// through unchanged.
fn to_combining(c: char) -> char {
    match c {
        // Spacing → combining for the common math accents.
        '`' => '\u{0300}',         // grave
        '´' => '\u{0301}',         // acute
        '^' => '\u{0302}',         // circumflex / hat
        '~' => '\u{0303}',         // tilde
        '¯' | '\u{02C9}' => '\u{0304}', // macron / bar (+ modifier macron)
        '\u{02D8}' => '\u{0306}',  // breve
        '\u{02D9}' => '\u{0307}',  // dot above
        '¨' => '\u{0308}',         // diaeresis / ddot
        '°' | '\u{02DA}' => '\u{030A}', // ring above
        '\u{02DD}' => '\u{030B}',  // double acute
        'ˇ' => '\u{030C}',         // caron / check
        '→' => '\u{20D7}',         // rightwards arrow → combining (vec)
        '←' => '\u{20D6}',         // leftwards arrow → combining
        '↔' => '\u{20E1}',         // left-right arrow → combining
        // Already a combining mark, or a dedicated accent codepoint: keep it.
        _ => c,
    }
}

// ===========================================================================
// OMML string builder (no XML declaration; balanced by construction).
// ===========================================================================

/// A minimal XML builder for OMML *fragments*.
///
/// We cannot reuse `crate::xml::XmlWriter` directly because its constructor
/// emits the `<?xml …?>` declaration (illegal inside a fragment that gets
/// `raw`-spliced into `document.xml`). A fragment-local builder also keeps this
/// module self-contained within its single-file boundary.
///
/// Escaping reuses `crate::xml::{escape, escape_attr}` so it matches the rest of
/// the package exactly (same control-char stripping, same entity set).
struct Omml {
    buf: String,
    stack: Vec<String>,
    /// Whether a start tag is currently open (awaiting attributes / children).
    open_tag: bool,
}

impl Omml {
    fn new() -> Self {
        Self { buf: String::new(), stack: Vec::new(), open_tag: false }
    }

    fn into_string(self) -> String {
        debug_assert!(self.stack.is_empty(), "Omml: unbalanced elements: {:?}", self.stack);
        debug_assert!(!self.open_tag, "Omml: dangling open tag");
        self.buf
    }

    fn flush_open(&mut self) {
        if self.open_tag {
            self.buf.push('>');
            self.open_tag = false;
        }
    }

    /// Opens an element `<name`; attributes may follow until `children`/`empty`.
    fn open(&mut self, name: &str) -> &mut Self {
        self.flush_open();
        self.buf.push('<');
        self.buf.push_str(name);
        self.stack.push(name.to_string());
        self.open_tag = true;
        self
    }

    /// Adds an attribute (value escaped).
    fn attr(&mut self, name: &str, value: &str) -> &mut Self {
        debug_assert!(self.open_tag, "Omml::attr with no open tag");
        self.buf.push(' ');
        self.buf.push_str(name);
        self.buf.push_str("=\"");
        self.buf.push_str(&crate::xml::escape_attr(value));
        self.buf.push('"');
        self
    }

    /// Closes the start tag (`>`) so children/text may follow.
    fn children(&mut self) -> &mut Self {
        self.flush_open();
        self
    }

    /// Self-closes the currently open element `<name .../>`.
    fn empty(&mut self) {
        debug_assert!(self.open_tag, "Omml::empty with no open tag");
        self.buf.push_str("/>");
        self.open_tag = false;
        self.stack.pop();
    }

    /// A self-closing leaf element `<name/>`.
    fn leaf(&mut self, name: &str) {
        self.open(name).empty();
    }

    /// Emits escaped text content.
    fn text(&mut self, s: &str) {
        self.flush_open();
        self.buf.push_str(&crate::xml::escape(s));
    }

    /// Splices already-serialized child XML verbatim.
    fn raw(&mut self, xml: &str) {
        self.flush_open();
        self.buf.push_str(xml);
    }

    /// Closes the most recently opened element. If it had no children, it is
    /// emitted as `<name></name>` (some OMML readers dislike self-closing
    /// structural elements that were opened with `children()`).
    fn close(&mut self) {
        let name = self.stack.pop().expect("Omml: close with empty stack");
        self.flush_open();
        self.buf.push_str("</");
        self.buf.push_str(&name);
        self.buf.push('>');
    }

    /// Convenience: `<wrapper>{inner}</wrapper>` where `inner` is raw OMML.
    fn wrap_raw(&mut self, wrapper: &str, inner: &str) {
        self.open(wrapper).children();
        self.raw(inner);
        self.close();
    }
}
