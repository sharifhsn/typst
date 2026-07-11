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
use typst_library::introspection::Tag;
use typst_library::layout::{Abs, Frame, FrameItem};
use typst_syntax::Span;

use crate::ctx::DocxCtx;
use crate::report::{
    DecisionReason, ExportStage, LossSet, Representation, SuppressedKind,
};

/// What [`DocxCtx::rasterize`] produces for renderable content: the media
/// relationship id, drawing size, and plain text recovered from the frame.
pub(crate) type Rasterized = Option<(EcoString, typst_library::layout::Size, String)>;

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
        let inf_frame = self.layout_export_frame(content, styles, span, Abs::inf())?;
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
                    Some(frame) if usable_size(frame.size()) => frame,
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
    /// page content width and through a sub-engine with a throwaway sink.
    pub(crate) fn layout_export_frame(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
        height: Abs,
    ) -> SourceResult<Option<Frame>> {
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
    ) -> SourceResult<Option<Frame>> {
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

        let frame = match caught {
            Ok(Ok(frame)) => Some(frame),
            Ok(Err(errors)) => {
                for diagnostic in errors {
                    self.fidelity_report.suppress_content(
                        content,
                        ExportStage::FallbackLayout,
                        SuppressedKind::Error,
                        diagnostic,
                    );
                }
                None
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
                None
            }
        };
        Ok(frame)
    }

    /// Rasterizes arbitrary content and embeds it as a PNG media part.
    pub fn rasterize(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<Rasterized> {
        let (tags, rasterized) = self.rasterize_with_tags(content, styles, span)?;
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
    ) -> SourceResult<Rasterized> {
        let key = typst_utils::hash128(&(content, styles, height, self.available_width));
        if let Some(cached) = self.overlay_cache.get(&key) {
            let cached = Arc::clone(cached);
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

        let Some(frame) =
            self.layout_export_frame_in(content, styles, span, height, true)?
        else {
            return Ok(None);
        };
        let mut tags = Vec::new();
        collect_frame_tags(&frame, &mut tags);
        self.deferred_tags.extend(tags.iter().cloned());
        let frame_text = frame_to_text(&frame);
        let Some(rendered) = raster::render_frame_to_png(
            frame,
            raster::RasterOptions { pixel_per_pt: 2.0, crop_to_ink: false },
        ) else {
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

    /// Same as [`Self::rasterize`], but returns the frame tags to the caller
    /// instead of appending them to `deferred_tags`.
    pub(crate) fn rasterize_with_tags(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<(Vec<Tag>, Rasterized)> {
        self.rasterize_impl(content, styles, span, true)
    }

    fn rasterize_impl(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
        crop: bool,
    ) -> SourceResult<(Vec<Tag>, Rasterized)> {
        if std::env::var_os("DOCX_DEBUG_RASTER").is_some() {
            eprintln!("RASTERIZE: {}", content.elem().name());
        }

        // Prefer an infinite-height frame so tall figures are captured whole.
        let inf_frame = self.layout_export_frame(content, styles, span, Abs::inf())?;

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
            Some(frame) if usable_size(frame.size()) => frame,
            _ => match self.layout_export_frame(
                content,
                styles,
                span,
                self.raster_height,
            )? {
                Some(frame) if usable_size(frame.size()) => frame,
                _ => return Ok((tags, None)),
            },
        };
        let frame_text = frame_to_text(&frame);

        let Some(rendered) = raster::render_frame_to_png(
            frame,
            raster::RasterOptions { pixel_per_pt: 2.0, crop_to_ink: crop },
        ) else {
            return Ok((tags, None));
        };
        Ok((
            tags,
            Some((self.add_image(&rendered.png, "png"), rendered.size, frame_text)),
        ))
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
        let Some(frame) = self.layout_export_frame(content, styles, span, Abs::inf())?
        else {
            return Ok(None);
        };
        let size = frame.size();
        Ok(usable_size(size).then_some(size))
    }
}

/// Whether a laid-out size is finite and strictly positive on both axes.
fn usable_size(size: typst_library::layout::Size) -> bool {
    size.x.to_pt().is_finite()
        && size.y.to_pt().is_finite()
        && size.x > Abs::zero()
        && size.y > Abs::zero()
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

/// Reconstructs approximate reading-order text from a laid-out frame.
fn frame_to_text(frame: &Frame) -> String {
    use typst_library::layout::Point;
    let mut items: Vec<(Point, EcoString, Abs, Abs)> = Vec::new();
    collect_frame_text(frame, Point::zero(), &mut items);
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
            && pos.x - x_end > size * 0.25
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
