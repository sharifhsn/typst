//! Format-specific invariants for the finalized DOCX IR.
//!
//! OPC validates package mechanics. These checks cover WordprocessingML IDs
//! whose meaning spans several XML elements or parts and therefore cannot be
//! inferred by the format-neutral package writer.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Display, Formatter};

use ecow::EcoString;

use crate::dom::{
    Block, DocxDocument, FieldCacheStatus, FieldDisplay, FieldMode, Footnote, HdrFtrPart,
    Para, ParaChild, Run,
};

/// Strips redundant re-emissions of the same bookmark within one part.
///
/// Repeated content legitimately lowers the same `Location` more than once —
/// a slide deck re-shows a labeled element on every subslide, a labeled
/// element in a running head repeats per header part — and the idempotent
/// per-`Location` allocation then emits the same bookmark id/name at each
/// occurrence. Consumers resolve a bookmark name to its FIRST occurrence, so
/// only the first start/end pair carries meaning; later repeats are dropped
/// here so the serialized parts satisfy [`validate`]'s uniqueness invariants
/// instead of shipping spec-invalid duplicates for Word to repair. The walk
/// and per-part scoping mirror [`validate`] exactly.
pub(crate) fn dedupe_repeated_bookmarks(
    body: &mut [Block],
    headers: &mut [HdrFtrPart],
    footers: &mut [HdrFtrPart],
    footnotes: &mut [Footnote],
) {
    let mut scope = BookmarkScope::default();
    dedupe_blocks(body, &mut scope);
    for part in headers.iter_mut().chain(footers.iter_mut()) {
        let mut scope = BookmarkScope::default();
        dedupe_blocks(&mut part.blocks, &mut scope);
    }
    // All footnotes serialize into one part (word/footnotes.xml).
    let mut scope = BookmarkScope::default();
    for footnote in footnotes {
        dedupe_blocks(&mut footnote.blocks, &mut scope);
    }
}

/// Maps Typst's paint order onto Word's separate text/drawing layers for the
/// common background idiom: one or more non-text `place` drawings precede all
/// flowing content in the same story/container. Word otherwise paints every
/// foreground anchor over ordinary text regardless of XML order, hiding the
/// later content. A leading source drawing belongs behind that later text.
///
/// We deliberately require that no flowing content precedes the drawing. Word
/// has no layer that is simultaneously above earlier text and below later text;
/// leaving such mid-flow overlays in front is the honest, deterministic choice.
pub(crate) fn resolve_leading_background_layers(
    body: &mut [Block],
    headers: &mut [HdrFtrPart],
    footers: &mut [HdrFtrPart],
    footnotes: &mut [Footnote],
) {
    resolve_block_backgrounds(body);
    for part in headers.iter_mut().chain(footers.iter_mut()) {
        resolve_block_backgrounds(&mut part.blocks);
    }
    for footnote in footnotes {
        resolve_block_backgrounds(&mut footnote.blocks);
    }
}

fn resolve_block_backgrounds(blocks: &mut [Block]) {
    for block in blocks.iter_mut() {
        match block {
            Block::Para(para) => resolve_para_backgrounds(para),
            Block::Table(table) => {
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        resolve_block_backgrounds(&mut cell.blocks);
                    }
                }
            }
            Block::Toc(toc) => {
                for entry in &mut toc.entries {
                    resolve_para_backgrounds(entry);
                }
            }
            Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => {}
        }
    }

    let Some(first_flow) = blocks.iter().position(block_has_flow_content) else {
        return;
    };
    for block in &mut blocks[..first_flow] {
        set_block_drawings_behind(block);
    }
}

fn resolve_para_backgrounds(para: &mut Para) {
    let Some(first_flow) = para.content.iter().position(para_child_has_flow_content)
    else {
        return;
    };
    for child in &mut para.content[..first_flow] {
        if let ParaChild::Run(run) = child {
            set_run_drawing_behind(run);
        }
    }
}

fn block_has_flow_content(block: &Block) -> bool {
    match block {
        Block::Para(para) => para.content.iter().any(para_child_has_flow_content),
        Block::Table(_) | Block::Toc(_) => true,
        Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => false,
    }
}

fn para_child_has_flow_content(child: &ParaChild) -> bool {
    match child {
        ParaChild::Run(run) => run_has_flow_content(run),
        ParaChild::Hyperlink { runs, .. } => runs.iter().any(run_has_flow_content),
        ParaChild::OmmlPara(_) => true,
        ParaChild::BookmarkStart { .. }
        | ParaChild::BookmarkEnd { .. }
        | ParaChild::Tag(_) => false,
    }
}

