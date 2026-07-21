//! WML → Typst-IR mappers, one per Word construct. Mirror of the exporter's
//! `mappers/`. Called by [`crate::lower`].

pub mod bibliography;
pub mod chart;
pub mod comment;
pub mod dml_shape;
pub mod drawing;
pub mod field;
pub mod math;
pub mod note;
pub mod para;
pub mod revision;
pub mod run;
pub mod section;
pub mod shape;
pub mod table;
