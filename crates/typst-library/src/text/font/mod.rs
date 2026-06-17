//! Font handling.

pub mod color;

mod book;
mod exceptions;
mod info;
mod metrics;
mod tag;
mod variant;
mod variations;

pub use self::book::FontBook;
pub use self::info::{Coverage, FontFlags, FontInfo};
pub use self::metrics::{
    FontMetrics, LineMetrics, MathConstants, ScriptMetrics, TextEdgeBounds,
    VerticalFontMetric,
};
pub use self::tag::Tag;
pub use self::variant::{FontStretch, FontStyle, FontVariant, FontWeight};
pub use self::variations::{AxisValue, FontAxis, FontVariations, StandardAxes};

use std::cell::OnceCell;
use std::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

use skrifa::MetadataProvider;
use skrifa::instance::Size;
use skrifa::outline::DrawSettings;
use skrifa::outline::pen::ControlBoundsPen;
use skrifa::raw::types::BoundingBox;
use skrifa::string::StringId;
use ttf_parser::GlyphId;

use self::exceptions::find_exception;
use self::info::find_name;
use crate::foundations::Bytes;
use crate::layout::{Abs, Em};
use crate::text::{BottomEdge, TopEdge};

/// An OpenType font.
///
/// Values of this type are cheap to clone and hash.
#[derive(Clone)]
pub struct Font(Arc<FontInner>);

/// The internal representation of a [`Font`].
struct FontInner {
    /// The font's index in the buffer.
    index: u32,
    /// Metadata about the font.
    info: FontInfo,
    // NOTE: `fontations` and `skrifa` both reference `data`, so it's important
    // for `data` to be dropped after them or they will be left dangling while
    // the data is dropped. Fields are dropped in declaration order, so `data`
    // needs to be declared after them.
    /// The underlying fontations face, used for harfrust shaping. This is a
    /// `read-fonts` 0.40 `FontRef` (harfrust's transitive `read-fonts`).
    fontations: harfrust::FontRef<'static>,
    /// A `skrifa` face over the same data. `skrifa` rides its own (older)
    /// `read-fonts`, so this is a *distinct* `FontRef` type from `fontations`
    /// and must be parsed separately. Used for metadata, metrics, outlines,
    /// charmap, attributes, and color.
    skrifa: skrifa::FontRef<'static>,
    /// Cached, coordinate-independent shaper data for harfrust.
    shaper_data: harfrust::ShaperData,
    /// The raw font data, possibly shared with other fonts from the same
    /// collection. The vector's allocation must not move, because `fontations`
    /// and `skrifa` point into it using unsafe code.
    data: Bytes,
}

impl Font {
    /// Parse a font from data and collection index.
    pub fn new(data: Bytes, index: u32) -> Option<Self> {
        // Safety:
        // - The slices's location is stable in memory:
        //   - We don't move the underlying vector
        //   - Nobody else can move it since we have a strong ref to the `Arc`.
        // - The internal 'static lifetime is not leaked because it's rewritten
        //   to the self-lifetime in `fontations()` and `skrifa()`.
        let slice: &'static [u8] =
            unsafe { std::slice::from_raw_parts(data.as_ptr(), data.len()) };

        let fontations = harfrust::FontRef::from_index(slice, index).ok()?;
        let skrifa = skrifa::FontRef::from_index(slice, index).ok()?;
        let shaper_data = harfrust::ShaperData::new(&fontations);
        let info = FontInfo::from_skrifa(&skrifa)?;

        Some(Self(Arc::new(FontInner {
            index,
            info,
            fontations,
            skrifa,
            shaper_data,
            data,
        })))
    }

    /// Parse all fonts in the given data.
    pub fn iter(data: Bytes) -> impl Iterator<Item = Self> {
        let count = font_count(&data);
        (0..count).filter_map(move |index| Self::new(data.clone(), index))
    }

    /// The underlying buffer.
    pub fn data(&self) -> &Bytes {
        &self.0.data
    }

    /// The font's index in the buffer.
    pub fn index(&self) -> u32 {
        self.0.index
    }

    /// The font's metadata.
    pub fn info(&self) -> &FontInfo {
        &self.0.info
    }