fn run_has_flow_content(run: &Run) -> bool {
    match run {
        Run::Text { text, .. } => !text.is_empty(),
        Run::Field(field) => {
            field.display == FieldDisplay::Visible
                && field.result.iter().any(run_has_flow_content)
        }
        Run::OmmlInline(_)
        | Run::FootnoteRef { .. }
        | Run::FootnoteRefMark
        | Run::Break { .. }
        | Run::PageBreak
        | Run::ColumnBreak
        | Run::Tab
        | Run::FillTab => true,
        Run::Drawing(_) => false,
    }
}

fn set_block_drawings_behind(block: &mut Block) {
    if let Block::Para(para) = block {
        for child in &mut para.content {
            if let ParaChild::Run(run) = child {
                set_run_drawing_behind(run);
            }
        }
    }
}

fn set_run_drawing_behind(run: &mut Run) {
    let Run::Drawing(drawing) = run else { return };
    if drawing.has_native_text() {
        return;
    }
    if let Some(anchor) = &mut drawing.anchor
        && matches!(anchor.wrap, crate::dom::AnchorWrap::None)
    {
        anchor.behind = true;
    }
}

/// Replaces an internal field whose target bookmark does not exist with its
/// cached result. This is deliberately a final-IR pass: a reference can precede
/// its target, and only after all lowering and bookmark deduplication do we know
/// whether the target number was actually emitted.
///
/// Returning the missing target names lets the caller retain an explicit
/// fidelity decision for the lost consumer-side update behavior.
pub(crate) fn fallback_dangling_internal_fields(
    body: &mut [Block],
    headers: &mut [HdrFtrPart],
    footers: &mut [HdrFtrPart],
    footnotes: &mut [Footnote],
) -> Vec<EcoString> {
    let mut bookmarks = BTreeSet::new();
    collect_bookmark_names(body, &mut bookmarks);
    for part in headers.iter().chain(footers.iter()) {
        collect_bookmark_names(&part.blocks, &mut bookmarks);
    }
    for footnote in footnotes.iter() {
        collect_bookmark_names(&footnote.blocks, &mut bookmarks);
    }

    let mut missing = Vec::new();
    rewrite_dangling_fields_in_blocks(body, &bookmarks, &mut missing);
    for part in headers.iter_mut().chain(footers.iter_mut()) {
        rewrite_dangling_fields_in_blocks(&mut part.blocks, &bookmarks, &mut missing);
    }
    for footnote in footnotes {
        rewrite_dangling_fields_in_blocks(&mut footnote.blocks, &bookmarks, &mut missing);
    }
    missing
}

fn internal_bookmark_target(instruction: &str) -> Option<&str> {
    let mut words = instruction.split_whitespace();
    match words.next()? {
        "REF" | "PAGEREF" | "NOTEREF" => words.next(),
        _ => None,
    }
}

fn collect_bookmark_names(blocks: &[Block], names: &mut BTreeSet<EcoString>) {
    for block in blocks {
        match block {
            Block::Para(para) => collect_para_bookmarks(para, names),
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        collect_bookmark_names(&cell.blocks, names);
                    }
                }
            }
            Block::Toc(toc) => {
                for entry in &toc.entries {
                    collect_para_bookmarks(entry, names);
                }
                for run in &toc.fallback {
                    collect_run_bookmarks(run, names);
                }
            }
            Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => {}
        }
    }
}

fn collect_para_bookmarks(para: &Para, names: &mut BTreeSet<EcoString>) {
    for child in &para.content {
        match child {
            ParaChild::BookmarkStart { name, .. } => {
                names.insert(name.clone());
            }
            ParaChild::Run(run) => collect_run_bookmarks(run, names),
            ParaChild::Hyperlink { runs, .. } => {
                for run in runs {
                    collect_run_bookmarks(run, names);
                }
            }
            ParaChild::BookmarkEnd { .. }
            | ParaChild::OmmlPara(_)
            | ParaChild::Tag(_) => {}
        }
    }
}

