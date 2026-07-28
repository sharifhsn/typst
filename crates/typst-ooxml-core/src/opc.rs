//! OPC (Open Packaging Conventions) zip package assembly, plus a hardened
//! read side ([`Reader`]) shared by the DOCX importer and round-trip merge.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::io::{Cursor, Read, Write};

use ecow::{EcoString, eco_format};
use rustc_hash::FxHashMap;
use zip::read::ZipArchive;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, DateTime};

use crate::xml::escape_attr;
use crate::{ns, xml};

// --- Read side --------------------------------------------------------------

/// Conservative resource limits for untrusted `.docx`/`.pptx` archives. These
/// mirror the round-trip merge path's own limits; a zip-bomb or a hostile part
/// must never exhaust memory. Callers that need larger caps can pre-validate
/// and use [`Reader::open_with_limits`].
#[derive(Copy, Clone, Debug)]
pub struct ReadLimits {
    pub max_archive_bytes: usize,
    pub max_part_bytes: u64,
    pub max_expanded_bytes: u64,
    pub max_entries: usize,
    /// Maximum XML element-nesting depth. XML parsers (roxmltree included)
    /// descend recursively per open element and overflow the stack on
    /// pathologically deep documents (e.g. a torture file nesting 5000 tables).
    /// Real OOXML nests only a handful of levels; this cap is far above any
    /// genuine document and far below the overflow threshold.
    pub max_xml_depth: usize,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 128 * 1024 * 1024,
            max_part_bytes: 64 * 1024 * 1024,
            max_expanded_bytes: 256 * 1024 * 1024,
            max_entries: 8192,
            max_xml_depth: 256,
        }
    }
}

/// A cheap, allocation-free scan for the maximum XML element-nesting depth,
/// used to reject documents that would overflow a recursive-descent parser
/// before it ever runs. Approximate by design (it does not fully parse
/// attribute values), but conservative: it never *under*-counts a genuinely
/// deep chain of simple element tags, which is the shape that causes overflow.
fn exceeds_xml_depth(xml: &str, limit: usize) -> bool {
    let bytes = xml.as_bytes();
    let mut depth: usize = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            // Comments, CDATA, processing instructions, declarations: not
            // elements, no depth change.
            Some(b'!') | Some(b'?') => {}
            // A close tag `</...>`.
            Some(b'/') => depth = depth.saturating_sub(1),
            // An open tag — unless it self-closes (`<.../>`).
            _ => {
                let end = bytes[i..].iter().position(|&b| b == b'>').map(|p| i + p);
                let self_closing = end.is_some_and(|e| e > i && bytes[e - 1] == b'/');
                if !self_closing {
                    depth += 1;
                    if depth > limit {
                        return true;
                    }
                }
            }
        }
        // Advance past this tag.
        match bytes[i..].iter().position(|&b| b == b'>') {
            Some(p) => i += p + 1,
            None => break,
        }
    }
    false
}

/// Error reading an OPC package.
#[derive(Debug)]
pub enum ReadError {
    /// The archive itself could not be opened as a zip.
    Zip(zip::result::ZipError),
    /// An I/O error occurred while reading a part.
    Io(std::io::Error),
    /// The archive violated a [`ReadLimits`] bound, or contained a forbidden
    /// construct (a `<!DOCTYPE>`/`<!ENTITY>` declaration — the XXE vector).
    Unsafe(&'static str),
}

impl Display for ReadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Zip(e) => write!(f, "invalid OPC package: {e}"),
            ReadError::Io(e) => write!(f, "error reading OPC part: {e}"),
            ReadError::Unsafe(msg) => write!(f, "unsafe OPC package: {msg}"),
        }
    }
}

impl Error for ReadError {}

/// A read-only view over an OPC package (a `.docx`/`.pptx` zip). Enforces
/// [`ReadLimits`] on open and on every part read, and rejects XML parts that
/// carry `<!DOCTYPE>`/`<!ENTITY>` declarations.
pub struct Reader<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    limits: ReadLimits,
    names: Vec<EcoString>,
}

impl<'a> Reader<'a> {
    /// Opens a package with the default [`ReadLimits`].
    pub fn open(bytes: &'a [u8]) -> Result<Self, ReadError> {
        Self::open_with_limits(bytes, ReadLimits::default())
    }