    /// A reference to the underlying `fontations` face (harfrust's
    /// `read-fonts`).
    pub fn fontations(&self) -> &harfrust::FontRef<'_> {
        // We can't implement Deref because that would leak the
        // internal 'static lifetime.
        &self.0.fontations
    }

    /// A reference to the underlying `skrifa` face.
    ///
    /// Used as the entry point for `skrifa`'s metadata providers (metrics,
    /// outlines, charmap, attributes, color). Note that this is a *different*
    /// `read-fonts` version than [`Font::fontations`], so the two `FontRef`
    /// types are not interchangeable.
    pub fn skrifa(&self) -> &skrifa::FontRef<'_> {
        // We can't implement Deref because that would leak the
        // internal 'static lifetime.
        &self.0.skrifa
    }

    /// Determine the font's PostScript name.
    pub fn post_script_name(&self) -> Option<String> {
        find_name(&self.0.skrifa, StringId::POSTSCRIPT_NAME)
    }

    /// Instantiates the font with specific text properties. The resulting
    /// type allows access to methods that depend on coordinates.
    #[comemo::memoize]
    pub fn instantiate(
        self,
        variant: FontVariant,
        size: Abs,
        custom: &FontVariations,
    ) -> FontInstance {
        let axes = &self.info().axes;
        let automatic = FontVariations::resolve(axes, variant, size);
        let full = automatic.chain(custom).normalized();
        self.instantiate_impl(full)
    }

    /// Instantiates the font with specific variation coordinates. The resulting
    /// type allows access to methods that depend on coordinates.
    #[comemo::memoize]
    fn instantiate_impl(self, variations: FontVariations) -> FontInstance {
        let data = self.data();
        let index = self.index();

        // Safety: See `Self::new`.
        let slice: &'static [u8] =
            unsafe { std::slice::from_raw_parts(data.as_ptr(), data.len()) };

        // A ttf-parser face with the variation coordinates applied, used for
        // metrics and glyph measurements.
        let mut ttf = ttf_parser::Face::parse(slice, index).unwrap();
        for &(tag, value) in &variations.0 {
            ttf.set_variation(tag.into(), value.0);
        }

        // A harfrust shaping instance carrying the same variation coordinates.
        let shaper_instance = harfrust::ShaperInstance::from_variations(
            self.fontations(),
            variations
                .0
                .iter()
                .map(|&(tag, value)| (harfrust::Tag::new(&tag.to_bytes()), value.0)),
        );

        // The same variation coordinates, normalized into `skrifa`'s space for
        // variation-aware metrics and outlines.
        let location = self.skrifa().axes().location(
            variations
                .0
                .iter()
                .map(|&(tag, value)| (skrifa::Tag::new(&tag.to_bytes()), value.0)),
        );

        let metrics = FontMetrics::from_ttf(&ttf);

        FontInstance(Arc::new(FontInstanceInner {
            metrics,
            ttf,
            shaper_instance,
            location,
            variations,
            font: self,
        }))
    }
}

impl Debug for Font {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "Font({}, {:?})", self.info().family, self.info().variant)
    }
}

impl Hash for Font {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.data.hash(state);
        self.0.index.hash(state);
    }
}

impl Eq for Font {}

impl PartialEq for Font {
    fn eq(&self, other: &Self) -> bool {
        self.0.data == other.0.data && self.0.index == other.0.index
    }
}

/// An OpenType font with fixed variation coordinates.
///
/// Values of this type are cheap to clone and hash.
#[derive(Clone)]
pub struct FontInstance(Arc<FontInstanceInner>);

/// The internal representation of a [`FontInstance`].
struct FontInstanceInner {
    /// The font's metrics.
    metrics: FontMetrics,
    // NOTE: `ttf` references `font`'s data, so it's important for `font` to be
    // dropped after `ttf` or `ttf` will be left dangling while the font is
    // dropped. Fields are dropped in declaration order, so `font` needs to be
    // declared after `ttf`.
    /// The underlying ttf-parser face, with variation coordinates applied.
    ttf: ttf_parser::Face<'static>,
    /// The harfrust shaping instance carrying the variation coordinates.
    shaper_instance: harfrust::ShaperInstance,
    /// The instance's variation coordinates, normalized into `skrifa`'s
    /// coordinate space. Pass these to `skrifa` metadata providers (via
    /// [`FontInstance::location`]) to obtain variation-aware metrics and
    /// outlines.
    location: skrifa::instance::Location,
    // The instance's variation coordinates.
    variations: FontVariations,
    /// The underlying font.
    font: Font,
}

impl FontInstance {
    /// The instance's underlying font.
    pub fn font(&self) -> &Font {
        &self.0.font
    }

    /// The instance's variation coordinates.
    pub fn variations(&self) -> &FontVariations {
        &self.0.variations
    }

    /// The font's metrics.
    pub fn metrics(&self) -> &FontMetrics {
        &self.0.metrics
    }

    /// The font's math constants.
    #[inline]
    pub fn math(&self) -> &MathConstants {
        self.0.metrics.math.get_or_init(|| MathConstants::new(self))
    }

    /// The number of font units per one em.
    pub fn units_per_em(&self) -> f64 {
        self.0.metrics.units_per_em
    }

    /// Convert from font units to an em length.
    pub fn to_em(&self, units: impl Into<f64>) -> Em {
        Em::from_units(units, self.units_per_em())
    }

    /// Look up the horizontal advance width of a glyph.
    pub fn x_advance(&self, glyph: u16) -> Option<Em> {
        self.0
            .ttf
            .glyph_hor_advance(GlyphId(glyph))
            .map(|units| self.to_em(units))
    }