fn collect_run_bookmarks(run: &Run, names: &mut BTreeSet<EcoString>) {
    match run {
        Run::FootnoteRef { bookmark: Some((_, name)), .. } => {
            // The first native footnote mark carries its bookmark inside the
            // run; the encoder materializes the start/end pair around
            // `<w:footnoteReference>`. Count it as a real target so later
            // NOTEREF fields are not mistaken for dangling references and
            // flattened to cached text.
            names.insert(name.clone());
        }
        Run::Drawing(drawing) => {
            if let Some(text_box) =
                drawing.shape.as_ref().and_then(|shape| shape.txbx.as_ref())
            {
                collect_bookmark_names(&text_box.blocks, names);
            }
            if let Some(group) = &drawing.group {
                for child in &group.children {
                    if let Some(text_box) = &child.shape.txbx {
                        collect_bookmark_names(&text_box.blocks, names);
                    }
                }
            }
        }
        Run::Field(field) => {
            for result in &field.result {
                collect_run_bookmarks(result, names);
            }
        }
        _ => {}
    }
}

fn rewrite_dangling_fields_in_blocks(
    blocks: &mut [Block],
    bookmarks: &BTreeSet<EcoString>,
    missing: &mut Vec<EcoString>,
) {
    for block in blocks {
        match block {
            Block::Para(para) => {
                rewrite_dangling_fields_in_para(para, bookmarks, missing)
            }
            Block::Table(table) => {
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        rewrite_dangling_fields_in_blocks(
                            &mut cell.blocks,
                            bookmarks,
                            missing,
                        );
                    }
                }
            }
            Block::Toc(toc) => {
                for entry in &mut toc.entries {
                    rewrite_dangling_fields_in_para(entry, bookmarks, missing);
                }
                rewrite_dangling_fields_in_runs(&mut toc.fallback, bookmarks, missing);
            }
            Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => {}
        }
    }
}

fn rewrite_dangling_fields_in_para(
    para: &mut Para,
    bookmarks: &BTreeSet<EcoString>,
    missing: &mut Vec<EcoString>,
) {
    let old = std::mem::take(&mut para.content);
    for child in old {
        match child {
            ParaChild::Run(run) => {
                let mut runs = vec![run];
                rewrite_dangling_fields_in_runs(&mut runs, bookmarks, missing);
                para.content.extend(runs.into_iter().map(ParaChild::Run));
            }
            ParaChild::Hyperlink { rel, anchor, mut runs } => {
                rewrite_dangling_fields_in_runs(&mut runs, bookmarks, missing);
                para.content.push(ParaChild::Hyperlink { rel, anchor, runs });
            }
            other => para.content.push(other),
        }
    }
}

fn rewrite_dangling_fields_in_runs(
    runs: &mut Vec<Run>,
    bookmarks: &BTreeSet<EcoString>,
    missing: &mut Vec<EcoString>,
) {
    let old = std::mem::take(runs);
    for mut run in old {
        match &mut run {
            Run::Drawing(drawing) => {
                if let Some(text_box) =
                    drawing.shape.as_mut().and_then(|shape| shape.txbx.as_mut())
                {
                    rewrite_dangling_fields_in_blocks(
                        &mut text_box.blocks,
                        bookmarks,
                        missing,
                    );
                }
                if let Some(group) = drawing.group.as_mut() {
                    for child in &mut group.children {
                        if let Some(text_box) = child.shape.txbx.as_mut() {
                            rewrite_dangling_fields_in_blocks(
                                &mut text_box.blocks,
                                bookmarks,
                                missing,
                            );
                        }
                    }
                }
            }
            Run::Field(field) => {
                rewrite_dangling_fields_in_runs(&mut field.result, bookmarks, missing);
                if field.display == FieldDisplay::Visible
                    && !field.result.is_empty()
                    && let Some(target) = internal_bookmark_target(&field.instr)
                    && !bookmarks.contains(target)
                {
                    // A normal reference can be provisionally aimed at a
                    // number-only bookmark before its target lowers. If the
                    // target emitted no displayed number but did emit its
                    // whole-element bookmark, retain the exact cached label as
                    // a locked, clickable REF to that real bookmark. Updating
                    // fields cannot then replace it with the target's full body.
                    if let Some(base) = target.strip_suffix("Number")
                        && bookmarks.contains(base)
                        && field.instr.split_whitespace().next() == Some("REF")
                    {
                        field.instr = ecow::eco_format!(" REF {base} \\h ");
                        field.mode = FieldMode::Static;
                        field.cache_status = FieldCacheStatus::Resolved;
                        runs.push(run);
                        continue;
                    }
                    missing.push(target.into());
                    runs.append(&mut field.result);
                    continue;
                }
            }
            _ => {}
        }
        runs.push(run);
    }
}

#[derive(Default)]
struct BookmarkScope {
    starts: BTreeSet<u32>,
    ends: BTreeSet<u32>,
}

