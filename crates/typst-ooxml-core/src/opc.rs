//! OPC (Open Packaging Conventions) zip package assembly.

use std::collections::BTreeMap;
use std::io::{Cursor, Write};

use ecow::{EcoString, eco_format};
use rustc_hash::FxHashMap;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, DateTime};

use crate::xml::escape_attr;
use crate::{ns, xml};

/// Relationship target mode.
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum RelMode {
    Internal,
    External,
}

/// One relationship entry.
#[derive(Clone)]
struct RelEntry {
    id: EcoString,
    type_uri: EcoString,
    target: EcoString,
    mode: RelMode,
}

/// A relationships container for one source part (the package root or
/// `document.xml`). Hands out rIds and emits both the `r:id` used in XML and the
/// matching `<Relationship>`.
#[derive(Clone)]
pub struct Rels {
    next: u32,
    entries: Vec<RelEntry>,
    by_target: FxHashMap<EcoString, EcoString>,
}

impl Rels {
    pub fn new() -> Self {
        Self {
            next: 1,
            entries: Vec::new(),
            by_target: FxHashMap::default(),
        }
    }

    /// Allocates (or reuses) a relationship; returns the rId string (`"rId7"`).
    pub fn add(&mut self, type_uri: &str, target: &str, mode: RelMode) -> EcoString {
        let key: EcoString = eco_format!("{type_uri}\u{0}{target}");
        if let Some(existing) = self.by_target.get(&key) {
            return existing.clone();
        }
        let id: EcoString = eco_format!("rId{}", self.next);
        self.next += 1;
        self.entries.push(RelEntry {
            id: id.clone(),
            type_uri: type_uri.into(),
            target: target.into(),
            mode,
        });
        self.by_target.insert(key, id.clone());
        id
    }

    /// Serializes to a `<Relationships>` XML string.
    pub fn to_xml(&self) -> String {
        let mut s = String::from(crate::xml::XML_DECL);
        s.push_str("<Relationships xmlns=\"");
        s.push_str(ns::RELATIONSHIPS);
        s.push_str("\">");
        for e in &self.entries {
            s.push_str("<Relationship Id=\"");
            s.push_str(&e.id);
            s.push_str("\" Type=\"");
            s.push_str(&escape_attr(&e.type_uri));
            s.push_str("\" Target=\"");
            s.push_str(&escape_attr(&e.target));
            s.push('"');
            if e.mode == RelMode::External {
                s.push_str(" TargetMode=\"External\"");
            }
            s.push_str("/>");
        }
        s.push_str("</Relationships>");
        s
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for Rels {
    fn default() -> Self {
        Self::new()
    }
}

/// Compression mode for a part.
#[derive(Copy, Clone)]
pub enum Compress {
    Deflate,
    Store,
}

#[derive(Copy, Clone)]
pub struct PackageOptions {
    pub rels_overrides: bool,
    pub media_defaults: &'static [(&'static str, &'static str)],
}

/// Accumulates parts + their content types, then zips.
pub struct Package {
    parts: Vec<(String, Vec<u8>, Compress)>,
    defaults: BTreeMap<String, &'static str>,
    overrides: Vec<(String, &'static str)>,
    rels_overrides: bool,
}

/// Content type of the package-relationships part.
const CT_RELS: &str = ns::ct::RELS;

impl Package {
    pub fn new(options: PackageOptions) -> Self {
        let mut defaults = BTreeMap::new();
        defaults.insert("rels".to_string(), CT_RELS);
        defaults.insert("xml".to_string(), "application/xml");
        for (ext, content_type) in options.media_defaults {
            defaults.insert((*ext).to_string(), *content_type);
        }
        Self {
            parts: Vec::new(),
            defaults,
            overrides: Vec::new(),
            rels_overrides: options.rels_overrides,
        }
    }

    /// Adds an XML text part (Deflate). Registers its `Override` content type.
    pub fn add_xml(&mut self, part_name: &str, content_type: &'static str, body: String) {
        if self.rels_overrides || !part_name.ends_with(".rels") {
            self.overrides.push((format!("/{part_name}"), content_type));
        }
        self.parts
            .push((part_name.to_string(), body.into_bytes(), Compress::Deflate));
    }

    /// Adds a media/binary part (Store). Registers a `Default` for its extension.
    pub fn add_media(
        &mut self,
        part_name: &str,
        ext: &str,
        content_type: &'static str,
        bytes: Vec<u8>,
    ) {
        self.defaults.entry(ext.to_ascii_lowercase()).or_insert(content_type);
        self.parts.push((part_name.to_string(), bytes, Compress::Store));
    }

    /// Builds `[Content_Types].xml`.
    fn content_types_xml(&self) -> String {
        let mut s = String::from(xml::XML_DECL);
        s.push_str("<Types xmlns=\"");
        s.push_str(ns::CONTENT_TYPES);
        s.push_str("\">");
        for (ext, ct) in &self.defaults {
            s.push_str("<Default Extension=\"");
            s.push_str(&escape_attr(ext));
            s.push_str("\" ContentType=\"");
            s.push_str(ct);
            s.push_str("\"/>");
        }
        for (part, ct) in &self.overrides {
            s.push_str("<Override PartName=\"");
            s.push_str(&escape_attr(part));
            s.push_str("\" ContentType=\"");
            s.push_str(ct);
            s.push_str("\"/>");
        }
        s.push_str("</Types>");
        s
    }

    /// Adds the literal `[Content_Types].xml` + `_rels/.rels`, then zips.
    pub fn finish(mut self, root_rels: &Rels) -> Vec<u8> {
        // `[Content_Types].xml` must be written first.
        let content_types = self.content_types_xml();
        let root_rels_xml = root_rels.to_xml();

        let cursor = Cursor::new(Vec::new());
        let mut zip = ZipWriter::new(cursor);
        // Fixed timestamp for determinism.
        let mtime = DateTime::default();

        let write_one = |zip: &mut ZipWriter<Cursor<Vec<u8>>>,
                         name: &str,
                         bytes: &[u8],
                         c: Compress| {
            let method = match c {
                Compress::Deflate => CompressionMethod::Deflated,
                Compress::Store => CompressionMethod::Stored,
            };
            let opts = SimpleFileOptions::default()
                .compression_method(method)
                .last_modified_time(mtime)
                .unix_permissions(0o644);
            zip.start_file(name, opts).expect("zip start_file");
            zip.write_all(bytes).expect("zip write_all");
        };

        write_one(
            &mut zip,
            "[Content_Types].xml",
            content_types.as_bytes(),
            Compress::Deflate,
        );
        write_one(&mut zip, "_rels/.rels", root_rels_xml.as_bytes(), Compress::Deflate);

        // Take ownership of parts so the closure borrow above is released.
        let parts = std::mem::take(&mut self.parts);
        for (name, bytes, c) in &parts {
            write_one(&mut zip, name, bytes, *c);
        }

        zip.finish().expect("zip finish").into_inner()
    }
}

impl Default for Package {
    fn default() -> Self {
        Self::new(PackageOptions { rels_overrides: true, media_defaults: &[] })
    }
}
