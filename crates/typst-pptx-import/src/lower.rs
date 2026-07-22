//! [`PmlPackage`] → [`TypstDoc`].
//!
//! Where every judgement lives. The parser reads and the emitter prints; this
//! is the only layer allowed to decide that a `rot` becomes a `#rotate`, that
//! a furniture placeholder is not slide content, or that a SmartArt diagram
//! cannot come across.

use std::path::PathBuf;

use ecow::{eco_format, EcoString};
use rustc_hash::FxHashMap;

use crate::mappers::{picture, shape as shape_mapper, table as table_mapper, text};
use crate::opts::{Fidelity, ImportOptions};
use crate::pml::model::*;
use crate::pml::parse::Parser;
use crate::report::ImportReport;
use crate::resolve::{color, inherit};
use crate::tdoc;

/// EMU per point. 914400 per inch, 72 points per inch.
pub const EMU_PER_PT: f64 = 12700.0;

pub fn emu(value: Emu) -> f64 {
    value as f64 / EMU_PER_PT
}

/// Two lifetimes rather than one: the package bytes the parser borrows
/// (`'p`) outlive this context (`'a`), and collapsing them would force a
/// borrow that cannot be satisfied without `unsafe`.
pub struct LowerCtx<'a, 'p> {
    pub theme: &'a Theme,
    pub color_map: ColorMap,
    pub report: &'a mut ImportReport,
    pub opts: &'a ImportOptions,
    pub parser: &'a mut Parser<'p>,
    /// Extracted media, keyed by relationship id so one picture used twice is
    /// written once.
    pub assets: Vec<(PathBuf, Vec<u8>)>,
    seen_media: FxHashMap<EcoString, EcoString>,
    /// Slide part name → index, for resolving same-deck jumps.
    pub slide_index: FxHashMap<EcoString, usize>,
}

impl LowerCtx<'_, '_> {
    pub fn paint(&mut self, c: &Color) -> Option<tdoc::Paint> {
        let resolved = color::paint(c, self.theme, &self.color_map);
        if resolved.is_none() {
            self.report.approximate(
                "theme colour",
                "a colour naming a placeholder slot (`phClr`) or a theme slot this \
                 deck does not define could not be resolved, so the shape keeps \
                 Typst's default rather than a guessed colour",
            );
        }
        resolved
    }

    /// Extract a media part and return the path to write it at.
    pub fn media(&mut self, rel_id: &str) -> Option<EcoString> {
        if let Some(path) = self.seen_media.get(rel_id) {
            return Some(path.clone());
        }
        let target = self.parser.target(rel_id)?;
        if target.external {
            self.report.drop(
                "linked image",
                "the picture lives outside the package and was not embedded, so \
                 there are no bytes to extract",
            );
            return None;
        }
        let bytes = self.parser.bytes(&target.part)?;
        let name = target.part.rsplit('/').next().unwrap_or("image");
        let declared = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        // Typst decodes by extension, and a part's extension is whatever the
        // producer felt like writing. Sniffing the magic bytes is the only
        // thing that makes `image1.png` holding a JPEG compile — real decks
        // ship exactly that.
        let ext = sniff(&bytes).map(str::to_string).unwrap_or(declared);

        // Typst decodes by extension and cannot read metafiles at all. Say so
        // once, by name, rather than emitting an `image()` that fails to
        // compile — a broken import is worse than a reported gap.
        if matches!(ext.as_str(), "emf" | "wmf") {
            self.report.drop(
                "metafile image",
                "EMF/WMF are streams of GDI drawing commands rather than images, \
                 and no Rust decoder exists — the picture is omitted",
            );
            return None;
        }
        if matches!(ext.as_str(), "tiff" | "tif" | "bmp") {
            self.report.drop(
                "image format",
                eco_format!("Typst cannot load {ext} images, so this picture is omitted"),
            );
            return None;
        }

        // A format neither sniffing nor the extension recognises would emit
        // an `image()` Typst cannot decode, failing the whole compile for one
        // picture. Refuse it by name instead.
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp") {
            self.report.drop(
                "image format",
                eco_format!(
                    "`{ext}` is not a format Typst can load, so this picture is omitted"
                ),
            );
            return None;
        }