fn dedupe_blocks(blocks: &mut [Block], scope: &mut BookmarkScope) {
    for block in blocks.iter_mut() {
        match block {
            Block::Para(para) => dedupe_para(para, scope),
            Block::Table(table) => {
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        dedupe_blocks(&mut cell.blocks, scope);
                    }
                }
            }
            Block::Toc(toc) => {
                for entry in &mut toc.entries {
                    dedupe_para(entry, scope);
                }
                for run in &mut toc.fallback {
                    dedupe_run(run, scope);
                }
            }
            Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => {}
        }
    }
}

fn dedupe_para(para: &mut Para, scope: &mut BookmarkScope) {
    para.content.retain_mut(|child| match child {
        ParaChild::BookmarkStart { id, .. } => scope.starts.insert(*id),
        ParaChild::BookmarkEnd { id } => scope.ends.insert(*id),
        ParaChild::Run(run) => {
            dedupe_run(run, scope);
            true
        }
        ParaChild::Hyperlink { runs, .. } => {
            for run in runs {
                dedupe_run(run, scope);
            }
            true
        }
        ParaChild::OmmlPara(_) | ParaChild::Tag(_) => true,
    });
}

fn dedupe_run(run: &mut Run, scope: &mut BookmarkScope) {
    match run {
        Run::Drawing(drawing) => {
            if let Some(text_box) =
                drawing.shape.as_mut().and_then(|shape| shape.txbx.as_mut())
            {
                dedupe_blocks(&mut text_box.blocks, scope);
            }
            if let Some(group) = drawing.group.as_mut() {
                for child in &mut group.children {
                    if let Some(text_box) = child.shape.txbx.as_mut() {
                        dedupe_blocks(&mut text_box.blocks, scope);
                    }
                }
            }
        }
        Run::Field(field) => {
            for result in &mut field.result {
                dedupe_run(result, scope);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum DocumentInvariantError {
    ZeroDrawingId,
    DuplicateDrawingId(u32),
    DecorativeDrawingHasAccessibleContent(u32),
    DuplicateBookmarkId(u32),
    DuplicateBookmarkName(EcoString),
    OrphanBookmarkEnd(u32),
    MissingBookmarkEnd(u32),
    InvalidFootnoteId(i32),
    DuplicateFootnoteId(i32),
    MissingFootnoteBody(i32),
    DuplicateAbstractNumberingId(u32),
    DuplicateNumberingId(u32),
    MissingAbstractNumberingId(u32),
    MissingNumberingId(u32),
    ResolvedFieldHasNoCache(EcoString),
    BestEffortFieldHasNoCache(EcoString),
    BestEffortFieldIsStatic(EcoString),
    UnavailableFieldHasCache(EcoString),
    UnavailableFieldIsStatic(EcoString),
    HiddenFieldHasVisibleCache(EcoString),
    DanglingInternalFieldTarget(EcoString),
}

impl Display for DocumentInvariantError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDrawingId => write!(f, "drawing ID must be greater than zero"),
            Self::DuplicateDrawingId(id) => write!(f, "duplicate drawing ID `{id}`"),
            Self::DecorativeDrawingHasAccessibleContent(id) => write!(
                f,
                "decorative drawing `{id}` also carries alternative or native text"
            ),
            Self::DuplicateBookmarkId(id) => write!(f, "duplicate bookmark ID `{id}`"),
            Self::DuplicateBookmarkName(name) => {
                write!(f, "duplicate bookmark name `{name}`")
            }
            Self::OrphanBookmarkEnd(id) => {
                write!(f, "bookmark end `{id}` has no matching start")
            }
            Self::MissingBookmarkEnd(id) => {
                write!(f, "bookmark start `{id}` has no matching end")
            }
            Self::InvalidFootnoteId(id) => {
                write!(f, "footnote ID `{id}` must be greater than zero")
            }
            Self::DuplicateFootnoteId(id) => write!(f, "duplicate footnote ID `{id}`"),
            Self::MissingFootnoteBody(id) => {
                write!(f, "footnote reference `{id}` has no matching body")
            }
            Self::DuplicateAbstractNumberingId(id) => {
                write!(f, "duplicate abstract numbering ID `{id}`")
            }
            Self::DuplicateNumberingId(id) => write!(f, "duplicate numbering ID `{id}`"),
            Self::MissingAbstractNumberingId(id) => {
                write!(f, "numbering instance references missing abstract ID `{id}`")
            }
            Self::MissingNumberingId(id) => {
                write!(f, "paragraph references missing numbering ID `{id}`")
            }
            Self::ResolvedFieldHasNoCache(instr) => {
                write!(f, "resolved field `{instr}` has no cached result")
            }
            Self::BestEffortFieldHasNoCache(instr) => {
                write!(f, "best-effort field `{instr}` has no placeholder result")
            }
            Self::BestEffortFieldIsStatic(instr) => {
                write!(f, "best-effort field `{instr}` cannot be Typst-owned and locked")
            }
            Self::UnavailableFieldHasCache(instr) => {
                write!(f, "unavailable field `{instr}` unexpectedly has a cached result")
            }
            Self::UnavailableFieldIsStatic(instr) => {
                write!(f, "unavailable field `{instr}` cannot be Typst-owned and locked")
            }
            Self::HiddenFieldHasVisibleCache(instr) => {
                write!(f, "hidden field `{instr}` carries visible cached runs")
            }
            Self::DanglingInternalFieldTarget(target) => {
                write!(f, "internal field targets missing bookmark `{target}`")
            }
        }
    }
}

impl Error for DocumentInvariantError {}

pub(crate) fn validate(document: &DocxDocument) -> Result<(), DocumentInvariantError> {
    let mut available_bookmarks = BTreeSet::new();
    collect_bookmark_names(&document.body, &mut available_bookmarks);
    for part in document.header_parts.iter().chain(&document.footer_parts) {
        collect_bookmark_names(&part.blocks, &mut available_bookmarks);
    }
    for footnote in &document.footnotes {
        collect_bookmark_names(&footnote.blocks, &mut available_bookmarks);
    }
    validate_internal_field_targets(&document.body, &available_bookmarks)?;
    for part in document.header_parts.iter().chain(&document.footer_parts) {
        validate_internal_field_targets(&part.blocks, &available_bookmarks)?;
    }
    for footnote in &document.footnotes {
        validate_internal_field_targets(&footnote.blocks, &available_bookmarks)?;
    }

    let mut state = State::default();

    for abstract_num in &document.numbering.abstracts {
        if !state.abstract_numbering_ids.insert(abstract_num.id) {
            return Err(DocumentInvariantError::DuplicateAbstractNumberingId(
                abstract_num.id,
            ));
        }
    }
    for num in &document.numbering.nums {
        if !state.numbering_ids.insert(num.num_id) {
            return Err(DocumentInvariantError::DuplicateNumberingId(num.num_id));
        }
        if !state.abstract_numbering_ids.contains(&num.abstract_id) {
            return Err(DocumentInvariantError::MissingAbstractNumberingId(
                num.abstract_id,
            ));
        }
    }
    for footnote in &document.footnotes {
        if footnote.id <= 0 {
            return Err(DocumentInvariantError::InvalidFootnoteId(footnote.id));
        }
        if !state.footnote_ids.insert(footnote.id) {
            return Err(DocumentInvariantError::DuplicateFootnoteId(footnote.id));
        }
    }

    // Bookmark identity is scoped PER PART, not document-wide: repeated page
    // furniture legitimately re-emits the same logical bookmark (same id and
    // name, from the idempotent per-`Location` allocation) once per header/
    // footer part — a labeled element shown in a running head appears in
    // every section's header part. That pattern has shipped and been verified
    // in real Word/LibreOffice; both tolerate the cross-part repetition (the
    // OOXML name-uniqueness nicety notwithstanding). A duplicate WITHIN one
    // part is the real corruption signal and stays fatal.
    state.visit_blocks(&document.body)?;
    state.finish_part()?;
    for part in document.header_parts.iter().chain(&document.footer_parts) {
        state.visit_blocks(&part.blocks)?;
        state.finish_part()?;
    }
    // All footnotes serialize into one part (word/footnotes.xml).
    for footnote in &document.footnotes {
        state.visit_blocks(&footnote.blocks)?;
    }
    state.finish_part()
}

fn validate_internal_field_targets(
    blocks: &[Block],
    bookmarks: &BTreeSet<EcoString>,
) -> Result<(), DocumentInvariantError> {
    for block in blocks {
        match block {
            Block::Para(para) => validate_para_field_targets(para, bookmarks)?,
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        validate_internal_field_targets(&cell.blocks, bookmarks)?;
                    }
                }
            }
            Block::Toc(toc) => {
                for entry in &toc.entries {
                    validate_para_field_targets(entry, bookmarks)?;
                }
                for run in &toc.fallback {
                    validate_run_field_targets(run, bookmarks)?;
                }
            }
            Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => {}
        }
    }
    Ok(())
}

