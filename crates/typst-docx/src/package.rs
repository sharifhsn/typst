pub use typst_ooxml_core::opc::{Package, PackageOptions, RelMode, Rels};

pub const DOCX_PACKAGE_OPTIONS: PackageOptions =
    PackageOptions { rels_overrides: true, media_defaults: &[] };