    /// Opens a package with explicit limits.
    pub fn open_with_limits(
        bytes: &'a [u8],
        limits: ReadLimits,
    ) -> Result<Self, ReadError> {
        if bytes.len() > limits.max_archive_bytes {
            return Err(ReadError::Unsafe("archive is too large"));
        }
        let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(ReadError::Zip)?;
        if archive.len() > limits.max_entries {
            return Err(ReadError::Unsafe("archive has too many entries"));
        }
        let mut expanded = 0u64;
        let mut names = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let entry = archive.by_index(index).map_err(ReadError::Zip)?;
            expanded = expanded
                .checked_add(entry.size())
                .ok_or(ReadError::Unsafe("expanded size overflow"))?;
            if expanded > limits.max_expanded_bytes {
                return Err(ReadError::Unsafe("expanded archive is too large"));
            }
            names.push(EcoString::from(entry.name()));
        }
        Ok(Self { archive, limits, names })
    }

    /// The names of every part in the package (zip entry paths), in archive
    /// order. Use to discover `word/media/*`, `word/header*.xml`, etc.
    pub fn names(&self) -> &[EcoString] {
        &self.names
    }

    /// Whether a part exists.
    pub fn has(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// Reads a part as a validated UTF-8 XML string. Returns `None` if the
    /// part is absent. Enforces the per-part size cap and rejects
    /// `<!DOCTYPE>`/`<!ENTITY>` (the XXE vector).
    pub fn xml_part(&mut self, name: &str) -> Result<Option<String>, ReadError> {
        let Some(bytes) = self.part_bytes(name)? else { return Ok(None) };
        let xml = String::from_utf8(bytes)
            .map_err(|_| ReadError::Unsafe("XML part is not valid UTF-8"))?;
        let lowered = xml.to_ascii_lowercase();
        if lowered.contains("<!doctype") || lowered.contains("<!entity") {
            return Err(ReadError::Unsafe(
                "DOCTYPE and ENTITY declarations are forbidden",
            ));
        }
        if exceeds_xml_depth(&xml, self.limits.max_xml_depth) {
            return Err(ReadError::Unsafe("XML nesting is too deep"));
        }
        Ok(Some(xml))
    }

    /// Reads a part's raw bytes (media, embedded objects). Returns `None` if
    /// absent. Enforces the per-part size cap.
    pub fn part_bytes(&mut self, name: &str) -> Result<Option<Vec<u8>>, ReadError> {
        if !self.has(name) {
            return Ok(None);
        }
        let mut entry = self.archive.by_name(name).map_err(ReadError::Zip)?;
        if entry.size() > self.limits.max_part_bytes {
            return Err(ReadError::Unsafe("a package part is too large"));
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry
            .by_ref()
            .take(self.limits.max_part_bytes + 1)
            .read_to_end(&mut buf)
            .map_err(ReadError::Io)?;
        if buf.len() as u64 > self.limits.max_part_bytes {
            return Err(ReadError::Unsafe("a package part is too large"));
        }
        Ok(Some(buf))
    }
}

// --- Relationship reading ---------------------------------------------------

/// One relationship parsed from a `.rels` part, verbatim.
#[derive(Clone, Debug)]
pub struct RelEntry {
    pub id: EcoString,
    /// The full relationship `Type` URI, unmodified.
    pub type_uri: EcoString,
    /// The raw `Target`, exactly as written — relative, possibly with `../`,
    /// and *not* resolved. Apply [`resolve_target`] when a package-absolute
    /// part name is needed.
    pub target: EcoString,
    /// `TargetMode="External"`.
    pub external: bool,
}

/// The `.rels` part name for a source part: `word/document.xml` →
/// `word/_rels/document.xml.rels`; a root-level `foo` → `_rels/foo.rels`. The
/// OPC convention every part-owned relationships part follows, and the inverse
/// the write side uses when it emits one.
pub fn rels_part_name(source_part: &str) -> String {
    match source_part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{source_part}.rels"),
    }
}

/// Every `<Relationship>` found beneath `root` (a parsed `.rels` document's
/// root node), in document order. Reads by local name; targets come back raw,
/// unresolved (see [`RelEntry::target`]).
pub fn rel_entries(root: roxmltree::Node) -> Vec<RelEntry> {
    root.descendants()
        .filter(|n| crate::xmlread::is_el(*n, "Relationship"))
        .filter_map(|node| {
            let id = crate::xmlread::attr(node, "Id")?;
            let target = crate::xmlread::attr(node, "Target")?;
            let type_uri = crate::xmlread::attr(node, "Type").unwrap_or("");
            let external = crate::xmlread::attr(node, "TargetMode") == Some("External");
            Some(RelEntry {
                id: id.into(),
                type_uri: type_uri.into(),
                target: target.into(),
                external,
            })
        })
        .collect()
}

