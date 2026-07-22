//! The chart-cache point reader shared by the OOXML importers.
//!
//! Both a `.docx` and a `.pptx` chart keep a *cache* of the values it last
//! drew, as a list of `<c:pt idx="…">` points. The cache is sparse: a point
//! with no data is omitted entirely, so its `@idx` — not its position in the
//! list — is what pins a value to its category. Reading positionally would
//! silently shift every later value up a row the moment one index is missing.
//!
//! This is pure mechanics. What counts as a point's text (a nested `c:v`, a
//! `cx:pt`'s own text, trimmed or not) and how a gap is represented afterwards
//! (empty string vs. `None`) are the caller's to decide.

use ecow::EcoString;

/// The largest `@idx` the reader will honor. A cache index is an
/// attacker-controlled integer that sizes a vector; `idx="4000000000"` would
/// ask for four billion entries from a few bytes of XML. No real chart has
/// more points than a spreadsheet has rows.
pub const MAX_CHART_POINTS: usize = 1 << 20;

/// Place each point into a dense, sparse-aware `Vec`: index `i` is `Some(text)`
/// if some point declared `@idx = i`, else `None`. `idx_of`/`text_of` read one
/// point; a point with no parsable index is skipped, and an index at or beyond
/// [`MAX_CHART_POINTS`] is dropped rather than allowed to force the allocation.
/// Where several points share an index, the last one wins.
pub fn indexed_points<P: Copy>(
    points: impl Iterator<Item = P>,
    idx_of: impl Fn(P) -> Option<usize>,
    text_of: impl Fn(P) -> EcoString,
) -> Vec<Option<EcoString>> {
    let mut out: Vec<Option<EcoString>> = Vec::new();
    for pt in points {
        let Some(index) = idx_of(pt) else { continue };
        if index >= MAX_CHART_POINTS {
            continue;
        }
        if out.len() <= index {
            out.resize(index + 1, None);
        }
        out[index] = Some(text_of(pt));
    }
    out
}