fn validate_para_field_targets(
    para: &Para,
    bookmarks: &BTreeSet<EcoString>,
) -> Result<(), DocumentInvariantError> {
    for child in &para.content {
        match child {
            ParaChild::Run(run) => validate_run_field_targets(run, bookmarks)?,
            ParaChild::Hyperlink { runs, .. } => {
                for run in runs {
                    validate_run_field_targets(run, bookmarks)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_run_field_targets(
    run: &Run,
    bookmarks: &BTreeSet<EcoString>,
) -> Result<(), DocumentInvariantError> {
    match run {
        Run::Field(field) => {
            if let Some(target) = internal_bookmark_target(&field.instr)
                && !bookmarks.contains(target)
            {
                return Err(DocumentInvariantError::DanglingInternalFieldTarget(
                    target.into(),
                ));
            }
            for result in &field.result {
                validate_run_field_targets(result, bookmarks)?;
            }
        }
        Run::Drawing(drawing) => {
            if let Some(text_box) =
                drawing.shape.as_ref().and_then(|shape| shape.txbx.as_ref())
            {
                validate_internal_field_targets(&text_box.blocks, bookmarks)?;
            }
            if let Some(group) = &drawing.group {
                for child in &group.children {
                    if let Some(text_box) = &child.shape.txbx {
                        validate_internal_field_targets(&text_box.blocks, bookmarks)?;
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[derive(Default)]
struct State {
    drawing_ids: BTreeSet<u32>,
    bookmark_start_ids: BTreeSet<u32>,
    bookmark_end_ids: BTreeSet<u32>,
    bookmark_names: BTreeSet<EcoString>,
    footnote_ids: BTreeSet<i32>,
    abstract_numbering_ids: BTreeSet<u32>,
    numbering_ids: BTreeSet<u32>,
}

impl State {
    fn visit_blocks(&mut self, blocks: &[Block]) -> Result<(), DocumentInvariantError> {
        for block in blocks {
            match block {
                Block::Para(para) => self.visit_para(para)?,
                Block::Table(table) => {
                    for row in &table.rows {
                        for cell in &row.cells {
                            self.visit_blocks(&cell.blocks)?;
                        }
                    }
                }
                Block::Toc(toc) => {
                    for entry in &toc.entries {
                        self.visit_para(entry)?;
                    }
                    for run in &toc.fallback {
                        self.visit_run(run)?;
                    }
                }
                Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => {}
            }
        }
        Ok(())
    }

    fn visit_para(&mut self, para: &Para) -> Result<(), DocumentInvariantError> {
        if let Some((num_id, _)) = para.props.num
            && !self.numbering_ids.contains(&num_id)
        {
            return Err(DocumentInvariantError::MissingNumberingId(num_id));
        }
        for child in &para.content {
            match child {
                ParaChild::Run(run) => self.visit_run(run)?,
                ParaChild::Hyperlink { runs, .. } => {
                    for run in runs {
                        self.visit_run(run)?;
                    }
                }
                ParaChild::BookmarkStart { id, name } => {
                    if !self.bookmark_start_ids.insert(*id) {
                        return Err(DocumentInvariantError::DuplicateBookmarkId(*id));
                    }
                    if !self.bookmark_names.insert(name.clone()) {
                        return Err(DocumentInvariantError::DuplicateBookmarkName(
                            name.clone(),
                        ));
                    }
                }
                ParaChild::BookmarkEnd { id } => {
                    if !self.bookmark_end_ids.insert(*id) {
                        return Err(DocumentInvariantError::DuplicateBookmarkId(*id));
                    }
                }
                ParaChild::OmmlPara(_) | ParaChild::Tag(_) => {}
            }
        }
        Ok(())
    }

    fn visit_run(&mut self, run: &Run) -> Result<(), DocumentInvariantError> {
        match run {
            Run::FootnoteRef { id, .. } => {
                if !self.footnote_ids.contains(id) {
                    return Err(DocumentInvariantError::MissingFootnoteBody(*id));
                }
            }
            Run::Drawing(drawing) => {
                self.register_drawing_id(drawing.docpr_id)?;
                if drawing.decorative
                    && (drawing.alt.is_some() || drawing.has_native_text())
                {
                    return Err(
                        DocumentInvariantError::DecorativeDrawingHasAccessibleContent(
                            drawing.docpr_id,
                        ),
                    );
                }
                if let Some(text_box) =
                    drawing.shape.as_ref().and_then(|shape| shape.txbx.as_ref())
                {
                    self.visit_blocks(&text_box.blocks)?;
                }
                if let Some(group) = &drawing.group {
                    for child in &group.children {
                        if let Some(text_box) = &child.shape.txbx {
                            self.visit_blocks(&text_box.blocks)?;
                        }
                    }
                }
            }
            Run::Field(field) => {
                if field.display == FieldDisplay::Hidden && !field.result.is_empty() {
                    return Err(DocumentInvariantError::HiddenFieldHasVisibleCache(
                        field.instr.clone(),
                    ));
                }
                match field.cache_status {
                    FieldCacheStatus::Resolved if field.result.is_empty() => {
                        return Err(DocumentInvariantError::ResolvedFieldHasNoCache(
                            field.instr.clone(),
                        ));
                    }
                    FieldCacheStatus::BestEffort if field.result.is_empty() => {
                        return Err(DocumentInvariantError::BestEffortFieldHasNoCache(
                            field.instr.clone(),
                        ));
                    }
                    FieldCacheStatus::BestEffort if field.mode == FieldMode::Static => {
                        return Err(DocumentInvariantError::BestEffortFieldIsStatic(
                            field.instr.clone(),
                        ));
                    }
                    FieldCacheStatus::Unavailable if !field.result.is_empty() => {
                        return Err(DocumentInvariantError::UnavailableFieldHasCache(
                            field.instr.clone(),
                        ));
                    }
                    FieldCacheStatus::Unavailable if field.mode == FieldMode::Static => {
                        return Err(DocumentInvariantError::UnavailableFieldIsStatic(
                            field.instr.clone(),
                        ));
                    }
                    _ => {}
                }
                for result in &field.result {
                    self.visit_run(result)?;
                }
            }
            Run::Text { .. }
            | Run::Break { .. }
            | Run::PageBreak
            | Run::ColumnBreak
            | Run::Tab
            | Run::FillTab
            | Run::FootnoteRefMark
            | Run::OmmlInline(_) => {}
        }
        Ok(())
    }

    fn register_drawing_id(&mut self, id: u32) -> Result<(), DocumentInvariantError> {
        if id == 0 {
            return Err(DocumentInvariantError::ZeroDrawingId);
        }
        if !self.drawing_ids.insert(id) {
            return Err(DocumentInvariantError::DuplicateDrawingId(id));
        }
        Ok(())
    }

    /// Checks start/end pairing for the part walked so far and resets the
    /// bookmark scope for the next part (see `validate` for why bookmark
    /// identity is per-part while every other id space stays document-wide).
    fn finish_part(&mut self) -> Result<(), DocumentInvariantError> {
        if let Some(id) =
            self.bookmark_end_ids.difference(&self.bookmark_start_ids).next()
        {
            return Err(DocumentInvariantError::OrphanBookmarkEnd(*id));
        }
        if let Some(id) =
            self.bookmark_start_ids.difference(&self.bookmark_end_ids).next()
        {
            return Err(DocumentInvariantError::MissingBookmarkEnd(*id));
        }
        self.bookmark_start_ids.clear();
        self.bookmark_end_ids.clear();
        self.bookmark_names.clear();
        Ok(())
    }
}

#[cfg(test)]
mod dangling_field_tests {
    use super::*;
    use crate::dom::{Field, ParaProps, RunProps};

    fn reference_paragraph(target: &str) -> Block {
        Block::Para(Para {
            props: ParaProps::default(),
            content: vec![ParaChild::Run(Run::Field(Field {
                instr: format!(" REF {target} \\h ").into(),
                result: vec![Run::Text {
                    props: RunProps::default(),
                    text: "cached 7".into(),
                }],
                mode: FieldMode::Live,
                display: FieldDisplay::Visible,
                cache_status: FieldCacheStatus::Resolved,
            }))],
        })
    }

    #[test]
    fn dangling_internal_field_becomes_cached_text() {
        let mut body = vec![reference_paragraph("_MissingNumber")];
        assert_eq!(
            validate_internal_field_targets(&body, &BTreeSet::new()),
            Err(DocumentInvariantError::DanglingInternalFieldTarget(
                "_MissingNumber".into()
            ))
        );
        let missing =
            fallback_dangling_internal_fields(&mut body, &mut [], &mut [], &mut []);

        assert_eq!(missing, [EcoString::from("_MissingNumber")]);
        let Block::Para(para) = &body[0] else { panic!("paragraph") };
        assert!(matches!(
            para.content.as_slice(),
            [ParaChild::Run(Run::Text { text, .. })] if text == "cached 7"
        ));
    }

    #[test]
    fn internal_field_with_target_remains_live() {
        let mut body = vec![
            Block::Para(Para {
                props: ParaProps::default(),
                content: vec![
                    ParaChild::BookmarkStart { id: 1, name: "_TargetNumber".into() },
                    ParaChild::BookmarkEnd { id: 1 },
                ],
            }),
            reference_paragraph("_TargetNumber"),
        ];
        let missing =
            fallback_dangling_internal_fields(&mut body, &mut [], &mut [], &mut []);

        assert!(missing.is_empty());
        let Block::Para(para) = &body[1] else { panic!("paragraph") };
        assert!(matches!(para.content.as_slice(), [ParaChild::Run(Run::Field(_))]));
    }

    #[test]
    fn missing_number_target_retargets_locked_full_bookmark() {
        let mut body = vec![
            Block::Para(Para {
                props: ParaProps::default(),
                content: vec![
                    ParaChild::BookmarkStart { id: 1, name: "_Target".into() },
                    ParaChild::BookmarkEnd { id: 1 },
                ],
            }),
            reference_paragraph("_TargetNumber"),
        ];

        let missing =
            fallback_dangling_internal_fields(&mut body, &mut [], &mut [], &mut []);
        assert!(missing.is_empty());
        let Block::Para(para) = &body[1] else { panic!("paragraph") };
        let [ParaChild::Run(Run::Field(field))] = para.content.as_slice() else {
            panic!("locked field")
        };
        assert_eq!(field.instr, " REF _Target \\h ");
        assert_eq!(field.mode, FieldMode::Static);
        assert!(matches!(
            field.result.as_slice(),
            [Run::Text { text, .. }] if text == "cached 7"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{Drawing, Field};

    #[test]
    fn duplicate_drawing_ids_are_rejected() {
        let mut state = State::default();
        assert_eq!(state.register_drawing_id(7), Ok(()));
        assert_eq!(
            state.register_drawing_id(7),
            Err(DocumentInvariantError::DuplicateDrawingId(7))
        );
    }

    #[test]
    fn unpaired_bookmarks_are_rejected() {
        let mut state = State::default();
        state.bookmark_start_ids.insert(3);
        assert_eq!(
            state.finish_part(),
            Err(DocumentInvariantError::MissingBookmarkEnd(3))
        );
    }

    #[test]
    fn decorative_drawing_cannot_also_have_alt_text() {
        let mut state = State::default();
        let run = Run::Drawing(Drawing {
            rel: EcoString::new(),
            svg_rel: None,
            compatibility_split_ids: None,
            w_emu: 1,
            h_emu: 1,
            source_offset_emu: [0, 0],
            alt: Some("meaningful".into()),
            decorative: true,
            docpr_id: 9,
            name: "Shape 9".into(),
            anchor: None,
            shape: None,
            group: None,
        });
        assert_eq!(
            state.visit_run(&run),
            Err(DocumentInvariantError::DecorativeDrawingHasAccessibleContent(9))
        );
    }

    #[test]
    fn resolved_field_requires_a_cached_result() {
        let mut state = State::default();
        let run = Run::Field(Field {
            instr: " PAGE ".into(),
            result: Vec::new(),
            mode: FieldMode::Live,
            display: FieldDisplay::Visible,
            cache_status: FieldCacheStatus::Resolved,
        });
        assert_eq!(
            state.visit_run(&run),
            Err(DocumentInvariantError::ResolvedFieldHasNoCache(" PAGE ".into()))
        );
    }

    #[test]
    fn unavailable_field_cannot_be_typst_owned() {
        let mut state = State::default();
        let run = Run::Field(Field {
            instr: " REF _Ref1 ".into(),
            result: Vec::new(),
            mode: FieldMode::Static,
            display: FieldDisplay::Visible,
            cache_status: FieldCacheStatus::Unavailable,
        });
        assert_eq!(
            state.visit_run(&run),
            Err(DocumentInvariantError::UnavailableFieldIsStatic(" REF _Ref1 ".into()))
        );
    }

    #[test]
    fn best_effort_field_requires_a_live_placeholder() {
        let mut state = State::default();
        let run = Run::Field(Field {
            instr: " PAGEREF _Ref1 ".into(),
            result: Vec::new(),
            mode: FieldMode::Live,
            display: FieldDisplay::Visible,
            cache_status: FieldCacheStatus::BestEffort,
        });
        assert_eq!(
            state.visit_run(&run),
            Err(DocumentInvariantError::BestEffortFieldHasNoCache(
                " PAGEREF _Ref1 ".into()
            ))
        );
    }
}
