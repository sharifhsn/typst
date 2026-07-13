//! A minimal vendored serde mirror of the Pandoc JSON AST (api `[1,23,1,1]`).
//!
//! We only ever *construct + serialize once* (~25 variants), never deserialize
//! or mutate, so we vendor an adjacently-tagged (`{"t":..,"c":..}`) enum rather
//! than depend on `pandoc_types` (filter/visitor-shaped, 3 yrs stale). serde
//! renders tuple variants as positional JSON arrays — exactly pandoc's
//! encoding; nullary unit variants drop the `c` key automatically, which is a
//! producer obligation pandoc tolerates but never emits with a spurious `c`.
//!
//! Every shape here was ground-truthed against pandoc 3.9.0.2; see the design
//! doc §2 (FORMAT REFERENCE) and §4.5 (vendored types).
//!
//! Some variants/constructors are not yet emitted by the phase-1 foundation
//! (the stub mappers rasterize instead); they are part of the complete vendored
//! mirror that phase-2 mappers will use, so dead-code is allowed module-wide.
//!
//! The variant names (and their shared prefixes / enum-name suffixes) are
//! Pandoc's own constructor names, ground-truthed against the `{"t":..}` JSON
//! encoding — they MUST match verbatim, so the clippy naming/size lints that
//! would otherwise want them renamed are silenced module-wide.
#![allow(dead_code)]
#![allow(clippy::enum_variant_names, clippy::large_enum_variant)]

use std::collections::BTreeMap;

use serde::Serialize;

/// The pandoc-api-version pandoc 3.9 writes. The reader's compatibility gate
/// compares only `[major, minor]` (= `1.23`); a *minor* mismatch is a hard
/// `exit 64` with no warn band, so keep this pinnable (one-line bump) + CI-gated.
pub const PANDOC_API_VERSION: [u32; 4] = [1, 23, 1, 1];

/// The top-level document: `{"pandoc-api-version", "meta", "blocks"}`.
#[derive(Serialize)]
pub struct Pandoc {
    #[serde(rename = "pandoc-api-version")]
    pub pandoc_api_version: [u32; 4],
    pub meta: Meta,
    pub blocks: Vec<Block>,
}

/// Document metadata; `{}` when empty (not `null`, not omitted).
pub type Meta = BTreeMap<String, MetaValue>;

/// `Attr = (id, [class], [(key,value)])` → `[identifier, [classes], [[k,v],…]]`.
/// Empty = `["",[],[]]`.
pub type Attr = (String, Vec<String>, Vec<(String, String)>);

/// `Target = (url, title)`; title is `""` when absent.
pub type Target = (String, String);

/// An empty [`Attr`] (`["",[],[]]`).
pub fn empty_attr() -> Attr {
    (String::new(), Vec::new(), Vec::new())
}

/// An [`Attr`] carrying only an identifier.
pub fn id_attr(id: impl Into<String>) -> Attr {
    (id.into(), Vec::new(), Vec::new())
}

/// An [`Attr`] carrying only a single class.
pub fn class_attr(class: impl Into<String>) -> Attr {
    (String::new(), vec![class.into()], Vec::new())
}

#[derive(Serialize)]
#[serde(tag = "t", content = "c")]
pub enum MetaValue {
    MetaInlines(Vec<Inline>),
    MetaList(Vec<MetaValue>),
    MetaBool(bool),
    MetaMap(BTreeMap<String, MetaValue>),
    MetaString(String),
    MetaBlocks(Vec<Block>),
}

#[derive(Serialize)]
#[serde(tag = "t", content = "c")]
pub enum Inline {
    Str(String),
    Emph(Vec<Inline>),
    Underline(Vec<Inline>),
    Strong(Vec<Inline>),
    Strikeout(Vec<Inline>),
    Superscript(Vec<Inline>),
    Subscript(Vec<Inline>),
    SmallCaps(Vec<Inline>),
    Quoted(QuoteType, Vec<Inline>),
    Cite(Vec<Citation>, Vec<Inline>),
    Code(Attr, String),
    Space,
    SoftBreak,
    LineBreak,
    Math(MathType, String),
    RawInline(String, String),
    Link(Attr, Vec<Inline>, Target),
    Image(Attr, Vec<Inline>, Target),
    Note(Vec<Block>),
    Span(Attr, Vec<Inline>),
}