/// Resolve a relationship `target` against the `source_part` that declared it,
/// including `../` segments — OOXML targets are relative and PowerPoint uses
/// `../` for nearly every cross-directory reference. A leading `/` is treated
/// as package-absolute. This is the lenient read-side resolver (it skips `.`
/// and empty segments rather than rejecting them); the strict validating
/// resolver used when *writing* a package is separate.
pub fn resolve_target(source_part: &str, target: &str) -> EcoString {
    if target.starts_with('/') {
        return EcoString::from(target.trim_start_matches('/'));
    }
    let base = source_part.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    let mut segments: Vec<&str> =
        if base.is_empty() { Vec::new() } else { base.split('/').collect() };
    for seg in target.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    EcoString::from(segments.join("/"))
}

// --- Write side -------------------------------------------------------------

/// Relationship target mode.
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum RelMode {
    Internal,
    External,
}

/// One relationship record held by a [`Rels`] set as it is built for writing.
#[derive(Clone)]
struct RelRecord {
    id: EcoString,
    type_uri: EcoString,
    target: EcoString,
    mode: RelMode,
}

/// The dedupe key for a relationship: two `add` calls agreeing on all three
/// fields reuse one rId.
fn rel_key(type_uri: &str, target: &str, mode: RelMode) -> EcoString {
    let mode_key = if mode == RelMode::Internal { 'I' } else { 'E' };
    eco_format!("{type_uri}\u{0}{target}\u{0}{mode_key}")
}

/// A position in a [`Rels`] table, taken with [`Rels::savepoint`].
#[derive(Copy, Clone)]
pub struct RelsSavepoint(usize);

/// A relationships container for one source part (the package root or
/// `document.xml`). Hands out rIds and emits both the `r:id` used in XML and the
/// matching `<Relationship>`.
#[derive(Clone)]
pub struct Rels {
    next: u32,
    entries: Vec<RelRecord>,
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

    /// Records the current state so a speculative batch of relationships can be
    /// undone with [`Rels::rollback`].
    pub fn savepoint(&self) -> RelsSavepoint {
        RelsSavepoint(self.entries.len())
    }

    /// Discards every relationship added since `savepoint` was taken.
    ///
    /// A caller that allocates relationships while building a part it may still
    /// abandon (a section whose header/footer lowering fails) must undo them:
    /// a relationship whose target part is never written makes the package
    /// invalid. Ids are deliberately *not* reused — `rId`s only have to be
    /// unique within their part, and a gap is harmless where a recycled id
    /// pointing at unrelated content would not be.
    pub fn rollback(&mut self, savepoint: RelsSavepoint) {
        for record in self.entries.drain(savepoint.0..) {
            let key = rel_key(&record.type_uri, &record.target, record.mode);
            self.by_target.remove(&key);
        }
    }

