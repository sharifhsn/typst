//! The Pandoc export driver: realizes the native element tree and walks it into
//! the Pandoc AST.

use std::sync::Arc;

use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, StyleChain};
use typst_library::introspection::{Locator, Tag};
use typst_library::model::DocumentInfo;
use typst_library::routines::{Arenas, RealizationKind};

use typst_syntax::Span;

use crate::ast::{Block, Inline, Meta, MetaValue};
use crate::ctx::PandocCtx;
use crate::dom::PandocDocument;
use crate::introspect::PandocIntrospector;

/// Produces a Pandoc document (in-memory AST) from content.
///
/// First performs root-level realization, then walks the resulting native
/// elements into the Pandoc AST. The JSON bytes are written separately by
/// [`crate::pandoc`].
#[typst_macros::time(name = "pandoc document")]
pub fn pandoc_document(
    engine: &mut Engine,
    content: &Content,
    styles: StyleChain,
) -> SourceResult<PandocDocument> {
    // Mark the external styles as document-level "outside".
    let styles = styles.to_map().outside();
    let styles = StyleChain::new(&styles);

    let mut locator = Locator::root().split();
    let arenas = Arenas::default();

    let mut info = DocumentInfo::default();
    info.populate(styles);
    info.populate_locale(styles);

    let children = (engine.library.routines.realize)(
        RealizationKind::Document { info: &mut info },
        engine,
        &mut locator,
        &arenas,
        content,
        styles,
    )?;

    let pairs: Vec<_> = children.to_vec();

    // Synthesize the document's bibliography into a BibLaTeX (`.bib`) string, so
    // the CLI can write it as a sidecar beside the JSON. This lets
    // `pandoc --citeproc --bibliography=<sidecar>` re-resolve the structured
    // `Cite` nodes the citation mapper emits. The query is a pure read of the
    // (now-stabilized) shared introspector — no sink side effect — so it does not
    // perturb convergence. `None` when the document has no bibliography.
    let bibliography = {
        let introspector = engine.introspector.access(
            "querying bibliography elements to synthesize a .bib sidecar is a pure query",
        );
        typst_library::model::BibliographyElem::biblatex(*introspector)
    };

    // Build the bibliography-entry anchor → cite-key map, so the conversion's
    // post-walk pass can recover the cite key behind each realized in-text
    // citation `Link` and emit a structured `Cite`. `Works::generate` is the same
    // memoized call the bibliography show rule already made during realization, so
    // this is effectively free and cannot perturb convergence (it is a pure
    // function of the stabilized introspector). An empty list when there is no
    // bibliography (or a transient `Works` failure) — the self-contained formatted
    // references still render either way.
    let entry_keys = match typst_library::model::Works::generate(engine, Span::detached())
    {
        Ok(works) => works.entry_keys(),
        Err(_) => Vec::new(),
    };

    // Walk the native element tree into the Pandoc AST.
    //
    // Isolate the conversion walk's error sink (same reasoning as the rasterize
    // re-layout isolation): lowering already-realized content can surface
    // *delayed* errors for values that only resolve during layout (a date
    // `display(auto)`, a page-number query) which the main realize never hit.
    // Those must not fail a best-effort export. The shared introspector and its
    // convergence constraint are kept (refs/cites/bibliography still resolve);
    // only this walk's own delayed-error reporting is discarded. Warnings are
    // forwarded to the real sink.
    let mut conv_sink = typst_library::engine::Sink::new();
    let (blocks, deferred_tags) = {
        use comemo::Track;
        let mut sub = typst_library::engine::Engine {
            world: engine.world,
            library: engine.library,
            introspector: typst_utils::Protected::from_raw(
                engine.introspector.into_raw(),
            ),
            traced: engine.traced,
            sink: conv_sink.track_mut(),
            route: typst_library::engine::Route::extend(engine.route.track()),
        };
        let mut ctx = PandocCtx::new(&mut sub, &mut locator);
        ctx.load_cite_anchors(&entry_keys);
        let blocks = crate::convert::run(&mut ctx, &pairs)?;
        let deferred_tags = std::mem::take(&mut ctx.deferred_tags);
        (blocks, deferred_tags)
    };
    // Forward conversion warnings to the real sink (delayed errors stay isolated
    // in `conv_sink` and are dropped).
    for w in conv_sink.warnings() {
        engine.sink.warn(w);
    }

    // Collect introspection tags from the AST for the introspector, plus the
    // tags harvested from rasterized content.
    let mut tags = Vec::new();
    collect_tags(&blocks, &mut tags);
    tags.extend(deferred_tags);

    let introspector = PandocIntrospector::new(&tags);

    Ok(PandocDocument {
        info,
        blocks,
        introspector: Arc::new(introspector),
        bibliography,
    })
}

