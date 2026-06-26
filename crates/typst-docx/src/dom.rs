//! The typed DOCX intermediate representation.

use std::sync::Arc;

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, Output, StyleChain, Target};
use typst_library::introspection::{Introspector, Tag};
use typst_library::model::{Document, DocumentInfo};

use crate::introspect::DocxIntrospector;
use crate::package::Rels;

/// Output document: realized native tree lowered to the OOXML IR + metadata +
/// introspector.
pub struct DocxDocument {
    pub(crate) info: DocumentInfo,
    pub(crate) body: Vec<Block>,
    pub(crate) sect: SectPr,
    pub(crate) footnotes: Vec<Footnote>,
    pub(crate) numbering: NumberingTable,
    pub(crate) media: Vec<MediaPart>,
    pub(crate) doc_rels: Rels,
    pub(crate) bookmarks: BookmarkTable,
    pub(crate) max_heading_level: u8,
    pub(crate) uses_fields: bool,
    pub(crate) uses_math: bool,
    pub(crate) introspector: Arc<DocxIntrospector>,
}

impl DocxDocument {
    pub fn info(&self) -> &DocumentInfo {
        &self.info
    }
}

impl Document for DocxDocument {
    fn info(&self) -> &DocumentInfo {
        &self.info
    }
}

impl Output for DocxDocument {
    fn introspector(&self) -> &dyn Introspector {
        self.introspector.as_ref()
    }

    fn target() -> Target {
        Target::Docx
    }

    fn create(
        engine: &mut Engine,
        content: &Content,
        styles: StyleChain,
    ) -> SourceResult<Self> {
        crate::docx_document(engine, content, styles)
    }
}

/// A block-level body item.
pub enum Block {
    Para(Para),
    Table(Tbl),
    /// A non-final section break carrying its own `SectPr`.
    SectionBreak(SectPr),
    /// Introspection tag passthrough for the introspector + bookmarks.
    Tag(Tag),
}

/// A paragraph.
pub struct Para {
    pub props: ParaProps,
    pub content: Vec<ParaChild>,
}

/// Paragraph-level content.
pub enum ParaChild {
    Run(Run),
    /// A display equation `<m:oMathPara>` (serialized XML).
    OmmlPara(String),
    /// `<w:hyperlink r:id|w:anchor>` wrapping runs.
    Hyperlink { rel: Option<EcoString>, anchor: Option<EcoString>, runs: Vec<Run> },
    BookmarkStart { id: u32, name: EcoString },
    BookmarkEnd { id: u32 },
    Tag(Tag),
}

/// A run-level item.
pub enum Run {
    Text { props: RunProps, text: EcoString },
    Break,
    PageBreak,
    Tab,
    FootnoteRef { props: RunProps, id: i32 },
    Drawing(Drawing),
    /// An inline equation `<m:oMath>` (serialized XML).
    OmmlInline(String),
    Field(Field),
}

/// Flattened character formatting → `<w:rPr>`.
#[derive(Default, Clone, PartialEq)]
pub struct RunProps {
    pub style: Option<EcoString>,
    pub font: Option<EcoString>,
    pub bold: bool,
    pub italic: bool,
    pub smallcaps: bool,
    pub strike: bool,
    pub color: Option<[u8; 3]>,
    pub tracking: Option<i32>,
    pub size_half_pt: Option<u32>,
    pub shd_fill: Option<[u8; 3]>,
    pub underline: bool,
    pub vert_align: Option<VertAlign>,
    pub lang: Option<EcoString>,
}

#[derive(Copy, Clone, PartialEq)]
pub enum VertAlign {
    Super,
    Sub,
}

/// Paragraph formatting → `<w:pPr>`.
#[derive(Default, Clone)]
pub struct ParaProps {
    pub style: Option<EcoString>,
    pub keep_next: bool,
    pub num: Option<(u32, u8)>,
    pub spacing: Option<Spacing>,
    pub ind: Option<Indent>,
    pub jc: Option<Jc>,
    pub outline_lvl: Option<u8>,
    pub tabs: Vec<TabStop>,
}

#[derive(Copy, Clone)]
pub enum Jc {
    Start,
    End,
    Center,
    Both,
}

#[derive(Default, Clone)]
pub struct Spacing {
    pub before: Option<i32>,
    pub after: Option<i32>,
    pub line: Option<i32>,
    pub line_rule_auto: bool,
}

#[derive(Default, Clone)]
pub struct Indent {
    pub left: Option<i32>,
    pub right: Option<i32>,
    pub first_line: Option<i32>,
    pub hanging: Option<i32>,
}

#[derive(Clone)]
pub struct TabStop {
    pub val: TabAlign,
    pub leader: Option<TabLeader>,
    pub pos: i32,
}

#[derive(Copy, Clone)]
pub enum TabAlign {
    Start,
    End,
    Center,
}

#[derive(Copy, Clone)]
pub enum TabLeader {
    Dot,
    Hyphen,
    Underscore,
}

/// A complex field code run sequence.
pub struct Field {
    pub instr: EcoString,
    pub result: Vec<Run>,
    pub dirty: bool,
}

