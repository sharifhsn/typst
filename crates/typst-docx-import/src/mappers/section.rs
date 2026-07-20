//! The `section` mapper: Word `w:sectPr` → the Typst IR's [`tdoc::PageSetup`].

use typst_ooxml_core::units::twip_to_abs;

use crate::tdoc::{Margins, PageSetup};
use crate::wml::model::SectPr;

pub fn lower_section(sect: &SectPr) -> PageSetup {
    let margin = if sect.margin_top.is_some()
        || sect.margin_bottom.is_some()
        || sect.margin_left.is_some()
        || sect.margin_right.is_some()
    {
        Some(Margins {
            top_pt: twip(sect.margin_top),
            bottom_pt: twip(sect.margin_bottom),
            left_pt: twip(sect.margin_left),
            right_pt: twip(sect.margin_right),
        })
    } else {
        None
    };

    PageSetup {
        width_pt: sect.page_w.map(|w| twip_to_abs(w as f64).to_pt()),
        height_pt: sect.page_h.map(|h| twip_to_abs(h as f64).to_pt()),
        margin,
        flipped: sect.landscape,
    }
}

fn twip(v: Option<i64>) -> f64 {
    v.map(|v| twip_to_abs(v as f64).to_pt()).unwrap_or(0.0)
}