/// Builds the Pandoc `meta` map from the document info (title/author/date/
/// keywords/description). Empty fields are omitted; the map is `{}` when nothing
/// is set. Meta values are tagged (`{"t":"MetaInlines",…}`) — a bare string
/// hard-fails pandoc's reader.
pub(crate) fn build_meta(info: &DocumentInfo, bibliography: Option<&str>) -> Meta {
    use typst_library::foundations::Smart;

    let mut meta = Meta::new();

    // Record the synthesized `.bib` sidecar so `pandoc --citeproc` re-resolves
    // the structured `Cite` nodes against it. Pandoc reads `bibliography` from
    // the document metadata as a string (or list of strings) path; a single
    // `MetaInlines` string is the canonical form.
    if let Some(path) = bibliography
        && !path.is_empty()
    {
        meta.insert(
            "bibliography".into(),
            MetaValue::MetaInlines(vec![Inline::Str(path.to_string())]),
        );
    }

    if let Some(title) = &info.title
        && !title.is_empty()
    {
        meta.insert(
            "title".into(),
            MetaValue::MetaInlines(vec![Inline::Str(title.to_string())]),
        );
    }

    if !info.author.is_empty() {
        let authors = info
            .author
            .iter()
            .map(|a| MetaValue::MetaInlines(vec![Inline::Str(a.to_string())]))
            .collect();
        meta.insert("author".into(), MetaValue::MetaList(authors));
    }

    if let Smart::Custom(Some(date)) = &info.date
        && let Some(iso) = datetime_iso(date)
    {
        meta.insert("date".into(), MetaValue::MetaInlines(vec![Inline::Str(iso)]));
    }

    if !info.keywords.is_empty() {
        let kws = info
            .keywords
            .iter()
            .map(|k| MetaValue::MetaInlines(vec![Inline::Str(k.to_string())]))
            .collect();
        meta.insert("keywords".into(), MetaValue::MetaList(kws));
    }

    if let Some(desc) = &info.description
        && !desc.is_empty()
    {
        meta.insert(
            "abstract".into(),
            MetaValue::MetaInlines(vec![Inline::Str(desc.to_string())]),
        );
    }

    meta
}

/// Formats a `Datetime` as an ISO `yyyy-mm-dd` string when it has a date part.
fn datetime_iso(date: &typst_library::foundations::Datetime) -> Option<String> {
    let y = date.year()?;
    let m = date.month()?;
    let d = date.day()?;
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Recursively collects introspection tags carried in the AST as `RawInline`
/// markers is NOT how we store them — tags live only in `deferred_tags` for
/// Pandoc (there is no `Block::Tag` node), so this currently walks nothing but
/// exists for symmetry and future block-level tag carriers. The introspector is
/// built primarily from `deferred_tags`.
fn collect_tags(_blocks: &[Block], _out: &mut Vec<Tag>) {
    // Pandoc has no tag-bearing AST node; all tags are deferred during the walk
    // (run-only contexts and rasterized content), so nothing to collect here.
}
