//! Pandoc show-rule registration.
//!
//! Deliberately empty: no element show rules are registered for
//! `Target::Pandoc`, so the native element tree survives realization and
//! reaches the converter intact. There is no "skip show rules" flag — the
//! *absence* of `(elem, Target::Pandoc)` entries in the `NativeRuleMap` is the
//! mechanism. Exists for symmetry with `typst_docx::register` /
//! `typst_html::register`.

use typst_library::foundations::NativeRuleMap;

/// Registers the Pandoc show rules (currently none).
pub fn register(_rules: &mut NativeRuleMap) {}
