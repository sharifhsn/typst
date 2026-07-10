//! Format-specific invariants for the finalized DOCX IR.
//!
//! OPC validates package mechanics. These checks cover WordprocessingML IDs
//! whose meaning spans several XML elements or parts and therefore cannot be
//! inferred by the format-neutral package writer.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Display, Formatter};

use ecow::EcoString;

use crate::dom::{Block, DocxDocument, Para, ParaChild, Run};

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
        }
    }
}

impl Error for DocumentInvariantError {}

pub(crate) fn validate(document: &DocxDocument) -> Result<(), DocumentInvariantError> {
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

    state.visit_blocks(&document.body)?;
    for part in document.header_parts.iter().chain(&document.footer_parts) {
        state.visit_blocks(&part.blocks)?;
    }
    for footnote in &document.footnotes {
        state.visit_blocks(&footnote.blocks)?;
    }
    state.finish()
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
                for result in &field.result {
                    self.visit_run(result)?;
                }
            }
            Run::Text { .. }
            | Run::Break
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

    fn finish(self) -> Result<(), DocumentInvariantError> {
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Drawing;

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
        assert_eq!(state.finish(), Err(DocumentInvariantError::MissingBookmarkEnd(3)));
    }

    #[test]
    fn decorative_drawing_cannot_also_have_alt_text() {
        let mut state = State::default();
        let run = Run::Drawing(Drawing {
            rel: EcoString::new(),
            svg_rel: None,
            w_emu: 1,
            h_emu: 1,
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
}