        let index = self.assets.len() + 1;
        let path: EcoString =
            eco_format!("{}/image{index}.{ext}", self.opts.assets_dir);
        self.assets.push((PathBuf::from(path.as_str()), bytes));
        self.seen_media.insert(rel_id.into(), path.clone());
        Some(path)
    }
}

/// Identify an image by its magic bytes.
///
/// Only the formats Typst can actually load; anything else is better refused
/// by name than emitted as an `image()` that fails to decode.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("jpg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Some("webp");
    }
    // SVG is text, and may open with a comment, a declaration or a BOM.
    let head = &bytes[..bytes.len().min(512)];
    let text = String::from_utf8_lossy(head);
    if text.contains("<svg") {
        return Some("svg");
    }
    None
}

pub fn lower(
    pkg: &PmlPackage,
    parser: &mut Parser<'_>,
    opts: &ImportOptions,
    report: &mut ImportReport,
) -> (tdoc::TypstDoc, Vec<(PathBuf, Vec<u8>)>) {
    // A deck with several masters has several colour maps; the first master's
    // is the document-wide default, and a slide whose own master differs is a
    // rarity this importer does not chase.
    let color_map = pkg.masters.first().map(|m| m.color_map.clone()).unwrap_or_default();

    let mut ctx = LowerCtx {
        theme: &pkg.theme,
        color_map,
        report,
        opts,
        parser,
        assets: Vec::new(),
        seen_media: FxHashMap::default(),
        slide_index: FxHashMap::default(),
    };

    // Built before any slide is lowered: a link on slide 1 may point at
    // slide 40, so the whole map has to exist before the first lookup.
    for (index, slide) in pkg.slides.iter().enumerate() {
        ctx.slide_index.insert(slide.part.clone(), index);
    }

    let mut slides = Vec::new();
    for (index, slide) in pkg.slides.iter().enumerate() {
        slides.push(lower_slide(slide, index, pkg, &mut ctx));
    }

    let doc = tdoc::TypstDoc {
        width: emu(pkg.size.cx),
        height: emu(pkg.size.cy),
        title: None,
        author: None,
        slides,
    };
    (doc, ctx.assets)
}

fn lower_slide(
    slide: &Slide,
    index: usize,
    pkg: &PmlPackage,
    ctx: &mut LowerCtx<'_, '_>,
) -> tdoc::Slide {
    let layout = slide.layout.and_then(|i| pkg.layouts.get(i));

    // A slide's background falls back to its layout's, then its master's —
    // the same three-level chain the shapes use, and the reason a themed deck
    // is not white.
    let fill_source = slide
        .bg
        .as_ref()
        .or_else(|| layout.and_then(|l| l.bg.as_ref()))
        .or_else(|| {
            layout
                .and_then(|l| l.master)
                .and_then(|i| pkg.masters.get(i))
                .and_then(|m| m.bg.as_ref())
        });
    let fill = fill_source.and_then(|f| lower_fill(f, ctx));

    let mut out = tdoc::Slide {
        heading: None,
        items: Vec::new(),
        notes: slide.notes.clone(),
        fill,
        hidden: slide.hidden,
    };

    for shape in &slide.shapes {
        lower_shape(shape, layout, pkg, ctx, &mut out.items, None);
    }

    // In idiomatic fidelity the title stops being a positioned box and becomes
    // a touying heading. Done here rather than in the emitter because it is a
    // judgement — the geometry is discarded, and only a placeholder PowerPoint
    // itself labelled a title earns that.
    if ctx.opts.fidelity == Fidelity::Idiomatic {
        promote_title(slide, layout, pkg, ctx, &mut out);
    }

    let _ = index;
    out
}

/// Move the title placeholder's text out of the item list and into a heading.
fn promote_title(
    slide: &Slide,
    layout: Option<&SlideLayout>,
    pkg: &PmlPackage,
    ctx: &mut LowerCtx<'_, '_>,
    out: &mut tdoc::Slide,
) {
    let Some(position) = slide.shapes.iter().position(|s| {
        matches!(s, Shape::Text(t) if t.placeholder.as_ref().is_some_and(|p| p.kind.is_title()))
    }) else {
        return;
    };
    let Shape::Text(title) = &slide.shapes[position] else { return };

    let inherited = inherit::resolve(title, layout, &pkg.masters).unwrap_or_default();
    let paras = text::lower_paragraphs(&title.paras, &inherited, ctx);
    let inlines: Vec<tdoc::Inline> =
        paras.into_iter().flat_map(|p| p.inlines).collect();
    if inlines.is_empty() {
        return;
    }
    out.heading = Some(inlines);

    // Drop the item the title produced. Counting placed items up to this
    // shape is safe because `lower_shape` pushes at most one item per shape
    // and preserves order.
    let mut seen = 0;
    out.items.retain(|_| {
        let keep = seen != position;
        seen += 1;
        keep
    });
}

