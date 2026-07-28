//! The PresentationML IR: PowerPoint's own vocabulary, in Rust.
//!
//! Deliberately close to the XML — this layer's job is to *read* faithfully,
//! not to decide anything. Every mapping judgement happens one layer up in
//! `mappers`, so that a wrong judgement can be changed without touching the
//! parser and a parser fix cannot silently change output policy.
//!
//! The one structural liberty taken here: a shape tree is kept as a tree
//! (`Shape::Group` nests), because PowerPoint group transforms compose and
//! flattening them at parse time would lose the information needed to
//! compose them correctly.

use ecow::EcoString;

/// EMU (English Metric Units), PowerPoint's native length. 914400 per inch.
pub type Emu = i64;

#[derive(Debug, Default)]
pub struct PmlPackage {
    /// Slides in presentation order (`p:sldIdLst`), not archive order.
    pub slides: Vec<Slide>,
    pub layouts: Vec<SlideLayout>,
    pub masters: Vec<SlideMaster>,
    pub theme: Theme,
    pub size: SlideSize,
    /// Anything the reader recognised but this crate has no home for, keyed
    /// so the report can name it once.
    pub unsupported: Vec<EcoString>,
}

#[derive(Debug, Clone, Copy)]
pub struct SlideSize {
    pub cx: Emu,
    pub cy: Emu,
}

impl Default for SlideSize {
    /// PowerPoint's own default, 10in × 7.5in (4:3). Used only when
    /// `p:sldSz` is missing, which makes the deck unopenable in PowerPoint
    /// anyway — but an importer should still produce something.
    fn default() -> Self {
        Self { cx: 9144000, cy: 6858000 }
    }
}

#[derive(Debug, Default)]
pub struct Slide {
    /// The part this slide was read from, which is what a same-deck
    /// hyperlink's relationship resolves to.
    pub part: EcoString,
    pub shapes: Vec<Shape>,
    /// Index into [`PmlPackage::layouts`], resolved from the slide's own
    /// relationships.
    pub layout: Option<usize>,
    /// `p:notesSlide` body text, already flattened to plain text — Typst has
    /// no notes element, so nothing richer would survive anyway.
    pub notes: Option<EcoString>,
    pub bg: Option<Fill>,
    /// `p:sld/@show="0"` — a slide hidden from the presentation.
    pub hidden: bool,
    /// `@showMasterSp="0"` — suppress the master's own decoration behind this
    /// slide. Phrased negatively so `false` is PowerPoint's default and
    /// `derive(Default)` stays honest: a deck's logo appears on every slide
    /// precisely because no slide mentions it.
    pub hide_master_shapes: bool,
}

#[derive(Debug, Default)]
pub struct SlideLayout {
    pub name: EcoString,
    pub shapes: Vec<Shape>,
    pub master: Option<usize>,
    pub bg: Option<Fill>,
    pub hide_master_shapes: bool,
}

#[derive(Debug, Default)]
pub struct SlideMaster {
    pub shapes: Vec<Shape>,
    pub bg: Option<Fill>,
    /// `p:clrMap` — which theme slot each semantic colour name resolves to.
    pub color_map: ColorMap,
    /// `p:txStyles` — the master's title/body/other list styles, the last
    /// stop in the inheritance chain for text properties.
    pub text_styles: TextStyles,
}

/// `p:clrMap`: maps `bg1`/`tx1`/`bg2`/`tx2` onto theme slots (`lt1`, `dk1`…).
///
/// Without this a `schemeClr val="tx1"` cannot be resolved: the mapping is
/// per-master and decks routinely swap light and dark.
#[derive(Debug, Clone)]
pub struct ColorMap {
    pub bg1: EcoString,
    pub tx1: EcoString,
    pub bg2: EcoString,
    pub tx2: EcoString,
}

impl Default for ColorMap {
    fn default() -> Self {
        Self {
            bg1: "lt1".into(),
            tx1: "dk1".into(),
            bg2: "lt2".into(),
            tx2: "dk2".into(),
        }
    }
}

/// `a:clrScheme` from `theme1.xml`, by slot name.
#[derive(Debug, Default, Clone)]
pub struct Theme {
    pub colors: Vec<(EcoString, [u8; 3])>,
    /// `a:fontScheme` major (headings) and minor (body) latin typefaces.
    pub major_font: Option<EcoString>,
    pub minor_font: Option<EcoString>,
}

impl Theme {
    pub fn color(&self, slot: &str) -> Option<[u8; 3]> {
        self.colors.iter().find(|(name, _)| name == slot).map(|(_, rgb)| *rgb)
    }
}

