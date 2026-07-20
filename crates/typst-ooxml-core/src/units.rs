//! Unit conversions shared by OOXML writers.

use typst_library::layout::Abs;

pub const EMU_PER_PT: f64 = 12_700.0;
pub const EMU_PER_TWIP: i64 = 635;

pub fn pt_to_half_point(pt: f64) -> u32 {
    (pt * 2.0).round().max(0.0) as u32
}

pub fn pt_to_eighth_point(pt: f64) -> u32 {
    (pt * 8.0).round().max(0.0) as u32
}

pub fn abs_to_twip(abs: Abs) -> i32 {
    (abs.to_pt() * 20.0).round() as i32
}

pub fn abs_to_emu(abs: Abs) -> i64 {
    (abs.to_pt() * EMU_PER_PT).round() as i64
}

pub fn extent_emu(abs: Abs) -> i64 {
    abs_to_emu(abs).max(1)
}

pub fn alpha_to_100k(alpha: u8) -> u32 {
    alpha as u32 * 100_000 / 255
}

// --- Inverse conversions (OOXML readers / the DOCX importer) ----------------

/// Twips (twentieths of a point, the `w:pgSz`/`w:ind`/`w:spacing` unit) → an
/// absolute length. Inverse of [`abs_to_twip`].
pub fn twip_to_abs(twip: f64) -> Abs {
    Abs::pt(twip / 20.0)
}

/// Half-points (the `w:sz` unit) → points. Inverse of [`pt_to_half_point`].
pub fn half_point_to_pt(half_pt: f64) -> f64 {
    half_pt / 2.0
}

/// Eighths of a point (a border's `w:sz` unit) → points. Inverse of
/// [`pt_to_eighth_point`].
pub fn eighth_point_to_pt(eighth_pt: f64) -> f64 {
    eighth_pt / 8.0
}

/// EMU (English Metric Units, the DrawingML `cx`/`cy` unit) → an absolute
/// length. Inverse of [`abs_to_emu`].
pub fn emu_to_abs(emu: f64) -> Abs {
    Abs::pt(emu / EMU_PER_PT)
}
