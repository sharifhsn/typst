//! `p:pic` → a Typst image.

use crate::lower::{LowerCtx, emu};
use crate::pml::model::*;
use crate::tdoc;

pub fn lower(pic: &Picture, ctx: &mut LowerCtx<'_, '_>) -> Option<tdoc::Block> {
    // A native SVG is preferred over the raster fallback PowerPoint stores
    // beside it: it is the source the author actually supplied, and Typst
    // renders it at any size. `typst-pptx` writes exactly this pair on export,
    // so preferring the SVG is also what makes that round-trip lossless.
    let path = pic
        .svg_rel_id
        .as_ref()
        .and_then(|id| ctx.media(id))
        .or_else(|| ctx.media(&pic.rel_id))?;

    let (width, height) = pic
        .xfrm
        .map(|x| (emu(x.cx), emu(x.cy)))
        .filter(|(w, h)| *w > 0.0 && *h > 0.0)?;

    let radius = match &pic.geom {
        Some(Geometry::Preset { name, adjust }) if name == "roundRect" => {
            let adj = adjust
                .iter()
                .find(|(n, _)| n == "adj")
                .map(|(_, v)| *v)
                .unwrap_or(16667);
            Some(width.min(height) * (adj as f64 / 100_000.0))
        }
        // An ellipse-clipped picture — the circular-avatar idiom — needs a
        // radius of half the shorter side to become a circle.
        Some(Geometry::Preset { name, .. }) if name == "ellipse" => {
            Some(width.min(height) / 2.0)
        }
        _ => None,
    };

    Some(tdoc::Block::Image(tdoc::Image {
        path,
        width,
        height,
        alt: pic.alt.clone(),
        radius,
        // `a:srcRect` is stated in thousandths of a percent of the *source*
        // image, positive inwards from each edge.
        crop: pic.crop.map(|[l, t, r, b]| {
            [
                l as f64 / 100_000.0,
                t as f64 / 100_000.0,
                r as f64 / 100_000.0,
                b as f64 / 100_000.0,
            ]
        }),
    }))
}