/// The master's `p:txStyles`, one entry per outline level.
#[derive(Debug, Default, Clone)]
pub struct TextStyles {
    pub title: Vec<LevelStyle>,
    pub body: Vec<LevelStyle>,
    pub other: Vec<LevelStyle>,
}

/// One `a:lvlNpPr`: the defaults every paragraph at that outline level gets.
#[derive(Debug, Default, Clone)]
pub struct LevelStyle {
    pub run: RunProps,
    pub para: ParaProps,
}

#[derive(Debug)]
pub enum Shape {
    Text(TextShape),
    Picture(Picture),
    Table(Table),
    Group(Group),
    /// A `p:cxnSp`. Geometrically a shape; semantically a line between two
    /// other shapes, and the "between" half has no Typst counterpart.
    Connector(TextShape),
    /// A chart, whose *cached* data can still be recovered as a table.
    Chart {
        rel_id: EcoString,
        xfrm: Option<Xfrm>,
    },
    /// Recognised, and deliberately not mapped. The payload names it for the
    /// report ("SmartArt diagram", "embedded OLE object").
    Unsupported {
        kind: EcoString,
        xfrm: Option<Xfrm>,
    },
}

#[derive(Debug, Default)]
pub struct Group {
    pub xfrm: Option<Xfrm>,
    /// A group states *two* rectangles: where it sits (`a:off`/`a:ext`) and
    /// what coordinate space its children are authored in
    /// (`a:chOff`/`a:chExt`). Children must be mapped through the ratio.
    pub child_off: Option<(Emu, Emu)>,
    pub child_ext: Option<(Emu, Emu)>,
    pub shapes: Vec<Shape>,
}

#[derive(Debug, Default)]
pub struct TextShape {
    pub xfrm: Option<Xfrm>,
    pub geom: Option<Geometry>,
    pub fill: Option<Fill>,
    pub line: Option<Line>,
    pub placeholder: Option<Placeholder>,
    pub paras: Vec<Para>,
    /// `a:bodyPr/@anchor` — vertical anchoring of the text in its box.
    pub anchor: Option<EcoString>,
    /// `a:bodyPr` insets, defaulting to PowerPoint's 0.1in/0.05in.
    pub insets: Insets,
    /// The shape carries a `p:nvSpPr/p:cNvSpPr/@txBox="1"` marker: it is a
    /// plain text box rather than a shape that happens to hold text.
    pub is_text_box: bool,
    /// `a:lstStyle` on this shape's own `p:txBody`.
    ///
    /// On a *layout's* placeholder this is the middle link of the text
    /// inheritance chain — the level defaults that sit between the slide's own
    /// properties and the master's `p:txStyles`. It is where a template puts
    /// "the title on this layout is right-aligned and 54pt", and reading only
    /// the master leaves every such slide left-aligned at the wrong size.
    pub list_style: Vec<LevelStyle>,
}

#[derive(Debug, Clone, Copy)]
pub struct Insets {
    pub l: Emu,
    pub t: Emu,
    pub r: Emu,
    pub b: Emu,
}

impl Default for Insets {
    fn default() -> Self {
        Self { l: 91440, t: 45720, r: 91440, b: 45720 }
    }
}

#[derive(Debug, Default)]
pub struct Picture {
    pub xfrm: Option<Xfrm>,
    /// Relationship id of the image part, already namespaced per source part.
    pub rel_id: EcoString,
    /// `a:srcRect` crop, in 1/1000 of a percent, as `[l, t, r, b]`.
    pub crop: Option<[i32; 4]>,
    pub geom: Option<Geometry>,
    pub alt: Option<EcoString>,
    /// `asvg:svgBlip` — a native SVG alongside the raster fallback.
    pub svg_rel_id: Option<EcoString>,
}

#[derive(Debug, Default)]
pub struct Table {
    pub xfrm: Option<Xfrm>,
    /// `a:gridCol/@w` column widths.
    pub grid: Vec<Emu>,
    pub rows: Vec<TableRow>,
    pub first_row_header: bool,
    /// `a:tableStyleId` — the style whose `ppt/tableStyles.xml` entry supplies
    /// the borders and banding for a table that states none itself. Half the
    /// tables in the wild rely on it.
    pub style_id: Option<EcoString>,
}

#[derive(Debug, Default)]
pub struct TableRow {
    pub height: Emu,
    pub cells: Vec<TableCell>,
}

#[derive(Debug, Default)]
pub struct TableCell {
    pub paras: Vec<Para>,
    /// `a:tcPr/a:lnL`, `a:lnR`, `a:lnT`, `a:lnB` — the cell's own four edges.
    pub borders: [Option<Line>; 4],
    pub grid_span: usize,
    pub row_span: usize,
    /// Covered by another cell's span — carries no content of its own.
    pub merged: bool,
    pub fill: Option<Fill>,
    pub anchor: Option<EcoString>,
    pub insets: Insets,
}

