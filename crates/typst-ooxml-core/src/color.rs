//! Color conversion helpers shared by OOXML writers.

use typst_library::visualize::{Color, ColorSpace, ProcessColorSpace};

pub fn raw_rgb(color: &Color) -> [u8; 3] {
    let [r, g, b, _] = color.to_vec4_u8();
    [r, g, b]
}

pub fn srgb_rgba(color: &Color) -> [u8; 4] {
    let srgb = ColorSpace::Process(ProcessColorSpace::Srgb);
    let color = color.to_space(&srgb).unwrap_or_else(|_| color.clone());
    color.to_vec4_u8()
}

pub fn srgb_rgb(color: &Color) -> [u8; 3] {
    let [r, g, b, _] = srgb_rgba(color);
    [r, g, b]
}

pub fn hex_rgb(rgb: [u8; 3]) -> String {
    format!("{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

pub fn alpha_to_100k(alpha: u8) -> u32 {
    crate::units::alpha_to_100k(alpha)
}