/// Lower one shape, appending zero or one item.
///
/// `parent` carries a group's coordinate mapping, which is what makes nested
/// shapes land in the right place: a group states both where it sits and what
/// space its children are drawn in, and the two are routinely different.
fn lower_shape(
    shape: &Shape,
    layout: Option<&SlideLayout>,
    pkg: &PmlPackage,
    ctx: &mut LowerCtx<'_, '_>,
    items: &mut Vec<tdoc::Item>,
    parent: Option<&GroupFrame>,
) {
    match shape {
        Shape::Text(text_shape) | Shape::Connector(text_shape) => {
            if let Some(ph) = &text_shape.placeholder
                && ph.kind.is_furniture()
            {
                // Slide numbers, footers and dates are drawn by the layout in
                // PowerPoint. Reproducing them per-slide would double them
                // against whatever the touying theme draws.
                ctx.report.approximate(
                    "slide furniture",
                    "a slide-number, footer or date placeholder is left to the \
                     theme rather than reproduced per slide",
                );
                return;
            }
            let inherited =
                inherit::resolve(text_shape, layout, &pkg.masters).unwrap_or_default();
            let xfrm = text_shape.xfrm.or(inherited.xfrm);
            let paras = text::lower_paragraphs(&text_shape.paras, &inherited, ctx);

            let has_text = paras.iter().any(|p| !p.inlines.is_empty());
            let drawn = shape_mapper::lower(text_shape, ctx);

            if !has_text && drawn.is_none() {
                return;
            }
            let block = match drawn {
                Some(call) => tdoc::Block::Shape {
                    call,
                    body: has_text.then_some(paras),
                },
                None => tdoc::Block::Paras(paras),
            };
            items.push(place(xfrm, block, parent, ctx));
        }
        Shape::Picture(pic) => {
            if let Some(block) = picture::lower(pic, ctx) {
                items.push(place(pic.xfrm, block, parent, ctx));
            }
        }
        Shape::Table(table) => {
            let block = table_mapper::lower(table, ctx);
            items.push(place(table.xfrm, block, parent, ctx));
        }
        Shape::Group(group) => {
            let frame = GroupFrame::new(group, parent);
            let mut inner = Vec::new();
            for child in &group.shapes {
                lower_shape(child, layout, pkg, ctx, &mut inner, Some(&frame));
            }
            if inner.is_empty() {
                return;
            }
            // The group's children already carry absolute coordinates, so the
            // group itself contributes no box — flattening here keeps the
            // emitted source one level shallower without moving anything.
            items.extend(inner);
        }
        Shape::Unsupported { kind, .. } => {
            ctx.report.drop(
                kind.clone(),
                "has no Typst counterpart; PowerPoint stores it as a live object \
                 rather than as drawable content",
            );
        }
    }
}

/// A group's coordinate mapping, composed with any enclosing group's.
pub struct GroupFrame {
    scale_x: f64,
    scale_y: f64,
    offset_x: f64,
    offset_y: f64,
}

impl GroupFrame {
    fn new(group: &Group, parent: Option<&GroupFrame>) -> Self {
        let xfrm = group.xfrm.unwrap_or_default();
        let (chx, chy) = group.child_off.unwrap_or((0, 0));
        let (chcx, chcy) = group.child_ext.unwrap_or((xfrm.cx, xfrm.cy));

        // The ratio between the group's own extent and its children's declared
        // extent is a genuine scale factor, and PowerPoint uses it: resizing a
        // group changes `ext` and leaves `chExt` alone.
        let sx = if chcx != 0 { emu(xfrm.cx) / emu(chcx) } else { 1.0 };
        let sy = if chcy != 0 { emu(xfrm.cy) / emu(chcy) } else { 1.0 };
        let own = GroupFrame {
            scale_x: sx,
            scale_y: sy,
            offset_x: emu(xfrm.x) - emu(chx) * sx,
            offset_y: emu(xfrm.y) - emu(chy) * sy,
        };
        match parent {
            None => own,
            Some(p) => GroupFrame {
                scale_x: own.scale_x * p.scale_x,
                scale_y: own.scale_y * p.scale_y,
                offset_x: p.offset_x + own.offset_x * p.scale_x,
                offset_y: p.offset_y + own.offset_y * p.scale_y,
            },
        }
    }