    /// Allocates (or reuses) a relationship; returns the rId string (`"rId7"`).
    pub fn add(&mut self, type_uri: &str, target: &str, mode: RelMode) -> EcoString {
        let key = rel_key(type_uri, target, mode);
        if let Some(existing) = self.by_target.get(&key) {
            return existing.clone();
        }
        let id: EcoString = eco_format!("rId{}", self.next);
        self.next += 1;
        self.entries.push(RelRecord {
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
    default_conflicts: Vec<(String, &'static str, &'static str)>,
    overrides: Vec<(String, &'static str)>,
    relationship_sets: Vec<(String, Rels)>,
    rels_overrides: bool,
}

/// A package invariant or ZIP-writing failure discovered during finalization.
#[derive(Debug)]
pub enum PackageError {
    DuplicatePart(String),
    InvalidPartName(String),
    ConflictingDefaultContentType {
        extension: String,
        first: &'static str,
        second: &'static str,
    },
    ConflictingOverrideContentType {
        part_name: String,
        first: &'static str,
        second: &'static str,
    },
    MissingRelationshipOwner {
        source_part: String,
    },
    MissingRelationshipTarget {
        source_part: String,
        target: String,
        resolved: String,
    },
    InvalidRelationshipTarget {
        source_part: String,
        target: String,
    },
    InvalidXml {
        part_name: String,
        message: String,
    },
    MissingRelationshipReference {
        source_part: String,
        relationship_id: String,
    },
    Zip(zip::result::ZipError),
}

impl Display for PackageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicatePart(name) => write!(f, "duplicate package part `{name}`"),
            Self::InvalidPartName(name) => {
                write!(f, "invalid package part name `{name}`")
            }
            Self::ConflictingDefaultContentType { extension, first, second } => write!(
                f,
                "conflicting content types for extension `{extension}`: `{first}` and `{second}`"
            ),
            Self::ConflictingOverrideContentType { part_name, first, second } => write!(
                f,
                "conflicting content types for part `{part_name}`: `{first}` and `{second}`"
            ),
            Self::MissingRelationshipOwner { source_part } => {
                write!(f, "relationship owner part `{source_part}` does not exist")
            }
            Self::MissingRelationshipTarget { source_part, target, resolved } => write!(
                f,
                "relationship from `{source_part}` targets missing part `{target}` (resolved as `{resolved}`)"
            ),
            Self::InvalidRelationshipTarget { source_part, target } => write!(
                f,
                "relationship from `{source_part}` has invalid internal target `{target}`"
            ),
            Self::InvalidXml { part_name, message } => {
                write!(f, "invalid XML in package part `{part_name}`: {message}")
            }
            Self::MissingRelationshipReference { source_part, relationship_id } => {
                write!(
                    f,
                    "package part `{source_part}` references missing relationship `{relationship_id}`"
                )
            }
            Self::Zip(err) => write!(f, "ZIP write failed: {err}"),
        }
    }
}

impl Error for PackageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Zip(err) => Some(err),
            _ => None,
        }
    }
}

impl From<zip::result::ZipError> for PackageError {
    fn from(err: zip::result::ZipError) -> Self {
        Self::Zip(err)
    }
}

/// Content type of the package-relationships part.
const CT_RELS: &str = ns::ct::RELS;