/// `a:xfrm`: where a shape sits, and how it is turned.
#[derive(Debug, Default, Clone, Copy)]
pub struct Xfrm {
    pub x: Emu,
    pub y: Emu,
    pub cx: Emu,
    pub cy: Emu,
    /// 60000ths of a degree, clockwise, about the box centre.
    pub rot: i32,
    pub flip_h: bool,
    pub flip_v: bool,
}

#[derive(Debug, Clone)]
pub enum Geometry {
    /// `a:prstGeom/@prst` plus its adjustment values.
    Preset { name: EcoString, adjust: Vec<(EcoString, i64)> },
    /// `a:custGeom` path commands, in the path's own coordinate space.
    Custom { w: Emu, h: Emu, segs: Vec<Seg> },
}

#[derive(Debug, Clone, Copy)]
pub enum Seg {
    Move(Emu, Emu),
    Line(Emu, Emu),
    Cubic(Emu, Emu, Emu, Emu, Emu, Emu),
    Close,
}

#[derive(Debug, Clone)]
pub enum Fill {
    /// Explicitly no fill (`a:noFill`) — distinct from "nothing was stated",
    /// which inherits.
    None,
    Solid(Color),
    Gradient {
        stops: Vec<(u32, Color)>,
        angle: Option<i32>,
        radial: bool,
    },
    /// `a:blipFill` — a picture used as a fill.
    Picture {
        rel_id: EcoString,
    },
    /// `a:pattFill` — a two-colour hatch. Typst has no pattern primitive that
    /// means the same thing; the foreground colour stands in.
    Pattern {
        fg: Color,
    },
}

#[derive(Debug, Clone)]
pub struct Line {
    pub width: Option<Emu>,
    pub fill: Option<Fill>,
    pub dash: Option<EcoString>,
    /// `a:custDash` run lengths in 1/1000 %, relative to the line width.
    pub custom_dash: Vec<(i64, i64)>,
    /// `a:headEnd`/`a:tailEnd` — an arrowhead at either end.
    pub arrowheads: bool,
}

/// A colour as PowerPoint states it, *before* theme resolution — the whole
/// point of keeping it unresolved here is that `schemeClr` needs the master's
/// colour map and the theme, neither of which the parser should reach for.
#[derive(Debug, Clone)]
pub enum Color {
    Srgb([u8; 3]),
    /// `a:schemeClr/@val`, plus the transforms applied to it.
    Scheme {
        slot: EcoString,
        transforms: Vec<ColorTransform>,
    },
    /// `a:sysClr` — resolved by its `lastClr` fallback.
    System([u8; 3]),
}

#[derive(Debug, Clone, Copy)]
pub enum ColorTransform {
    /// `a:alpha` in 1/1000 %.
    Alpha(u32),
    /// `a:lumMod`/`a:lumOff`/`a:shade`/`a:tint`, all in 1/1000 %.
    LumMod(u32),
    LumOff(u32),
    Shade(u32),
    Tint(u32),
}

#[derive(Debug, Default)]
pub struct Para {
    pub runs: Vec<Run>,
    pub props: ParaProps,
}