/// An inline image.
pub struct Drawing {
    pub rel: EcoString,
    pub w_emu: i64,
    pub h_emu: i64,
    pub alt: Option<EcoString>,
    pub docpr_id: u32,
    pub name: EcoString,
}

/// A table.
pub struct Tbl {
    pub props: TblProps,
    pub grid: Vec<i32>,
    pub rows: Vec<Row>,
}

#[derive(Default)]
pub struct TblProps {
    pub width_dxa: Option<i32>,
    pub style: Option<EcoString>,
}

pub struct Row {
    pub header: bool,
    pub cant_split: bool,
    pub height: Option<RowHeight>,
    pub cells: Vec<Cell>,
}

pub struct RowHeight {
    pub val: i32,
    pub exact: bool,
}

pub struct Cell {
    pub w_dxa: Option<i32>,
    pub grid_span: u32,
    pub v_merge: Option<VMerge>,
    pub borders: CellBorders,
    pub shd_fill: Option<[u8; 3]>,
    pub valign: Option<VAlign>,
    pub blocks: Vec<Block>,
}

#[derive(Copy, Clone)]
pub enum VMerge {
    Restart,
    Continue,
}

#[derive(Copy, Clone)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Default)]
pub struct CellBorders {
    pub top: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
    pub right: Option<Border>,
}

#[derive(Copy, Clone)]
pub struct Border {
    /// Width in eighths of a point.
    pub sz: u32,
    pub color: [u8; 3],
}

/// Page setup → `<w:sectPr>`.
#[derive(Clone)]
pub struct SectPr {
    /// Page width in twips.
    pub page_w: i32,
    /// Page height in twips.
    pub page_h: i32,
    pub landscape: bool,
    pub margin_top: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub margin_right: i32,
    pub header: i32,
    pub footer: i32,
    pub columns: u32,
}

impl Default for SectPr {
    fn default() -> Self {
        // US Letter, 1-inch margins (in twips: 1 inch = 1440).
        Self {
            page_w: 12240,
            page_h: 15840,
            landscape: false,
            margin_top: 1440,
            margin_bottom: 1440,
            margin_left: 1440,
            margin_right: 1440,
            header: 720,
            footer: 720,
            columns: 1,
        }
    }
}

/// One footnote entry routed to `footnotes.xml`.
pub struct Footnote {
    pub id: i32,
    pub blocks: Vec<Block>,
}

/// A media part to be embedded in `word/media/`.
pub struct MediaPart {
    pub rel: EcoString,
    pub part_name: EcoString,
    pub ext: EcoString,
    pub bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Numbering (lists / enums).
// ---------------------------------------------------------------------------

/// The numbering table for `numbering.xml`.
#[derive(Default)]
pub struct NumberingTable {
    /// Distinct abstract numbering definitions.
    pub abstracts: Vec<AbstractNum>,
    /// Concrete `<w:num>` instances mapping a numId to an abstractNumId.
    pub nums: Vec<NumInstance>,
}

pub struct AbstractNum {
    pub id: u32,
    pub levels: Vec<ListLevel>,
    pub multilevel: MultiLevelType,
}

pub struct NumInstance {
    pub num_id: u32,
    pub abstract_id: u32,
    /// Optional level-0 start override.
    pub start_override: Option<u64>,
}

/// The numbering shape passed to `register_list`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ListSpec {
    pub levels: Vec<ListLevel>,
    pub multilevel: MultiLevelType,
    pub restart_at_1: bool,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ListLevel {
    pub num_fmt: NumFmt,
    pub lvl_text: EcoString,
    pub start: u64,
    pub ind_left: i32,
    pub ind_hanging: i32,
    pub bullet_font: Option<EcoString>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum NumFmt {
    Bullet,
    Decimal,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    None,
}

impl NumFmt {
    pub fn as_str(self) -> &'static str {
        match self {
            NumFmt::Bullet => "bullet",
            NumFmt::Decimal => "decimal",
            NumFmt::LowerLetter => "lowerLetter",
            NumFmt::UpperLetter => "upperLetter",
            NumFmt::LowerRoman => "lowerRoman",
            NumFmt::UpperRoman => "upperRoman",
            NumFmt::None => "none",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum MultiLevelType {
    SingleLevel,
    Multilevel,
    HybridMultilevel,
}

impl MultiLevelType {
    pub fn as_str(self) -> &'static str {
        match self {
            MultiLevelType::SingleLevel => "singleLevel",
            MultiLevelType::Multilevel => "multilevel",
            MultiLevelType::HybridMultilevel => "hybridMultilevel",
        }
    }
}

// ---------------------------------------------------------------------------
// Bookmarks.
// ---------------------------------------------------------------------------

use rustc_hash::FxHashMap;
use typst_library::introspection::Location;

/// Maps `Location`s to their assigned bookmark `(name, id)`.
#[derive(Default)]
pub struct BookmarkTable {
    pub by_location: FxHashMap<Location, (EcoString, u32)>,
}