impl Package {
    pub fn new(options: PackageOptions) -> Self {
        let mut defaults = BTreeMap::new();
        let mut default_conflicts = Vec::new();
        defaults.insert("rels".to_string(), CT_RELS);
        defaults.insert("xml".to_string(), "application/xml");
        for (ext, content_type) in options.media_defaults {
            insert_default(&mut defaults, &mut default_conflicts, ext, content_type);
        }
        Self {
            parts: Vec::new(),
            defaults,
            default_conflicts,
            overrides: Vec::new(),
            relationship_sets: Vec::new(),
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
        insert_default(
            &mut self.defaults,
            &mut self.default_conflicts,
            ext,
            content_type,
        );
        self.parts.push((part_name.to_string(), bytes, Compress::Store));
    }

    /// Iterates over XML parts currently accumulated in the package.
    ///
    /// This is intentionally a read-only, format-neutral view. Format crates can
    /// use it for their own schema or consumer-policy gates before [`Self::finish`]
    /// performs the generic OPC validation and consumes the package.
    pub fn xml_parts(&self) -> impl Iterator<Item = (&str, &str)> {
        self.parts.iter().filter_map(|(name, bytes, _)| {
            if !name.ends_with(".xml") {
                return None;
            }
            std::str::from_utf8(bytes).ok().map(|body| (name.as_str(), body))
        })
    }

    /// Adds a part-owned relationships part and retains the typed set for
    /// target validation during finalization.
    pub fn add_relationships(
        &mut self,
        source_part: &str,
        rels: &Rels,
    ) -> Result<(), PackageError> {
        if rels.is_empty() {
            return Ok(());
        }
        if !valid_part_name(source_part) {
            return Err(PackageError::InvalidPartName(source_part.into()));
        }
        let rels_name = rels_part_name(source_part);
        self.add_xml(&rels_name, CT_RELS, rels.to_xml());
        self.relationship_sets.push((source_part.into(), rels.clone()));
        Ok(())
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
        let mut overrides = self.overrides.iter().collect::<Vec<_>>();
        overrides.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        for (part, ct) in overrides {
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
    pub fn finish(mut self, root_rels: &Rels) -> Result<Vec<u8>, PackageError> {
        self.validate(root_rels)?;

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
                         c: Compress|
         -> Result<(), PackageError> {
            let method = match c {
                Compress::Deflate => CompressionMethod::Deflated,
                Compress::Store => CompressionMethod::Stored,
            };
            let opts = SimpleFileOptions::default()
                .compression_method(method)
                .last_modified_time(mtime)
                .unix_permissions(0o644);
            zip.start_file(name, opts)?;
            zip.write_all(bytes).map_err(zip::result::ZipError::Io)?;
            Ok(())
        };

        write_one(
            &mut zip,
            "[Content_Types].xml",
            content_types.as_bytes(),
            Compress::Deflate,
        )?;
        write_one(&mut zip, "_rels/.rels", root_rels_xml.as_bytes(), Compress::Deflate)?;

        // Canonical part ordering makes output independent of mapper call order.
        self.parts.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        // Take ownership of parts so the closure borrow above is released.
        let parts = std::mem::take(&mut self.parts);
        for (name, bytes, c) in &parts {
            write_one(&mut zip, name, bytes, *c)?;
        }

        Ok(zip.finish()?.into_inner())
    }

    fn validate(&self, root_rels: &Rels) -> Result<(), PackageError> {
        if let Some((extension, first, second)) = self.default_conflicts.first() {
            return Err(PackageError::ConflictingDefaultContentType {
                extension: extension.clone(),
                first,
                second,
            });
        }

        let mut parts = BTreeMap::<&str, ()>::new();
        for (name, _, _) in &self.parts {
            if !valid_part_name(name) {
                return Err(PackageError::InvalidPartName(name.clone()));
            }
            if parts.insert(name, ()).is_some() {
                return Err(PackageError::DuplicatePart(name.clone()));
            }
        }

        let mut overrides = BTreeMap::<&str, &'static str>::new();
        for (name, content_type) in &self.overrides {
            if let Some(first) = overrides.insert(name, content_type)
                && first != *content_type
            {
                return Err(PackageError::ConflictingOverrideContentType {
                    part_name: name.clone(),
                    first,
                    second: content_type,
                });
            }
        }

        validate_relationships(None, root_rels, &parts)?;
        for (source_part, rels) in &self.relationship_sets {
            if !parts.contains_key(source_part.as_str()) {
                return Err(PackageError::MissingRelationshipOwner {
                    source_part: source_part.clone(),
                });
            }
            validate_relationships(Some(source_part), rels, &parts)?;
        }
        self.validate_relationship_references()?;
        Ok(())
    }

    /// Proves that relationship IDs referenced by XML belong to that exact
    /// source part. Target validation alone cannot catch a stale or cross-part
    /// `r:id`, `r:embed`, or `r:link` in the serialized markup.
    fn validate_relationship_references(&self) -> Result<(), PackageError> {
        let relationship_sets = self
            .relationship_sets
            .iter()
            .map(|(source, rels)| (source.as_str(), rels))
            .collect::<BTreeMap<_, _>>();

        for (part_name, bytes, _) in &self.parts {
            if !part_name.ends_with(".xml") {
                continue;
            }
            let body =
                std::str::from_utf8(bytes).map_err(|err| PackageError::InvalidXml {
                    part_name: part_name.clone(),
                    message: err.to_string(),
                })?;
            let document = roxmltree::Document::parse(body).map_err(|err| {
                PackageError::InvalidXml {
                    part_name: part_name.clone(),
                    message: err.to_string(),
                }
            })?;
            for attribute in document
                .descendants()
                .filter(|node| node.is_element())
                .flat_map(|node| node.attributes())
                .filter(|attribute| {
                    attribute.namespace() == Some(ns::R)
                        && matches!(attribute.name(), "id" | "embed" | "link")
                })
            {
                let relationship_id = attribute.value();
                let exists =
                    relationship_sets.get(part_name.as_str()).is_some_and(|rels| {
                        rels.entries.iter().any(|entry| entry.id == relationship_id)
                    });
                if !exists {
                    return Err(PackageError::MissingRelationshipReference {
                        source_part: part_name.clone(),
                        relationship_id: relationship_id.into(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn validate_relationships(
    source_part: Option<&str>,
    rels: &Rels,
    parts: &BTreeMap<&str, ()>,
) -> Result<(), PackageError> {
    let source_name = source_part.unwrap_or("<package>");
    for entry in &rels.entries {
        if entry.mode == RelMode::External {
            continue;
        }
        let Some(resolved) = resolve_relationship_target(source_part, &entry.target)
        else {
            return Err(PackageError::InvalidRelationshipTarget {
                source_part: source_name.into(),
                target: entry.target.to_string(),
            });
        };
        if !parts.contains_key(resolved.as_str()) {
            return Err(PackageError::MissingRelationshipTarget {
                source_part: source_name.into(),
                target: entry.target.to_string(),
                resolved,
            });
        }
    }
    Ok(())
}

fn resolve_relationship_target(
    source_part: Option<&str>,
    target: &str,
) -> Option<String> {
    let target = target.split(['#', '?']).next()?;
    if target.is_empty() || target.contains('\\') {
        return None;
    }

    let mut components = Vec::<&str>::new();
    if !target.starts_with('/')
        && let Some(source) = source_part
        && let Some((dir, _)) = source.rsplit_once('/')
    {
        components.extend(dir.split('/'));
    }
    for component in target.trim_start_matches('/').split('/') {
        match component {
            "" | "." => return None,
            ".." => {
                components.pop()?;
            }
            component => components.push(component),
        }
    }
    let resolved = components.join("/");
    valid_part_name(&resolved).then_some(resolved)
}

fn insert_default(
    defaults: &mut BTreeMap<String, &'static str>,
    conflicts: &mut Vec<(String, &'static str, &'static str)>,
    extension: &str,
    content_type: &'static str,
) {
    let extension = extension.to_ascii_lowercase();
    if let Some(first) = defaults.get(&extension) {
        if *first != content_type {
            conflicts.push((extension, *first, content_type));
        }
    } else {
        defaults.insert(extension, content_type);
    }
}

fn valid_part_name(name: &str) -> bool {
    !name.is_empty()
        && name != "[Content_Types].xml"
        && name != "_rels/.rels"
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains('\\')
        && name.split('/').all(|component| {
            !component.is_empty() && component != "." && component != ".."
        })
}

impl Default for Package {
    fn default() -> Self {
        Self::new(PackageOptions { rels_overrides: true, media_defaults: &[] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> Package {
        Package::default()
    }

    #[test]
    fn duplicate_parts_are_rejected() {
        let mut package = package();
        package.add_xml("word/document.xml", "application/xml", "one".into());
        package.add_xml("word/document.xml", "application/xml", "two".into());
        assert!(matches!(
            package.finish(&Rels::new()),
            Err(PackageError::DuplicatePart(name)) if name == "word/document.xml"
        ));
    }

    #[test]
    fn invalid_part_names_are_rejected() {
        let mut package = package();
        package.add_xml("word/../document.xml", "application/xml", String::new());
        assert!(matches!(
            package.finish(&Rels::new()),
            Err(PackageError::InvalidPartName(name)) if name == "word/../document.xml"
        ));
    }

    #[test]
    fn conflicting_media_defaults_are_rejected() {
        let mut package = package();
        package.add_media("word/media/a.png", "png", "image/png", vec![]);
        package.add_media("word/media/b.png", "PNG", "image/not-png", vec![]);
        assert!(matches!(
            package.finish(&Rels::new()),
            Err(PackageError::ConflictingDefaultContentType { extension, .. })
                if extension == "png"
        ));
    }

    #[test]
    fn package_bytes_are_independent_of_part_insertion_order() {
        let mut a = package();
        a.add_xml("word/z.xml", "application/z+xml", "<z/>".into());
        a.add_xml("word/a.xml", "application/a+xml", "<a/>".into());

        let mut b = package();
        b.add_xml("word/a.xml", "application/a+xml", "<a/>".into());
        b.add_xml("word/z.xml", "application/z+xml", "<z/>".into());

        assert_eq!(a.finish(&Rels::new()).unwrap(), b.finish(&Rels::new()).unwrap());
    }

    #[test]
    fn missing_internal_relationship_targets_are_rejected() {
        let mut package = package();
        package.add_xml("word/document.xml", "application/xml", String::new());
        let mut root = Rels::new();
        root.add("office-document", "word/missing.xml", RelMode::Internal);
        assert!(matches!(
            package.finish(&root),
            Err(PackageError::MissingRelationshipTarget { resolved, .. })
                if resolved == "word/missing.xml"
        ));
    }

    #[test]
    fn owned_relationship_targets_resolve_relative_to_the_source_part() {
        let mut package = package();
        package.add_xml("word/document.xml", "application/xml", "<document/>".into());
        package.add_xml("word/media/image1.png", "image/png", "<image/>".into());
        let mut rels = Rels::new();
        rels.add("image", "media/image1.png", RelMode::Internal);
        package.add_relationships("word/document.xml", &rels).unwrap();

        let mut root = Rels::new();
        root.add("office-document", "word/document.xml", RelMode::Internal);
        assert!(package.finish(&root).is_ok());
    }

    #[test]
    fn relationship_mode_participates_in_deduplication() {
        let mut rels = Rels::new();
        let internal = rels.add("kind", "same", RelMode::Internal);
        let external = rels.add("kind", "same", RelMode::External);
        assert_ne!(internal, external);
        assert_eq!(rels.to_xml().matches("<Relationship ").count(), 2);
    }

    #[test]
    fn missing_referenced_relationship_ids_are_rejected() {
        let mut package = package();
        package.add_xml(
            "word/document.xml",
            "application/xml",
            format!("<document xmlns:r=\"{}\" r:id=\"rId9\"/>", ns::R),
        );
        assert!(matches!(
            package.finish(&Rels::new()),
            Err(PackageError::MissingRelationshipReference {
                source_part,
                relationship_id,
            }) if source_part == "word/document.xml" && relationship_id == "rId9"
        ));
    }

    #[test]
    fn referenced_relationship_ids_are_scoped_to_the_owning_part() {
        let mut package = package();
        package.add_xml(
            "word/document.xml",
            "application/xml",
            format!("<document xmlns:r=\"{}\" r:id=\"rId1\"/>", ns::R),
        );
        package.add_xml("word/target.xml", "application/xml", "<target/>".into());
        let mut rels = Rels::new();
        assert_eq!(rels.add("kind", "target.xml", RelMode::Internal), "rId1");
        package.add_relationships("word/document.xml", &rels).unwrap();
        assert!(package.finish(&Rels::new()).is_ok());
    }

    #[test]
    fn rollback_drops_speculative_relationships_without_recycling_ids() {
        let mut rels = Rels::new();
        assert_eq!(rels.add("kind", "keep.xml", RelMode::Internal), "rId1");

        // A part the caller ends up not writing.
        let savepoint = rels.savepoint();
        assert_eq!(rels.add("kind", "abandoned.xml", RelMode::Internal), "rId2");
        rels.rollback(savepoint);

        // The abandoned target is gone from the emitted table, and asking for it
        // again allocates a fresh record rather than handing back the stale
        // `rId2` from the dedupe index.
        let xml = rels.to_xml();
        assert!(!xml.contains("abandoned.xml"), "{xml}");
        assert!(xml.contains("keep.xml"), "{xml}");
        assert_eq!(rels.add("kind", "abandoned.xml", RelMode::Internal), "rId3");
        assert!(rels.to_xml().contains("abandoned.xml"));
    }

    #[test]
    fn rollback_to_an_empty_savepoint_clears_the_table() {
        let mut rels = Rels::new();
        let savepoint = rels.savepoint();
        rels.add("kind", "a.xml", RelMode::Internal);
        rels.add("kind", "b.xml", RelMode::Internal);
        assert!(!rels.is_empty());
        rels.rollback(savepoint);
        assert!(rels.is_empty());
    }

    #[test]
    fn a_relationship_to_a_missing_part_is_rejected() {
        let mut package = package();
        package.add_xml(
            "word/document.xml",
            "application/xml",
            format!("<document xmlns:r=\"{}\" r:id=\"rId1\"/>", ns::R),
        );
        let mut rels = Rels::new();
        rels.add("kind", "header4.xml", RelMode::Internal);
        package.add_relationships("word/document.xml", &rels).unwrap();
        assert!(matches!(
            package.finish(&Rels::new()),
            Err(PackageError::MissingRelationshipTarget { target, .. })
                if target == "header4.xml"
        ));
    }

    #[test]
    fn malformed_xml_parts_are_rejected() {
        let mut package = package();
        package.add_xml("word/document.xml", "application/xml", "<document>".into());
        assert!(matches!(
            package.finish(&Rels::new()),
            Err(PackageError::InvalidXml { part_name, .. })
                if part_name == "word/document.xml"
        ));
    }
}