    fn map(&self, x: f64, y: f64) -> (f64, f64) {
        (self.offset_x + x * self.scale_x, self.offset_y + y * self.scale_y)
    }
}

fn place(
    xfrm: Option<Xfrm>,
    block: tdoc::Block,
    parent: Option<&GroupFrame>,
    ctx: &mut LowerCtx<'_, '_>,
) -> tdoc::Item {
    let Some(xfrm) = xfrm else {
        // A shape with no transform anywhere in its inheritance chain has no
        // position to reproduce, so it joins the flow rather than being
        // dropped or guessed at.
        return tdoc::Item::Flow(block);
    };
    let (mut x, mut y) = (emu(xfrm.x), emu(xfrm.y));
    let (mut w, mut h) = (emu(xfrm.cx), emu(xfrm.cy));
    if let Some(frame) = parent {
        let (mx, my) = frame.map(x, y);
        x = mx;
        y = my;
        w *= frame.scale_x;
        h *= frame.scale_y;
    }
    if xfrm.flip_h || xfrm.flip_v {
        ctx.report.approximate(
            "flipped shape",
            "a shape mirrored by `flipH`/`flipV` is drawn unmirrored: Typst has \
             no reflection on a laid-out box",
        );
    }
    tdoc::Item::Placed {
        x,
        y,
        w,
        h,
        // 60000ths of a degree, clockwise, about the box centre — which is
        // `#rotate`'s own default origin.
        rot: xfrm.rot as f64 / 60000.0,
        block,
    }
}

pub fn lower_fill(fill: &Fill, ctx: &mut LowerCtx<'_, '_>) -> Option<tdoc::Paint> {
    match fill {
        Fill::None => None,
        Fill::Solid(color) => ctx.paint(color),
        Fill::Gradient { stops, angle, radial } => {
            let mut lowered = Vec::new();
            for (pos, color) in stops {
                if let Some(tdoc::Paint::Rgb(rgba)) = ctx.paint(color) {
                    lowered.push((*pos as f64 / 100_000.0, rgba));
                }
            }
            // Typst requires monotonic stop offsets. PowerPoint does not order
            // `a:gsLst`, and real decks ship it unordered — an unsorted list is
            // a hard error, not a rendering quirk.
            lowered.sort_by(|a, b| a.0.total_cmp(&b.0));
            lowered.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
            // A gradient needs two distinct stops. One stop is a solid colour
            // and says so; none is nothing.
            if lowered.len() < 2 {
                return lowered.first().map(|(_, rgba)| tdoc::Paint::Rgb(*rgba));
            }
            // Typst also requires the run to span the whole 0..1 range, while
            // PowerPoint happily starts a gradient at 20% and ends it at 80%.
            // Extending the end colours flat is what PowerPoint renders anyway.
            if lowered[0].0 > 0.0 {
                let first = lowered[0].1;
                lowered.insert(0, (0.0, first));
            }
            if lowered[lowered.len() - 1].0 < 1.0 {
                let last = lowered[lowered.len() - 1].1;
                lowered.push((1.0, last));
            }
            Some(tdoc::Paint::Gradient {
                stops: lowered,
                angle: angle.unwrap_or(0) as f64 / 60000.0,
                radial: *radial,
            })
        }
        Fill::Picture { .. } => {
            ctx.report.approximate(
                "picture fill",
                "a shape filled with an image keeps its outline but not the \
                 image: Typst fills take a paint, not a picture",
            );
            None
        }
        Fill::Pattern { fg } => {
            ctx.report.approximate(
                "pattern fill",
                "a two-colour hatch has no Typst equivalent, so its foreground \
                 colour stands in as a solid fill",
            );
            ctx.paint(fg)
        }
    }
}