    /// Look up the vertical advance width of a glyph.
    pub fn y_advance(&self, glyph: u16) -> Option<Em> {
        self.0
            .ttf
            .glyph_ver_advance(GlyphId(glyph))
            .map(|units| self.to_em(units))
    }

    /// A reference to the underlying `ttf-parser` face.
    ///
    /// Retained for the MATH table (`MathConstants`) and math layout/export,
    /// which `read-fonts`/`skrifa` do not cover.
    pub fn ttf(&self) -> &ttf_parser::Face<'_> {
        // We can't implement Deref because that would leak the
        // internal 'static lifetime.
        &self.0.ttf
    }

    /// A reference to the underlying `skrifa` face.
    ///
    /// Combine with [`FontInstance::location`] to query variation-aware
    /// metrics, outlines, and color data via `skrifa`'s metadata providers.
    pub fn skrifa(&self) -> &skrifa::FontRef<'_> {
        self.0.font.skrifa()
    }

    /// The instance's variation coordinates in `skrifa`'s normalized space,
    /// for use with `skrifa` metadata providers.
    pub fn location(&self) -> skrifa::instance::LocationRef<'_> {
        (&self.0.location).into()
    }

    /// Build a `harfrust` shaper for this instance, carrying its variation
    /// coordinates.
    pub fn shaper(&self) -> harfrust::Shaper<'_> {
        self.0
            .font
            .0
            .shaper_data
            .shaper(&self.0.font.0.fontations)
            .instance(Some(&self.0.shaper_instance))
            .build()
    }

    /// The tight bounding box of a glyph in font units.
    ///
    /// This mirrors `ttf_parser`'s `glyph_bounding_box`: it computes a *tight*
    /// box from the glyph's outline (applying this instance's variation
    /// coordinates), rather than reading the stored `glyf` header box, so the
    /// result is identical across `glyf`, CFF, and variable fonts.
    pub fn glyph_bbox(&self, glyph: u16) -> Option<BoundingBox<f32>> {
        let outlines = self.skrifa().outline_glyphs();
        let outline = outlines.get(skrifa::GlyphId::new(u32::from(glyph)))?;
        let mut pen = ControlBoundsPen::new();
        let settings = DrawSettings::unhinted(Size::unscaled(), self.location());
        outline.draw(settings, &mut pen).ok()?;
        pen.bounding_box()
    }

    /// Resolve the top and bottom edges of text.
    pub fn edges(
        &self,
        top_edge: TopEdge,
        bottom_edge: BottomEdge,
        font_size: Abs,
        bounds: TextEdgeBounds,
    ) -> (Abs, Abs) {
        let cell = OnceCell::new();
        let bbox = |gid, f: fn(BoundingBox<f32>) -> f32| {
            cell.get_or_init(|| self.glyph_bbox(gid))
                .map(|bbox| self.to_em(f(bbox)).at(font_size))
                .unwrap_or_default()
        };

        let top = match top_edge {
            TopEdge::Metric(metric) => match metric.try_into() {
                Ok(metric) => self.metrics().vertical(metric).at(font_size),
                Err(_) => match bounds {
                    TextEdgeBounds::Zero => Abs::zero(),
                    TextEdgeBounds::Frame(frame) => frame.ascent(),
                    TextEdgeBounds::Glyph(gid) => bbox(gid, |b| b.y_max),
                },
            },
            TopEdge::Length(length) => length.at(font_size),
        };

        let bottom = match bottom_edge {
            BottomEdge::Metric(metric) => match metric.try_into() {
                Ok(metric) => -self.metrics().vertical(metric).at(font_size),
                Err(_) => match bounds {
                    TextEdgeBounds::Zero => Abs::zero(),
                    TextEdgeBounds::Frame(frame) => frame.descent(),
                    TextEdgeBounds::Glyph(gid) => -bbox(gid, |b| b.y_min),
                },
            },
            BottomEdge::Length(length) => -length.at(font_size),
        };

        (top, bottom)
    }
}

impl Deref for FontInstance {
    type Target = Font;

    fn deref(&self) -> &Self::Target {
        self.font()
    }
}

impl Debug for FontInstance {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("FontInstance")
            .field("font", self.font())
            .field("variations", self.variations())
            .finish()
    }
}

impl Hash for FontInstance {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.font.hash(state);
        self.0.variations.hash(state);
    }
}

impl Eq for FontInstance {}

impl PartialEq for FontInstance {
    fn eq(&self, other: &Self) -> bool {
        self.0.font == other.0.font && self.0.variations == other.0.variations
    }
}

/// The number of fonts contained in the given (possibly collection) data.
///
/// Returns 1 for a single font or unrecognized data, matching the previous
/// `ttf_parser::fonts_in_collection(..).unwrap_or(1)` behavior.
fn font_count(data: &[u8]) -> u32 {
    match skrifa::raw::FileRef::new(data) {
        Ok(skrifa::raw::FileRef::Collection(collection)) => collection.len(),
        _ => 1,
    }
}
