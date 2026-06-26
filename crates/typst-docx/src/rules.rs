//! DOCX show-rule registration.
//!
//! Deliberately empty for v1: we register **no** element show rules for
//! `Target::Docx`, so the native element tree survives realization and reaches
//! the converter intact. This function exists for symmetry with
//! `typst_html::register` and future use.

use typst_library::foundations::NativeRuleMap;

/// Registers the DOCX show rules (currently none).
pub fn register(_rules: &mut NativeRuleMap) {}
