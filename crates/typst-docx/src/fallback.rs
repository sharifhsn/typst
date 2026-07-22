//! Layout and raster fallback support for DOCX conversion.
//!
//! This module owns the boundary where flowing DOCX content is temporarily
//! re-laid out under the paged target, rendered, and converted into an embedded
//! image plus recovered text and introspection tags. Content classification and
//! mapper dispatch intentionally remain in [`crate::convert`] and
//! [`crate::ctx`].

use std::sync::Arc;

use ecow::EcoString;
use typst_export_common::raster;
use typst_library::diag::{SourceDiagnostic, SourceResult};
use typst_library::foundations::{Content, StyleChain};
use typst_library::introspection::{Location, Tag};
use typst_library::layout::{Abs, Frame, FrameItem, PlaceElem, Point, Size};
use typst_library::math::EquationElem;
use typst_library::visualize::{Color, Geometry};
use typst_syntax::Span;

use crate::ctx::DocxCtx;
use crate::report::{
    DecisionReason, ExportStage, LossSet, Representation, SuppressedKind,
};

/// What [`DocxCtx::rasterize`] produces for renderable content: the media
/// relationship id, drawing size, and plain text recovered from the frame.
pub(crate) type Rasterized = Option<(EcoString, typst_library::layout::Size, String)>;
type TiledRasterized = Option<(Vec<(EcoString, Size)>, String)>;
type RawRasterized = Option<(Vec<u8>, Size, String)>;
type RasterizeResult = (Vec<Tag>, RawRasterized, bool);

/// A cached [`DocxCtx::rasterize_page_overlay`] result, keyed on its inputs.
pub(crate) struct CachedOverlay {
    png: Arc<[u8]>,
    size: typst_library::layout::Size,
    frame_text: String,
    tags: Vec<Tag>,
}