#[derive(Debug, Default, Clone)]
pub struct ParaProps {
    /// `@lvl` — outline level, 0-based, which is what selects the master's
    /// list style.
    pub level: u8,
    pub align: Option<EcoString>,
    pub margin_left: Option<Emu>,
    pub indent: Option<Emu>,
    pub bullet: Option<Bullet>,
    /// `a:lnSpc` as a percentage in 1/1000 %, or absolute points.
    pub line_spacing: Option<Spacing>,
    pub space_before: Option<Spacing>,
    pub space_after: Option<Spacing>,
    pub rtl: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Spacing {
    /// 1/1000 of a percent.
    Percent(i64),
    /// Hundredths of a point.
    Points(i64),
}

#[derive(Debug, Clone)]
pub enum Bullet {
    /// `a:buNone` — explicitly no bullet, which must override an inherited one.
    None,
    Char {
        glyph: EcoString,
        font: Option<EcoString>,
    },
    /// `a:buAutoNum/@type` plus its start.
    AutoNum {
        kind: EcoString,
        start: u32,
    },
}

#[derive(Debug, Default)]
pub struct Run {
    pub text: EcoString,
    pub props: RunProps,
    /// A `a:hlinkClick` on the run.
    pub link: Option<Hyperlink>,
    /// This run is a field (`a:fld`), e.g. the slide number.
    pub field: Option<EcoString>,
    /// A line break (`a:br`) rather than a text run.
    pub line_break: bool,
}

#[derive(Debug, Clone)]
pub enum Hyperlink {
    /// External, by relationship id.
    Rel(EcoString),
    /// `ppaction://hlinkshowjump?jump=…` or a slide relationship.
    Slide(usize),
}

#[derive(Debug, Default, Clone)]
pub struct RunProps {
    /// Hundredths of a point (`@sz`).
    pub size: Option<i32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<EcoString>,
    pub strike: Option<EcoString>,
    pub color: Option<Color>,
    pub font: Option<EcoString>,
    /// `a:ea/@typeface` — the face for East-Asian text.
    ///
    /// A run's `a:latin` and `a:ea` are two faces for *one* run, chosen per
    /// glyph by script. Typst's `font:` takes a list and falls back per glyph,
    /// which is the same rule — so both must be carried, or every CJK deck
    /// gets its Latin face and tofu.
    pub east_asian: Option<EcoString>,
    /// `@spc` — tracking in hundredths of a point.
    pub spacing: Option<i32>,
    /// `@baseline` in 1/1000 %: positive superscript, negative subscript.
    pub baseline: Option<i32>,
    pub highlight: Option<Color>,
    pub caps: Option<EcoString>,
    pub lang: Option<EcoString>,
}

impl RunProps {
    /// Layer `self` over `base`: anything `self` states wins, anything it
    /// leaves unset inherits. The direction that matters — a slide's own run
    /// properties override the layout's, which override the master's.
    pub fn over(&self, base: &RunProps) -> RunProps {
        RunProps {
            size: self.size.or(base.size),
            bold: self.bold.or(base.bold),
            italic: self.italic.or(base.italic),
            underline: self.underline.clone().or_else(|| base.underline.clone()),
            strike: self.strike.clone().or_else(|| base.strike.clone()),
            color: self.color.clone().or_else(|| base.color.clone()),
            font: self.font.clone().or_else(|| base.font.clone()),
            east_asian: self.east_asian.clone().or_else(|| base.east_asian.clone()),
            spacing: self.spacing.or(base.spacing),
            baseline: self.baseline.or(base.baseline),
            highlight: self.highlight.clone().or_else(|| base.highlight.clone()),
            caps: self.caps.clone().or_else(|| base.caps.clone()),
            lang: self.lang.clone().or_else(|| base.lang.clone()),
        }
    }
}

impl ParaProps {
    pub fn over(&self, base: &ParaProps) -> ParaProps {
        ParaProps {
            // Not inherited: the level *selects* which base to use, so taking
            // it from the base would be circular.
            level: self.level,
            align: self.align.clone().or_else(|| base.align.clone()),
            margin_left: self.margin_left.or(base.margin_left),
            indent: self.indent.or(base.indent),
            bullet: self.bullet.clone().or_else(|| base.bullet.clone()),
            line_spacing: self.line_spacing.or(base.line_spacing),
            space_before: self.space_before.or(base.space_before),
            space_after: self.space_after.or(base.space_after),
            rtl: self.rtl || base.rtl,
        }
    }
}

/// `p:ph` — the shape's role, and the key its formatting inherits by.
#[derive(Debug, Clone)]
pub struct Placeholder {
    pub kind: PhKind,
    /// `@idx`. Two body placeholders on one layout are told apart by this and
    /// nothing else.
    pub idx: Option<u32>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PhKind {
    Title,
    CtrTitle,
    Subtitle,
    Body,
    Object,
    SlideNumber,
    Footer,
    Date,
    Other,
}

impl PhKind {
    pub fn parse(value: &str) -> Self {
        match value {
            "title" => Self::Title,
            "ctrTitle" => Self::CtrTitle,
            "subTitle" => Self::Subtitle,
            "body" => Self::Body,
            "obj" => Self::Object,
            "sldNum" => Self::SlideNumber,
            "ftr" => Self::Footer,
            "dt" => Self::Date,
            _ => Self::Other,
        }
    }

    /// Whether this placeholder holds the slide's headline.
    pub fn is_title(self) -> bool {
        matches!(self, Self::Title | Self::CtrTitle)
    }

    /// Furniture PowerPoint also draws from the layout: slide number, footer,
    /// date.
    ///
    /// These are *not* skipped on import. The importer does not reproduce a
    /// layout's own shapes, so nothing else would draw them — one corpus deck
    /// (`lo-sd-multiplelayoutfooter`) has a single footer placeholder as its
    /// entire visible content, and skipping it emitted an empty slide.
    pub fn is_furniture(self) -> bool {
        matches!(self, Self::SlideNumber | Self::Footer | Self::Date)
    }
}
