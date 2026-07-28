//! `a:schemeClr` → an actual colour.
//!
//! A theme colour is stated as a *name* (`accent1`, `tx1`) plus a chain of
//! transforms (`lumMod`, `shade`, `alpha`), and the name is resolved through
//! two indirections: the master's `p:clrMap` says which theme slot a semantic
//! name means, and `theme1.xml`'s `a:clrScheme` says what that slot's RGB is.
//! Skip either and every themed colour in the deck comes out wrong — which is
//! most of them, since PowerPoint's own UI paints in theme colours by default.

use crate::pml::model::{Color, ColorMap, ColorTransform, Theme};
use crate::tdoc::Paint;

/// Resolve a PowerPoint colour to RGBA.
pub fn resolve(color: &Color, theme: &Theme, map: &ColorMap) -> Option<[u8; 4]> {
    match color {
        Color::Srgb(rgb) => Some([rgb[0], rgb[1], rgb[2], 255]),
        Color::System(rgb) => Some([rgb[0], rgb[1], rgb[2], 255]),
        Color::Scheme { slot, transforms } => {
            let base = theme.color(&map_slot(slot, map))?;
            Some(apply(base, transforms))
        }
    }
}

pub fn paint(color: &Color, theme: &Theme, map: &ColorMap) -> Option<Paint> {
    resolve(color, theme, map).map(Paint::Rgb)
}

/// A semantic name (`bg1`, `tx1`, `phClr`…) → the theme slot it stands for.
///
/// The four mapped names are the whole point of `p:clrMap`: a deck that swaps
/// light and dark says so here, and nowhere else.
fn map_slot(slot: &str, map: &ColorMap) -> String {
    match slot {
        "bg1" => map.bg1.to_string(),
        "tx1" => map.tx1.to_string(),
        "bg2" => map.bg2.to_string(),
        "tx2" => map.tx2.to_string(),
        // `phClr` is "whatever colour the placeholder is being drawn with",
        // meaningful only inside a style matrix. Outside one there is nothing
        // to resolve it to, so it falls through to the caller's `None`.
        other => other.to_string(),
    }
}

fn apply(base: [u8; 3], transforms: &[ColorTransform]) -> [u8; 4] {
    let mut rgb = [base[0] as f64, base[1] as f64, base[2] as f64];
    let mut alpha = 255.0;

    for transform in transforms {
        match *transform {
            ColorTransform::Alpha(v) => alpha = 255.0 * pct(v),
            // Shade darkens towards black, tint lightens towards white. Both
            // are stated as "keep this fraction of the original".
            ColorTransform::Shade(v) => {
                let f = pct(v);
                rgb = rgb.map(|c| c * f);
            }
            ColorTransform::Tint(v) => {
                let f = pct(v);
                rgb = rgb.map(|c| c * f + 255.0 * (1.0 - f));
            }
            // Luminance is an HSL operation: scaling a channel would shift hue
            // as well, which is visible on saturated accent colours.
            ColorTransform::LumMod(v) => {
                let (h, s, l) = to_hsl(rgb);
                rgb = from_hsl(h, s, (l * pct(v)).clamp(0.0, 1.0));
            }
            ColorTransform::LumOff(v) => {
                let (h, s, l) = to_hsl(rgb);
                rgb = from_hsl(h, s, (l + pct(v)).clamp(0.0, 1.0));
            }
        }
    }
    [
        rgb[0].round().clamp(0.0, 255.0) as u8,
        rgb[1].round().clamp(0.0, 255.0) as u8,
        rgb[2].round().clamp(0.0, 255.0) as u8,
        alpha.round().clamp(0.0, 255.0) as u8,
    ]
}

/// OOXML states percentages in thousandths of a percent.
fn pct(value: u32) -> f64 {
    value as f64 / 100_000.0
}

fn to_hsl(rgb: [f64; 3]) -> (f64, f64, f64) {
    let [r, g, b] = rgb.map(|c| c / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f64::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, l)
}

fn from_hsl(h: f64, s: f64, l: f64) -> [f64; 3] {
    if s.abs() < f64::EPSILON {
        let v = l * 255.0;
        return [v, v, v];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    [
        hue_to_rgb(p, q, h + 1.0 / 3.0) * 255.0,
        hue_to_rgb(p, q, h) * 255.0,
        hue_to_rgb(p, q, h - 1.0 / 3.0) * 255.0,
    ]
}

fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

#[cfg(test)]
mod tests {
    use ecow::EcoString;

    use super::*;

    fn theme() -> Theme {
        Theme {
            colors: vec![
                (EcoString::from("dk1"), [0, 0, 0]),
                (EcoString::from("lt1"), [255, 255, 255]),
                (EcoString::from("accent1"), [0x44, 0x72, 0xC4]),
            ],
            ..Theme::default()
        }
    }

    #[test]
    fn a_scheme_colour_resolves_through_the_masters_colour_map() {
        // `tx1` is not a colour, it is a *name for a slot*, and the map is the
        // only thing that says which.
        let map = ColorMap { tx1: "lt1".into(), ..ColorMap::default() };
        let color = Color::Scheme { slot: "tx1".into(), transforms: vec![] };
        assert_eq!(resolve(&color, &theme(), &map), Some([255, 255, 255, 255]));

        // The default map sends tx1 to dk1 instead — same input, opposite
        // colour, which is exactly the failure a missing clrMap produces.
        let color = Color::Scheme { slot: "tx1".into(), transforms: vec![] };
        assert_eq!(resolve(&color, &theme(), &ColorMap::default()), Some([0, 0, 0, 255]));
    }

    #[test]
    fn shade_darkens_and_tint_lightens() {
        let map = ColorMap::default();
        let half = |t| Color::Scheme { slot: "accent1".into(), transforms: vec![t] };
        let shaded =
            resolve(&half(ColorTransform::Shade(50_000)), &theme(), &map).unwrap();
        let tinted =
            resolve(&half(ColorTransform::Tint(50_000)), &theme(), &map).unwrap();
        assert!(shaded[0] < 0x44, "shade must darken: {shaded:?}");
        assert!(tinted[0] > 0x44, "tint must lighten: {tinted:?}");
    }

    #[test]
    fn alpha_lands_in_the_fourth_channel() {
        let color = Color::Scheme {
            slot: "accent1".into(),
            transforms: vec![ColorTransform::Alpha(50_000)],
        };
        let resolved = resolve(&color, &theme(), &ColorMap::default()).unwrap();
        assert_eq!(resolved[3], 128);
    }

    #[test]
    fn an_unknown_slot_resolves_to_nothing_rather_than_a_guess() {
        // `phClr` outside a style matrix has no referent. Painting it black
        // would be a silent lie; `None` lets the caller report it.
        let color = Color::Scheme { slot: "phClr".into(), transforms: vec![] };
        assert_eq!(resolve(&color, &theme(), &ColorMap::default()), None);
    }
}
