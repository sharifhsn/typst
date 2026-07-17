//! Color conversion helpers shared by OOXML writers.

use typst_library::visualize::{Color, ColorSpace, ProcessColorSpace};

pub fn srgb_rgba(color: &Color) -> [u8; 4] {
    let srgb = ColorSpace::Process(ProcessColorSpace::Srgb);
    let color = color.to_space(&srgb).unwrap_or_else(|_| color.clone());
    color.to_vec4_u8()
}

pub fn srgb_rgb(color: &Color) -> [u8; 3] {
    let [r, g, b, _] = srgb_rgba(color);
    [r, g, b]
}

/// Flatten a color onto an opaque sRGB backdrop.
///
/// WordprocessingML colors such as `w:color`, `w:shd`, and border colors have
/// no alpha channel. Dropping alpha makes translucent colors unexpectedly dark
/// and saturated, so callers targeting those properties must composite first.
pub fn composite_rgb(color: &Color, backdrop: [u8; 3]) -> [u8; 3] {
    let [r, g, b, a] = srgb_rgba(color);
    let composite = |foreground: u8, background: u8| {
        let value = foreground as u32 * a as u32 + background as u32 * (255 - a as u32);
        ((value + 127) / 255) as u8
    };
    [composite(r, backdrop[0]), composite(g, backdrop[1]), composite(b, backdrop[2])]
}

/// Flatten a color onto Word's default white page/cell background.
pub fn composite_rgb_on_white(color: &Color) -> [u8; 3] {
    composite_rgb(color, [255; 3])
}

pub fn hex_rgb(rgb: [u8; 3]) -> String {
    format!("{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

pub fn alpha_to_100k(alpha: u8) -> u32 {
    crate::units::alpha_to_100k(alpha)
}