#[derive(Serialize)]
#[serde(tag = "t", content = "c")]
pub enum Block {
    Plain(Vec<Inline>),
    Para(Vec<Inline>),
    CodeBlock(Attr, String),
    RawBlock(String, String),
    BlockQuote(Vec<Block>),
    OrderedList(ListAttributes, Vec<Vec<Block>>),
    BulletList(Vec<Vec<Block>>),
    DefinitionList(Vec<(Vec<Inline>, Vec<Vec<Block>>)>),
    Header(i32, Attr, Vec<Inline>),
    HorizontalRule,
    Table(Attr, Caption, Vec<ColSpec>, TableHead, Vec<TableBody>, TableFoot),
    Figure(Attr, Caption, Vec<Block>),
    Div(Attr, Vec<Block>),
}

#[derive(Serialize)]
#[serde(tag = "t")]
pub enum MathType {
    InlineMath,
    DisplayMath,
}

#[derive(Serialize)]
#[serde(tag = "t")]
pub enum QuoteType {
    SingleQuote,
    DoubleQuote,
}

pub type ListAttributes = (i32, ListNumberStyle, ListNumberDelim);

#[derive(Serialize)]
#[serde(tag = "t")]
pub enum ListNumberStyle {
    DefaultStyle,
    Example,
    Decimal,
    LowerRoman,
    UpperRoman,
    LowerAlpha,
    UpperAlpha,
}

#[derive(Serialize)]
#[serde(tag = "t")]
pub enum ListNumberDelim {
    DefaultDelim,
    Period,
    OneParen,
    TwoParens,
}

/// `Caption = [ShortCaption?, [Block]]`; `None` serializes as `null`.
#[derive(Serialize)]
pub struct Caption(pub Option<Vec<Inline>>, pub Vec<Block>);

#[derive(Serialize)]
#[serde(tag = "t")]
pub enum Alignment {
    AlignLeft,
    AlignRight,
    AlignCenter,
    AlignDefault,
}

/// `ColWidth(0.25)` → `{"t":"ColWidth","c":0.25}`; `ColWidthDefault` →
/// `{"t":"ColWidthDefault"}` (the unit variant drops `c`). Validated
/// byte-canonical through `pandoc -f json -t json`.
#[derive(Serialize)]
#[serde(tag = "t", content = "c")]
pub enum ColWidth {
    ColWidth(f64),
    ColWidthDefault,
}

pub type ColSpec = (Alignment, ColWidth);

#[derive(Serialize)]
pub struct TableHead(pub Attr, pub Vec<Row>);

#[derive(Serialize)]
pub struct TableFoot(pub Attr, pub Vec<Row>);

#[derive(Serialize)]
pub struct TableBody(
    pub Attr,
    pub i32, // RowHeadColumns
    pub Vec<Row>,
    pub Vec<Row>,
);

#[derive(Serialize)]
pub struct Row(pub Attr, pub Vec<Cell>);

#[derive(Serialize)]
pub struct Cell(
    pub Attr,
    pub Alignment,
    pub i32, // RowSpan
    pub i32, // ColSpan
    pub Vec<Block>,
);

#[derive(Serialize)]
pub struct Citation {
    #[serde(rename = "citationId")]
    pub id: String,
    #[serde(rename = "citationPrefix")]
    pub prefix: Vec<Inline>,
    #[serde(rename = "citationSuffix")]
    pub suffix: Vec<Inline>,
    #[serde(rename = "citationMode")]
    pub mode: CitationMode,
    #[serde(rename = "citationNoteNum")]
    pub note_num: i32,
    #[serde(rename = "citationHash")]
    pub hash: i32,
}

#[derive(Serialize)]
#[serde(tag = "t")]
pub enum CitationMode {
    AuthorInText,
    SuppressAuthor,
    NormalCitation,
}
