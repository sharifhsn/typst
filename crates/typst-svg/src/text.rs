use ecow::EcoString;
use skrifa::MetadataProvider;
use skrifa::instance::Size as SkrifaSize;
use skrifa::outline::DrawSettings;
use skrifa::outline::pen::{ControlBoundsPen, PathStyle};
use ttf_parser::GlyphId;
use typst_library::layout::{Abs, Ratio, Size, Transform};
use typst_library::text::TextItem;
use typst_library::text::color::{
    GlyphFrame, GlyphFrameItem, glyph_frame, should_outline,
};
use typst_library::visualize::{FillRule, Paint, RelativeTo};

use crate::path::SvgPathBuilder;
use crate::write::{SvgElem, SvgIdRef, SvgTransform};
use crate::{DedupId, SVGRenderer, State};

/// Represents a glyph to be rendered.
#[derive(Clone)]
pub enum RenderedGlyph {
    /// A frame that contains an image glpyh.
    Frame(GlyphFrame),
    /// A path is a sequence of drawing commands.
    ///
    /// It is in the format of `M x y L x y C x1 y1 x2 y2 x y Z`.
    Path(EcoString),
}

impl SVGRenderer<'_> {
    /// Render a text item. The text is rendered as a group of glyphs. We will
    /// try to render the text as SVG first, then bitmap, then outline. If none
    /// of them works, we will skip the text.
    pub(super) fn render_text(
        &mut self,
        svg: &mut SvgElem,
        state: &State,
        text: &TextItem,
    ) {
        let svg = &mut svg.elem("g");

        // Flip the transform since fonts use a Y-Up coordinate system.
        let state = state.pre_concat(Transform::scale(Ratio::one(), -Ratio::one()));
        svg.attr("transform", SvgTransform(state.transform));

        let mut x = Abs::pt(0.0);
        let mut y = Abs::pt(0.0);
        for glyph in &text.glyphs {
            let id = GlyphId(glyph.id);
            let x_offset = x + glyph.x_offset.at(text.size);
            let y_offset = y + glyph.y_offset.at(text.size);

            self.render_glyph(svg, &state, text, id, x_offset, y_offset);

            x += glyph.x_advance.at(text.size);
            y += glyph.y_advance.at(text.size);
        }
    }

    fn render_glyph(
        &mut self,
        svg: &mut SvgElem,
        state: &State,
        text: &TextItem,
        glyph_id: GlyphId,
        x_offset: Abs,
        y_offset: Abs,
    ) {
        if should_outline(&text.font, glyph_id.0) {
            // Pre-scale outlined glyphs, so strokes and fill patterns don't
            // need to consider text size glyph scaling.
            let scale = Ratio::new(text.size.to_pt() / text.font.units_per_em());
            let key = (&text.font, glyph_id, scale);
            let (id, path) = self.glyphs.insert_with_val(key, || {
                let mut builder = SvgPathBuilder::with_scale(scale);
                draw_outline(text, glyph_id, &mut builder)?;
                // `skrifa` succeeds (with zero pen commands) for empty glyphs
                // like whitespace, where `ttf-parser`'s `outline_glyph` returned
                // `None`. Preserve the old behavior of skipping them.
                if builder.is_empty() {
                    return None;
                }
                Some(RenderedGlyph::Path(builder.finsish()))
            });

            if path.is_some() {
                self.render_path_glyph(svg, state, text, glyph_id, x_offset, y_offset, id)
            }
        } else {
            // Image glyphs apply a `scale` at use site, since colr, svg-, and
            // bitmap glyph images are usually quite large, and having one glyph
            // per text size is a bit of a waste.
            let key = (&text.font, glyph_id);
            let (id, frame) = self.glyphs.insert_with_val(key, || {
                let frame = glyph_frame(&text.font, glyph_id.0)?;
                Some(RenderedGlyph::Frame(frame))
            });

            if frame.is_some() {
                self.render_image_glyph(svg, x_offset, y_offset, text, id);
            }
        }
    }

    /// Write a reference to an image glyph that is stored in font units.
    fn render_image_glyph(
        &mut self,
        svg: &mut SvgElem,
        x_offset: Abs,
        y_offset: Abs,
        text: &TextItem,
        id: DedupId,
    ) {
        let scale = Ratio::new(text.size.to_pt() / text.font.units_per_em());
        // Flip the transform again, since images are drawn Y-Down.
        let ts = Transform::translate(x_offset, y_offset)
            .pre_concat(Transform::scale(scale, -scale));

        svg.elem("use")
            .attr("xlink:href", SvgIdRef(id))
            .attr("transform", SvgTransform(ts));
    }

    /// Render a pre-scaled path glyph defined by an outline.
    #[allow(clippy::too_many_arguments)]
    fn render_path_glyph(
        &mut self,
        svg: &mut SvgElem,
        state: &State,
        text: &TextItem,
        glyph_id: GlyphId,
        x_offset: Abs,
        y_offset: Abs,
        id: DedupId,
    ) {
        // Apply the transform here, because the state transform is used to draw
        // strokes and fills with gradients and tilings.
        let state = state.pre_concat(Transform::translate(x_offset, y_offset));

        let Some(bbox) = glyph_control_bounds(text, glyph_id) else {
            // This shouldn't happen, because the glyph has been successfully
            // outlined to create the path.
            return;
        };

        let aspect_ratio = Size::new(
            Abs::pt((bbox.x_max - bbox.x_min) as f64),
            Abs::pt((bbox.y_max - bbox.y_min) as f64),
        )
        .aspect_ratio();

        let mut use_ = svg.elem("use");
        use_.attr("xlink:href", SvgIdRef(id))
            .attr("x", x_offset.to_pt())
            .attr("y", y_offset.to_pt());

        self.write_fill(
            &mut use_,
            &text.fill,
            FillRule::default(),
            aspect_ratio,
            self.text_paint_transform(&state, &text.fill),
        );
        if let Some(stroke) = &text.stroke {
            self.write_stroke(
                &mut use_,
                stroke,
                aspect_ratio,
                self.text_paint_transform(&state, &stroke.paint),
            );
        }
    }

    fn text_paint_transform(&self, state: &State, paint: &Paint) -> Transform {
        match paint {
            Paint::Solid(_) => Transform::identity(),
            Paint::Gradient(gradient) => match gradient.unwrap_relative(true) {
                RelativeTo::Self_ => Transform::identity(),
                RelativeTo::Parent => Transform::scale(
                    Ratio::new(state.size.x.to_pt()),
                    Ratio::new(state.size.y.to_pt()),
                )
                .post_concat(state.transform.invert().unwrap()),
            },
            Paint::Tiling(tiling) => match tiling.unwrap_relative(true) {
                RelativeTo::Self_ => Transform::identity(),
                RelativeTo::Parent => state.transform.invert().unwrap(),
            },
        }
    }

    /// Build the glyph definitions.
    pub(super) fn write_glyph_defs(&mut self, svg: &mut SvgElem) {
        if self.glyphs.iter().all(|(_, g)| g.is_none()) {
            return;
        }

        let mut defs = svg.elem("defs");
        let glyphs = std::mem::take(&mut self.glyphs);
        for (id, glyph) in glyphs.iter() {
            let Some(glyph) = glyph else { continue };

            let mut symbol = defs.elem("symbol");
            symbol.attr("id", id);
            symbol.attr("overflow", "visible");

            match glyph {
                RenderedGlyph::Frame(frame) => {
                    let state = State::new(frame.size()).pre_translate(frame.item.pos());
                    match &frame.item {
                        GlyphFrameItem::Tofu(_, shape) => {
                            self.render_shape(&mut symbol, &state, shape);
                        }
                        GlyphFrameItem::Image(_, image, size) => {
                            self.render_image(&mut symbol, &state, image, size);
                        }
                    }
                }
                RenderedGlyph::Path(path) => {
                    symbol.elem("path").attr("d", path);
                }
            }
        }

        // The glyphs have been taken above, there shouldn't be any new glyphs
        // produced from writing the glyph definitions.
        assert!(self.glyphs.is_empty());
    }
}

