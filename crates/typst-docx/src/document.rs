//! The DOCX export driver: realizes the native element tree and walks it into
//! the typed IR.

use std::sync::Arc;

use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, StyleChain};
use typst_library::introspection::{Locator, Tag};
use typst_library::model::DocumentInfo;
use typst_library::routines::{Arenas, RealizationKind};

use crate::ctx::DocxCtx;
use crate::dom::{Block, DocxDocument, ParaChild, SectPr};
use crate::introspect::DocxIntrospector;

/// Produces a DOCX document (in-memory IR) from content.
///
/// First performs root-level realization, then walks the resulting native
/// elements into the typed DOCX IR. The OPC zip is written separately by
/// [`crate::docx`].
#[typst_macros::time(name = "docx document")]
pub fn docx_document(
    engine: &mut Engine,
    content: &Content,
    styles: StyleChain,
) -> SourceResult<DocxDocument> {
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

    // Walk the native element tree into the typed IR.
    let (body, footnotes, numbering, media, doc_rels, bookmarks, max_heading_level, uses_fields, uses_math) = {
        let mut ctx = DocxCtx::new(engine, &mut locator);
        let body = crate::convert::run(&mut ctx, &pairs)?;
        (
            body,
            std::mem::take(&mut ctx.footnotes),
            std::mem::take(&mut ctx.numbering),
            std::mem::take(&mut ctx.media),
            std::mem::take(&mut ctx.doc_rels),
            std::mem::take(&mut ctx.bookmarks),
            ctx.max_heading_level,
            ctx.uses_fields,
            ctx.uses_math,
        )
    };

    // Collect introspection tags from the IR for the introspector.
    let mut tags = Vec::new();
    collect_tags(&body, &mut tags);
    for fnote in &footnotes {
        collect_tags(&fnote.blocks, &mut tags);
    }

    let mut introspector = DocxIntrospector::new(&tags);
    introspector.set_anchors(crate::bookmark::anchors(&bookmarks));

    Ok(DocxDocument {
        info,
        body,
        sect: SectPr::default(),
        footnotes,
        numbering,
        media,
        doc_rels,
        bookmarks,
        max_heading_level,
        uses_fields,
        uses_math,
        introspector: Arc::new(introspector),
    })
}

/// Recursively collects introspection tags from the IR.
fn collect_tags(blocks: &[Block], out: &mut Vec<Tag>) {
    for block in blocks {
        match block {
            Block::Tag(tag) => out.push(tag.clone()),
            Block::Para(para) => {
                for child in &para.content {
                    if let ParaChild::Tag(tag) = child {
                        out.push(tag.clone());
                    }
                }
            }
            Block::Table(tbl) => {
                for row in &tbl.rows {
                    for cell in &row.cells {
                        collect_tags(&cell.blocks, out);
                    }
                }
            }
            Block::SectionBreak(_) => {}
        }
    }
}