impl<'a, 'e> DocxCtx<'a, 'e> {
    /// Recovers visible text from a whole region laid out under the paged
    /// target, while preserving any introspection tags produced by that layout.
    /// This is preferable to rasterization for text-primary fallbacks such as a
    /// caption whose target-specific semantic realization failed.
    pub(crate) fn layout_fallback_text(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<Option<String>> {
        let (inf_frame, _) =
            self.layout_export_frame(content, styles, span, Abs::inf())?;
        let mut tags = Vec::new();
        let frame = match inf_frame {
            Some(frame) if usable_size(frame.size()) => frame,
            rejected => {
                if let Some(frame) = &rejected {
                    collect_frame_tags(frame, &mut tags);
                }
                match self.layout_export_frame(
                    content,
                    styles,
                    span,
                    self.raster_height,
                )? {
                    (Some(frame), _) if usable_size(frame.size()) => frame,
                    _ => {
                        self.deferred_tags.extend(tags);
                        return Ok(None);
                    }
                }
            }
        };
        collect_frame_tags(&frame, &mut tags);
        self.deferred_tags.extend(tags);
        let text = frame_to_text(&frame);
        Ok((!text.is_empty()).then_some(text))
    }

    /// Lays `content` out to a single frame under the paged target, against the
    /// page content width and through a sub-engine with a throwaway sink. The
    /// boolean is true only when absence means an error or panic rather than a
    /// successful but empty frame.
    pub(crate) fn layout_export_frame(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
        height: Abs,
    ) -> SourceResult<(Option<Frame>, bool)> {
        self.layout_export_frame_in(content, styles, span, height, false)
    }

    /// Like [`Self::layout_export_frame`], with control over whether the region
    /// shrink-fits or expands to the requested page-relative box.
    pub(crate) fn layout_export_frame_in(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
        height: Abs,
        expand: bool,
    ) -> SourceResult<(Option<Frame>, bool)> {
        use comemo::Track;
        use typst_library::foundations::{Target, TargetElem};
        use typst_library::layout::{Axes, Region, Size};

        let target = TargetElem::target.set(Target::Paged).wrap();
        let styles = styles.chain(&target);
        let region =
            Region::new(Size::new(self.available_width, height), Axes::splat(expand));
        let loc = self.locator.next(&span);
        let layout_frame = self.engine.library.routines.layout_frame;

        // Re-layout can panic on content that cannot be handled frame-wise.
        // Rasterization is best-effort, so isolate the attempt and degrade only
        // this fragment rather than aborting the entire export.
        let (caught, delayed, warnings) = {
            let mut throwaway = typst_library::engine::Sink::new();
            let caught = {
                let mut sub = typst_library::engine::Engine {
                    world: self.engine.world,
                    library: self.engine.library,
                    introspector: typst_utils::Protected::from_raw(
                        self.engine.introspector.into_raw(),
                    ),
                    traced: self.engine.traced,
                    sink: throwaway.track_mut(),
                    route: typst_library::engine::Route::extend(
                        self.engine.route.track(),
                    ),
                };
                let prev_hook = std::panic::take_hook();
                std::panic::set_hook(Box::new(|_| {}));
                let caught =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        layout_frame(&mut sub, content, loc, styles, region)
                    }));
                std::panic::set_hook(prev_hook);
                caught
            };
            let delayed = throwaway.delayed();
            let warnings = throwaway.warnings();
            (caught, delayed, warnings)
        };

        for diagnostic in delayed {
            self.fidelity_report.suppress_content(
                content,
                ExportStage::FallbackLayout,
                SuppressedKind::DelayedError,
                diagnostic,
            );
        }
        for diagnostic in warnings {
            self.fidelity_report.suppress_content(
                content,
                ExportStage::FallbackLayout,
                SuppressedKind::Warning,
                diagnostic,
            );
        }

        let (frame, failed) = match caught {
            Ok(Ok(frame)) => (Some(frame), false),
            Ok(Err(errors)) => {
                for diagnostic in errors {
                    self.fidelity_report.suppress_content(
                        content,
                        ExportStage::FallbackLayout,
                        SuppressedKind::Error,
                        diagnostic,
                    );
                }
                (None, true)
            }
            Err(_) => {
                self.fidelity_report.suppress_content(
                    content,
                    ExportStage::FallbackLayout,
                    SuppressedKind::Panic,
                    SourceDiagnostic::error(
                        span,
                        "paged fallback layout panicked during DOCX export",
                    ),
                );
                self.warn_without_decision("content that could not be laid out", span);
                (None, true)
            }
        };
        Ok((frame, failed))
    }

    /// Rasterizes arbitrary content and embeds it as a PNG media part.
    pub fn rasterize(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<Rasterized> {
        let (tags, rasterized, _) = self.rasterize_with_tags(content, styles, span)?;
        self.deferred_tags.extend(tags);
        Ok(rasterized)
    }

    /// Rasterizes a page-relative background or foreground without ink-cropping.
    pub(crate) fn rasterize_page_overlay(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
        height: Abs,
        canvas_fill: Option<[u8; 3]>,
    ) -> SourceResult<Rasterized> {
        let key = typst_utils::hash128(&(
            content,
            styles,
            height,
            self.available_width,
            canvas_fill,
        ));
        if let Some(cached) = self.overlay_cache.get(&key) {
            let cached = Arc::clone(cached);
            self.real_alias_locations
                .extend(cached.tags.iter().map(Tag::location));
            self.deferred_tags.extend(cached.tags.iter().cloned());
            self.record_content_decision(
                content,
                Representation::Raster,
                DecisionReason::PageOverlayRasterFallback,
                LossSet::RASTER,
                cached.frame_text.chars().count(),
            );
            return Ok(Some((
                self.add_image(&cached.png, "png"),
                cached.size,
                cached.frame_text.clone(),
            )));
        }

        let (frame, _) =
            self.layout_export_frame_in(content, styles, span, height, true)?;
        let Some(mut frame) = frame else {
            return Ok(None);
        };
        // Page furniture can be made entirely from `place`, whose visual ink
        // does not contribute to the measured frame. Preserve the full page
        // canvas before rasterization; otherwise a 1pt border can become a
        // 1pt PNG that is later stretched across the whole sheet.
        frame.set_size(Size::new(self.available_width, height));
        // A solid `page(fill:)` lies below `page(background:)` in Typst. Fold it
        // into the raster canvas instead of emitting a second behind-text header
        // drawing: LibreOffice reverses the relative z-order of those two
        // drawings and otherwise hides the background artwork behind the fill.
        if let Some([r, g, b]) = canvas_fill {
            let fill = Geometry::Rect(frame.size()).filled(Color::from_u8(r, g, b, 255));
            frame.prepend(Point::zero(), FrameItem::Shape(fill, Span::detached()));
        }
        let mut tags = Vec::new();
        collect_frame_tags(&frame, &mut tags);
        // Page overlays become reusable header stories in DOCX but are repeated
        // once per page in paged layout. Mark their semantic locations as
        // furniture aliases as well as deferring their tags, so exact body
        // aliases never count overlay occurrences in source ordinals.
        self.real_alias_locations.extend(tags.iter().map(Tag::location));
        self.deferred_tags.extend(tags.iter().cloned());
        let frame_text = frame_to_text(&frame);
        let Some(rendered) = raster::render_full_frame_to_png(frame, 2.0) else {
            return Ok(None);
        };
        let rel = self.add_image(&rendered.png, "png");
        self.overlay_cache.insert(
            key,
            Arc::new(CachedOverlay {
                png: Arc::from(rendered.png),
                size: rendered.size,
                frame_text: frame_text.clone(),
                tags,
            }),
        );
        self.record_content_decision(
            content,
            Representation::Raster,
            DecisionReason::PageOverlayRasterFallback,
            LossSet::RASTER,
            frame_text.chars().count(),
        );
        Ok(Some((rel, rendered.size, frame_text)))
    }

    /// Embeds an already-converged full page as one PNG. This is reserved for
    /// text-free pages whose hundreds of independent native shapes cross the
    /// Office-consumer complexity budget; re-layout would both repeat the
    /// expensive work and risk diverging from the paged authority.
    pub(crate) fn rasterize_dense_visual_page(
        &mut self,
        source: &Content,
        frame: Frame,
    ) -> Rasterized {
        let mut tags = Vec::new();
        collect_frame_tags(&frame, &mut tags);
        self.deferred_tags.extend(tags);
        let frame_text = frame_to_text(&frame);
        let rendered = raster::render_full_frame_to_png(frame, 2.0)?;
        let rel = self.add_image(&rendered.png, "png");
        self.record_content_decision(
            source,
            Representation::Raster,
            DecisionReason::DenseVisualPageRasterFallback,
            LossSet::RASTER,
            frame_text.chars().count(),
        );
        Some((rel, rendered.size, frame_text))
    }

    /// Embeds an already-converged coherent placed canvas at its authored
    /// logical frame size. Cropping to the ink bounds would turn harmless
    /// visual overflow into additional Word flow height and can move later
    /// sections onto extra pages; the finite canvas frame is the authoritative
    /// layout footprint.
    pub(crate) fn rasterize_coherent_placed_canvas(
        &mut self,
        source: &Content,
        frame: Frame,
    ) -> Rasterized {
        let mut tags = Vec::new();
        collect_frame_tags(&frame, &mut tags);
        self.deferred_tags.extend(tags);
        let frame_text = frame_to_placed_text(&frame);
        let rendered = raster::render_full_frame_to_png(frame, 2.0)?;
        let rel = self.add_image(&rendered.png, "png");
        self.record_content_decision(
            source,
            Representation::Raster,
            DecisionReason::DensePlacedCanvasRasterFallback,
            LossSet::RASTER,
            frame_text.chars().count(),
        );
        Some((rel, rendered.size, frame_text))
    }

    /// Same as [`Self::rasterize`], but returns the frame tags to the caller
    /// instead of appending them to `deferred_tags`. The final boolean
    /// distinguishes failed layout from intentionally empty output when no
    /// raster was produced.
    pub(crate) fn rasterize_with_tags(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<(Vec<Tag>, Rasterized, bool)> {
        let (tags, rendered, failed) =
            self.rasterize_impl(content, styles, span, true)?;
        Ok((
            tags,
            rendered.map(|(png, size, text)| (self.add_image(&png, "png"), size, text)),
            failed,
        ))
    }

    /// Rasterize a tall fallback into page-sized PNGs. Word/Writer handle a
    /// sequence of ordinary inline pictures much more reliably than a single
    /// multi-page-height bitmap (the latter can make LibreOffice spend minutes
    /// importing the document). Text is returned once and remains searchable.
    pub(crate) fn rasterize_tiled(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<TiledRasterized> {
        let (tags, rendered, _) = self.rasterize_impl(content, styles, span, true)?;
        self.deferred_tags.extend(tags);
        let Some((png, size, text)) = rendered else {
            return Ok(None);
        };
        if size.y <= self.raster_height {
            let rel = self.add_image(&png, "png");
            return Ok(Some((vec![(rel, size)], text)));
        }
        let pixmap = tiny_skia::Pixmap::decode_png(&png).map_err(
            |_| -> ecow::EcoVec<SourceDiagnostic> {
                vec![SourceDiagnostic::error(
                    span,
                    "could not decode DOCX raster fallback",
                )]
                .into()
            },
        )?;
        let px_per_pt = pixmap.height() as f64 / size.y.to_pt();
        let tile_px = (self.raster_height.to_pt() * px_per_pt).floor().max(1.0) as u32;
        let raw_tiles = slice_png_tiles(&pixmap, tile_px, span)?;
        let mut tiles = Vec::with_capacity(raw_tiles.len());
        for (bytes, h) in raw_tiles {
            let tile_size = Size::new(size.x, Abs::pt(h as f64 / px_per_pt));
            tiles.push((self.add_image(&bytes, "png"), tile_size));
        }
        Ok(Some((tiles, text)))
    }

    fn rasterize_impl(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
        crop: bool,
    ) -> SourceResult<RasterizeResult> {
        if std::env::var_os("DOCX_DEBUG_RASTER").is_some() {
            eprintln!("RASTERIZE: {}", content.elem().name());
        }

        // Prefer an infinite-height frame so tall figures are captured whole.
        // Equations are atomic page content; laying an unsupported equation out
        // against infinity can retain its document-absolute Y position and
        // manufacture a hundred-thousand-pixel transparent gap. Start equation
        // fallbacks at the real page height instead, avoiding both the unsafe
        // extent and a second expensive layout pass in equation-heavy books.
        let initial_height =
            if contains_equation(content) { self.raster_height } else { Abs::inf() };
        let (inf_frame, inf_failed) =
            self.layout_export_frame(content, styles, span, initial_height)?;

        // Harvest tags before checking size. An introspecting element can have a
        // temporarily degenerate frame during convergence, and dropping its tags
        // here would prevent it from ever stabilizing.
        let mut tags = Vec::new();
        if let Some(frame) = &inf_frame {
            collect_frame_tags(frame, &mut tags);
        }

        // Page-relative content can collapse under infinite height. Retry it at
        // the real page height before giving up.
        let frame = match inf_frame {
            Some(frame)
                if usable_size(frame.size()) && !pathological_raster_ink(&frame) =>
            {
                frame
            }
            _ => match self.layout_export_frame(
                content,
                styles,
                span,
                self.raster_height,
            )? {
                (Some(frame), _)
                    if usable_size(frame.size()) && !pathological_raster_ink(&frame) =>
                {
                    frame
                }
                (_, retry_failed) => return Ok((tags, None, inf_failed || retry_failed)),
            },
        };
        let frame_text = frame_to_text(&frame);

        let Some(rendered) = raster::render_frame_to_png(
            frame,
            raster::RasterOptions { pixel_per_pt: 2.0, crop_to_ink: crop },
        ) else {
            return Ok((tags, None, false));
        };
        Ok((tags, Some((rendered.png, rendered.size, frame_text)), false))
    }

    /// Forwards tags from a frame consumed by a non-raster fallback, such as a
    /// native DrawingML shape group.
    pub(crate) fn defer_frame_tags(&mut self, frame: &Frame) {
        collect_frame_tags(frame, &mut self.deferred_tags);
    }

    /// Returns a laid-out content size without rendering or harvesting tags.
    pub fn measure(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<Option<typst_library::layout::Size>> {
        let (frame, _) = self.layout_export_frame(content, styles, span, Abs::inf())?;
        let Some(frame) = frame else {
            return Ok(None);
        };
        let size = frame.size();
        Ok(usable_size(size).then_some(size))
    }
}

/// An infinite-height fallback can retain a page-absolute item position,
/// leaving a tiny border at the origin and the real content thousands of
/// points away. Rendering that frame allocates hundreds of millions of pixels
/// and emits Word-fragile extents. Retry it against the real page instead.
fn pathological_raster_ink(frame: &Frame) -> bool {
    const MAX_AXIS_PT: f64 = 8_000.0;
    let mut ink = None;
    raster::frame_ink_rect(frame, typst_library::layout::Point::zero(), &mut ink);
    ink.is_some_and(|rect| {
        rect.size().x.to_pt() > MAX_AXIS_PT || rect.size().y.to_pt() > MAX_AXIS_PT
    })
}

fn contains_equation(content: &Content) -> bool {
    use std::ops::ControlFlow;
    content
        .traverse(&mut |element: Content| {
            if element.is::<EquationElem>() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .is_break()
}

/// Whether a laid-out size is finite and strictly positive on both axes.
fn usable_size(size: typst_library::layout::Size) -> bool {
    size.x.to_pt().is_finite()
        && size.y.to_pt().is_finite()
        && size.x > Abs::zero()
        && size.y > Abs::zero()
}

fn slice_png_tiles(
    pixmap: &tiny_skia::Pixmap,
    max_height: u32,
    span: Span,
) -> SourceResult<Vec<(Vec<u8>, u32)>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < pixmap.height() {
        let target = (start + max_height).min(pixmap.height());
        let mut end = target;
        // Prefer a transparent seam near the page limit so glyphs/lines are not
        // cut in half. The window is bounded, and the exact limit is retained
        // when no safe row exists.
        for row in (start + 1..target).rev().take(64) {
            let pixels = &pixmap.data()[row as usize * pixmap.width() as usize * 4
                ..(row + 1) as usize * pixmap.width() as usize * 4];
            if pixels.chunks_exact(4).all(|px| px[3] == 0) {
                end = row + 1;
                break;
            }
        }
        let rect = tiny_skia::IntRect::from_ltrb(
            0,
            start as i32,
            pixmap.width() as i32,
            end as i32,
        )
        .ok_or_else(|| -> ecow::EcoVec<SourceDiagnostic> {
            vec![SourceDiagnostic::error(span, "could not split DOCX raster fallback")]
                .into()
        })?;
        let tile = pixmap.clone_rect(rect).ok_or_else(
            || -> ecow::EcoVec<SourceDiagnostic> {
                vec![SourceDiagnostic::error(
                    span,
                    "could not split DOCX raster fallback",
                )]
                .into()
            },
        )?;
        let bytes = tile.encode_png().map_err(|_| -> ecow::EcoVec<SourceDiagnostic> {
            vec![SourceDiagnostic::error(span, "could not encode DOCX raster tile")]
                .into()
        })?;
        out.push((bytes, end - start));
        start = end;
    }
    Ok(out)
}

/// Recursively collects introspection tags from a laid-out frame.
fn collect_frame_tags(frame: &Frame, out: &mut Vec<Tag>) {
    for (_, item) in frame.items() {
        match item {
            FrameItem::Group(group) => collect_frame_tags(&group.frame, out),
            FrameItem::Tag(tag) => out.push(tag.clone()),
            _ => {}
        }
    }
}

/// Collects laid-out text runs and their positions for reading-order recovery.
fn collect_frame_text(
    frame: &Frame,
    offset: typst_library::layout::Point,
    out: &mut Vec<(typst_library::layout::Point, EcoString, Abs, Abs)>,
) {
    use typst_library::layout::Point;
    for (pos, item) in frame.items() {
        let point = offset + *pos;
        match item {
            FrameItem::Group(group) => {
                let transform = &group.transform;
                collect_frame_text(
                    &group.frame,
                    point + Point::new(transform.tx, transform.ty),
                    out,
                );
            }
            FrameItem::Text(text) if !text.text.is_empty() => {
                out.push((point, text.text.clone(), text.size, text.width()));
            }
            _ => {}
        }
    }
}

/// Collects text under its innermost placed semantic scope. A coherent canvas
/// can intentionally overlap labels, so global x/y adjacency is not a safe
/// word-boundary oracle: two independent placed labels at the same coordinate
/// must remain separate searchable words.
fn collect_placed_frame_text(
    frame: &Frame,
    offset: typst_library::layout::Point,
    active: &mut Vec<(Location, usize)>,
    groups: &mut Vec<Vec<(Point, EcoString, Abs, Abs)>>,
) {
    for (pos, item) in frame.items() {
        let point = offset + *pos;
        match item {
            FrameItem::Tag(Tag::Start(content, _)) if content.is::<PlaceElem>() => {
                let Some(location) = content.location() else { continue };
                groups.push(Vec::new());
                active.push((location, groups.len() - 1));
            }
            FrameItem::Tag(Tag::End(location, ..)) => {
                if let Some(index) =
                    active.iter().rposition(|(active, _)| active == location)
                {
                    active.remove(index);
                }
            }
            FrameItem::Group(group) => {
                let transform = &group.transform;
                collect_placed_frame_text(
                    &group.frame,
                    point + Point::new(transform.tx, transform.ty),
                    active,
                    groups,
                );
            }
            FrameItem::Text(text) if !text.text.is_empty() => {
                let Some((_, index)) = active.last().copied() else { continue };
                groups[index].push((point, text.text.clone(), text.size, text.width()));
            }
            _ => {}
        }
    }
}

fn positioned_text_to_string(mut items: Vec<(Point, EcoString, Abs, Abs)>) -> String {
    if items.is_empty() {
        return String::new();
    }

    items.sort_by(|a, b| {
        a.0.y
            .partial_cmp(&b.0.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.x.partial_cmp(&b.0.x).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut output = String::new();
    let mut last_y: Option<Abs> = None;
    let mut last_x_end: Option<Abs> = None;
    for (pos, text, size, width) in items {
        if let Some(y) = last_y
            && (pos.y - y).abs() > size * 0.6
        {
            output.push('\n');
        } else if let Some(x_end) = last_x_end
            // Two runs continue the same word only when the second starts
            // exactly where the first ended. A *gap* is the ordinary word
            // separator; an OVERLAP (the second run starting back inside the
            // first) means they are independent pieces of text that merely
            // share a line — separately placed labels, or table cells whose
            // content outgrew a squeezed column. Gluing those produced
            // `BoostHandlingClimbStallSpeed` from five header cells, a word no
            // reader or search can find. Neither direction may join silently.
            && (pos.x - x_end).abs() > size * 0.25
            && !output.ends_with(char::is_whitespace)
        {
            output.push(' ');
        }
        output.push_str(&text);
        last_y = Some(pos.y);
        last_x_end = Some(pos.x + width);
    }
    output
}

/// Reconstructs approximate reading-order text from a laid-out frame.
fn frame_to_text(frame: &Frame) -> String {
    let mut items: Vec<(Point, EcoString, Abs, Abs)> = Vec::new();
    collect_frame_text(frame, Point::zero(), &mut items);
    positioned_text_to_string(items)
}

/// Reconstructs a coherent canvas's searchable text while preserving a hard
/// boundary between independently placed labels.
fn frame_to_placed_text(frame: &Frame) -> String {
    let mut active = Vec::new();
    let mut groups = Vec::new();
    collect_placed_frame_text(frame, Point::zero(), &mut active, &mut groups);
    groups
        .into_iter()
        .map(positioned_text_to_string)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{Abs, EcoString, Point, positioned_text_to_string, slice_png_tiles};
    use typst_syntax::Span;

    fn run(x: f64, y: f64, text: &str, width: f64) -> (Point, EcoString, Abs, Abs) {
        (
            Point::new(Abs::pt(x), Abs::pt(y)),
            text.into(),
            Abs::pt(10.0),
            Abs::pt(width),
        )
    }

    #[test]
    fn recovered_text_joins_only_runs_that_continue_each_other() {
        // Exactly abutting runs are one word (a formatting change mid-word);
        // a gap is a space. Both were already true.
        assert_eq!(
            positioned_text_to_string(vec![
                run(0.0, 0.0, "Job", 15.0),
                run(15.0, 0.0, "ber", 15.0),
                run(60.0, 0.0, "Tagline", 30.0),
            ]),
            "Jobber Tagline"
        );
    }

    #[test]
    fn recovered_text_never_glues_overlapping_runs() {
        // Independent runs that overlap — separately placed labels, or cells
        // whose text outgrew a squeezed column — are not a continuation of each
        // other. Gluing them made `BoostHandling`, a word nothing can find.
        assert_eq!(
            positioned_text_to_string(vec![
                run(0.0, 0.0, "Boost", 25.0),
                run(10.0, 0.0, "Handling", 40.0),
            ]),
            "Boost Handling"
        );
    }

    #[test]
    fn tall_raster_tiles_prefer_transparent_seams_and_preserve_rows() {
        let mut pixmap = tiny_skia::Pixmap::new(2, 10).unwrap();
        for y in 0..10 {
            for x in 0..2 {
                pixmap.pixels_mut()[y * 2 + x] =
                    tiny_skia::PremultipliedColorU8::from_rgba(0, y as u8, 0, 255)
                        .unwrap();
            }
        }
        pixmap.data_mut()[4 * 2 * 4..5 * 2 * 4].fill(0);
        let tiles = slice_png_tiles(&pixmap, 6, Span::detached()).unwrap();
        let heights: Vec<u32> = tiles.iter().map(|(_, h)| *h).collect();
        assert_eq!(heights, vec![5, 5]);
        let mut rows = Vec::new();
        for (png, _) in tiles {
            let tile = tiny_skia::Pixmap::decode_png(&png).unwrap();
            rows.extend(tile.pixels().iter().map(|p| p.green()));
        }
        assert_eq!(rows, pixmap.pixels().iter().map(|p| p.green()).collect::<Vec<_>>());
    }
}