/// The settings used to draw glyph outlines via `skrifa`.
///
/// We request *unscaled* (font-unit) coordinates so the existing
/// [`SvgPathBuilder`] scaling stays correct, and the `HarfBuzz` path style so
/// the point-stream interpretation matches `ttf-parser` (which agrees with
/// HarfBuzz, not FreeType, on contours that start with an off-curve point).
/// This keeps the emitted path data byte-identical to the old `ttf-parser`
/// output.
fn draw_settings(text: &TextItem) -> DrawSettings<'_> {
    DrawSettings::unhinted(SkrifaSize::unscaled(), text.font.location())
        .with_path_style(PathStyle::HarfBuzz)
}

/// Draws a glyph's outline into the given pen using `skrifa`, in font units.
///
/// Returns `None` if the glyph has no outline (e.g. it is not present in the
/// font or could not be drawn).
fn draw_outline(
    text: &TextItem,
    glyph_id: GlyphId,
    builder: &mut SvgPathBuilder,
) -> Option<()> {
    let glyph = text.font.skrifa().outline_glyphs().get(skrifa_gid(glyph_id))?;
    glyph.draw(draw_settings(text), builder).ok()?;
    Some(())
}

/// Computes the tight *control-point* bounding box of a glyph's outline, in font
/// units.
///
/// This mirrors `ttf-parser`'s `glyph_bounding_box`, which traces the outline
/// and extends the bounds by off-curve control points rather than returning the
/// stored `glyf` header bbox. `skrifa`'s `GlyphMetrics::bounds` would return the
/// header bbox instead, so we draw into a [`ControlBoundsPen`] to match exactly.
fn glyph_control_bounds(
    text: &TextItem,
    glyph_id: GlyphId,
) -> Option<skrifa::raw::types::BoundingBox<f32>> {
    let glyph = text.font.skrifa().outline_glyphs().get(skrifa_gid(glyph_id))?;
    let mut pen = ControlBoundsPen::new();
    glyph.draw(draw_settings(text), &mut pen).ok()?;
    pen.bounding_box()
}

/// Converts a `ttf-parser` glyph id into a `skrifa` glyph id.
fn skrifa_gid(glyph_id: GlyphId) -> skrifa::GlyphId {
    skrifa::GlyphId::from(glyph_id.0)
}
