//! Format-neutral infrastructure shared by Typst exporters.
//!
//! Exporters decide whether content should stay native, be approximated, or
//! fall back to pixels. This crate owns the target-independent mechanics used
//! after that decision.

pub mod raster;
