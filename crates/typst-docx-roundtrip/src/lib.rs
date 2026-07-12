//! Safe, deliberately conservative support for merging edits made in Word back
//! into Typst source files.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use similar::{DiffTag, TextDiff};
use zip::ZipArchive;

const DOCUMENT_XML: &str = "word/document.xml";
const TAG_PREFIX: &str = "typst:v1:";
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_XML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

/// The complete, self-contained JSON state stored alongside an exported DOCX.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoundtripState {
    pub export_id: String,
    /// Project-relative path of the main Typst source file.
    pub main: String,
    pub files: Vec<BaselineFile>,
    pub regions: Vec<Region>,
}

/// A full UTF-8 source baseline, including its SHA-256 digest.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BaselineFile {
    pub path: String,
    pub text: String,
    pub sha256: String,
}

/// A source range represented by byte offsets into a baseline file.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub id: String,
    pub file: String,
    pub baseline_start: usize,
    pub baseline_end: usize,
    pub source: String,
    /// Visible text in Word at export time. Newlines separate paragraphs.
    pub word_baseline: String,
    pub kind: RegionKind,
}

/// A versioned review-region discriminator validated against the kinds whose
/// source replacement semantics this engine understands.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct RegionKind(pub String);

impl RoundtripState {
    pub fn from_json(bytes: &[u8]) -> Result<Self, Error> {
        let state: Self = serde_json::from_slice(bytes).map_err(Error::StateJson)?;
        state.validate()?;
        Ok(state)
    }

    pub fn to_json(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        serde_json::to_vec_pretty(self).map_err(Error::StateJson)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.export_id.is_empty() || self.export_id.contains(':') {
            return Err(Error::InvalidState(
                "export_id must be non-empty and contain no colon",
            ));
        }
        validate_relative_path(&self.main)?;
        let mut files = HashMap::new();
        for file in &self.files {
            validate_relative_path(&file.path)?;
            if files.insert(file.path.as_str(), file).is_some() {
                return Err(Error::InvalidState("duplicate file path"));
            }
            if sha256(file.text.as_bytes()) != file.sha256 {
                return Err(Error::InvalidState("baseline file SHA-256 mismatch"));
            }
        }
        if !files.contains_key(self.main.as_str()) {
            return Err(Error::InvalidState("main file is absent from files"));
        }

        let mut ids = HashSet::new();
        for region in &self.regions {
            if region.id.is_empty() || region.id.contains(':') || !ids.insert(&region.id)
            {
                return Err(Error::InvalidState(
                    "region ids must be unique, non-empty, and contain no colon",
                ));
            }
            let file = files
                .get(region.file.as_str())
                .ok_or(Error::InvalidState("region references an unknown file"))?;
            let range = region.baseline_start..region.baseline_end;
            let source = file
                .text
                .get(range)
                .ok_or(Error::InvalidState("region has an invalid UTF-8 byte range"))?;
            if source != region.source {
                return Err(Error::InvalidState(
                    "region source does not match its baseline range",
                ));
            }
            if !matches!(
                region.kind.0.as_str(),
                "heading" | "paragraph" | "list_item" | "table_cell"
            ) {
                return Err(Error::InvalidState("unsupported review region kind"));
            }
        }
        Ok(())
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The final visible content of every round-trip content control in a DOCX.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordEdits {
    pub regions: HashMap<String, String>,
}

/// Parse and strictly validate the tagged content controls in a DOCX.
pub fn parse_docx(docx: &[u8], state: &RoundtripState) -> Result<WordEdits, Error> {
    state.validate()?;
    if docx.len() > MAX_ARCHIVE_BYTES {
        return Err(Error::UnsafeDocx("archive is too large"));
    }
    let mut archive = ZipArchive::new(Cursor::new(docx)).map_err(Error::Zip)?;
    if archive.len() > MAX_ENTRIES {
        return Err(Error::UnsafeDocx("archive has too many entries"));
    }
    let mut expanded = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(Error::Zip)?;
        expanded = expanded
            .checked_add(entry.size())
            .ok_or(Error::UnsafeDocx("expanded size overflow"))?;
        if expanded > MAX_EXPANDED_BYTES {
            return Err(Error::UnsafeDocx("expanded archive is too large"));
        }
    }
    let entry = archive
        .by_name(DOCUMENT_XML)
        .map_err(|_| Error::UnsafeDocx("word/document.xml is missing"))?;
    if entry.size() > MAX_XML_BYTES {
        return Err(Error::UnsafeDocx("word/document.xml is too large"));
    }
    let mut xml = String::new();
    entry
        .take(MAX_XML_BYTES + 1)
        .read_to_string(&mut xml)
        .map_err(Error::Io)?;
    if xml.len() as u64 > MAX_XML_BYTES {
        return Err(Error::UnsafeDocx("word/document.xml is too large"));
    }
    let lowered = xml.to_ascii_lowercase();
    if lowered.contains("<!doctype") || lowered.contains("<!entity") {
        return Err(Error::UnsafeDocx("DOCTYPE and ENTITY declarations are forbidden"));
    }
    let document = Document::parse(&xml).map_err(Error::Xml)?;
    let expected: HashMap<_, _> =
        state.regions.iter().map(|r| (r.id.as_str(), r)).collect();
    let wanted_prefix = format!("{TAG_PREFIX}{}:", state.export_id);
    let mut regions = HashMap::new();

    for sdt in document.descendants().filter(|node| is_element(*node, "sdt")) {
        let Some(tag) = sdt
            .descendants()
            .find(|node| is_element(*node, "tag"))
            .and_then(|node| attribute(node, "val"))
        else {
            continue;
        };
        if !tag.starts_with(TAG_PREFIX) {
            continue;
        }
        let Some(id) = tag.strip_prefix(&wanted_prefix) else {
            return Err(Error::ForeignControl(tag.to_owned()));
        };
        let region = expected
            .get(id)
            .ok_or_else(|| Error::ForeignControl(tag.to_owned()))?;
        if regions.contains_key(id) {
            return Err(Error::DuplicateControl(id.to_owned()));
        }
        let content = sdt
            .children()
            .find(|node| is_element(*node, "sdtContent"))
            .ok_or_else(|| Error::StructuralEdit(id.to_owned()))?;
        let paragraphs: Vec<_> =
            content.descendants().filter(|node| is_element(*node, "p")).collect();
        let expected_paragraphs = region.word_baseline.split('\n').count();
        if paragraphs.len() != expected_paragraphs {
            return Err(Error::StructuralEdit(id.to_owned()));
        }
        if content
            .descendants()
            .filter(|node| node.is_element() && *node != content)
            .any(|node| !allowed_review_element(node))
        {
            return Err(Error::StructuralEdit(id.to_owned()));
        }
        let text = paragraphs
            .into_iter()
            .map(visible_text)
            .collect::<Vec<_>>()
            .join("\n");
        if text.contains(['\r', '\n']) {
            return Err(Error::StructuralEdit(id.to_owned()));
        }
        regions.insert(id.to_owned(), text);
    }
    for region in &state.regions {
        if !regions.contains_key(&region.id) {
            return Err(Error::MissingControl(region.id.clone()));
        }
    }
    Ok(WordEdits { regions })
}

fn visible_text(paragraph: Node<'_, '_>) -> String {
    paragraph
        .descendants()
        .filter(|node| is_element(*node, "t"))
        .filter(|node| {
            !node.ancestors().any(|ancestor| {
                is_element(ancestor, "del") || is_element(ancestor, "moveFrom")
            })
        })
        .filter_map(|node| node.text())
        .collect()
}

fn allowed_review_element(node: Node<'_, '_>) -> bool {
    if node.ancestors().any(|ancestor| is_element(ancestor, "pPr"))
        && matches!(node.tag_name().name(), "del" | "ins" | "moveFrom" | "moveTo")
    {
        return false;
    }
    if node
        .ancestors()
        .any(|ancestor| is_element(ancestor, "rPr") || is_element(ancestor, "pPr"))
    {
        return true;
    }
    matches!(
        node.tag_name().name(),
        "p" | "pPr"
            | "r"
            | "rPr"
            | "t"
            | "delText"
            | "ins"
            | "del"
            | "moveTo"
            | "moveFrom"
            | "bookmarkStart"
            | "bookmarkEnd"
            | "proofErr"
            | "permStart"
            | "permEnd"
    )
}

fn is_element(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn attribute<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attr| attr.name() == name)
        .map(|attr| attr.value())
}

/// A dry-run report. No file is changed until [`apply_atomic`] is called.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MergeReport {
    pub regions: Vec<RegionReport>,
    #[serde(skip)]
    files: Vec<PlannedFile>,
}

impl MergeReport {
    pub fn can_apply(&self) -> bool {
        self.regions.iter().all(|region| {
            matches!(region.status, RegionStatus::Ready | RegionStatus::Unchanged)
        })
    }

    pub fn changed_files(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|file| file.path.as_str())
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RegionReport {
    pub id: String,
    pub file: String,
    pub baseline: String,
    pub current: Option<String>,
    pub word: String,
    pub status: RegionStatus,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegionStatus {
    Unchanged,
    Ready,
    Conflict(ConflictKind),
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    LocalOverlap,
    AmbiguousLocation,
    OverlappingRegions,
}

type Replacement = (Range<usize>, String, usize);

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlannedFile {
    path: String,
    expected_sha256: String,
    contents: String,
}

/// Build a merge plan from current UTF-8 source files without mutating them.
pub fn dry_run(
    state: &RoundtripState,
    edits: &WordEdits,
    current: &HashMap<String, String>,
) -> Result<MergeReport, Error> {
    state.validate()?;
    let mut reports = Vec::with_capacity(state.regions.len());
    let mut replacements: BTreeMap<&str, Vec<Replacement>> = BTreeMap::new();

    for (index, region) in state.regions.iter().enumerate() {
        let word = edits
            .regions
            .get(&region.id)
            .ok_or_else(|| Error::MissingControl(region.id.clone()))?;
        let file = current
            .get(&region.file)
            .ok_or_else(|| Error::MissingCurrentFile(region.file.clone()))?;
        let baseline = state
            .files
            .iter()
            .find(|baseline| baseline.path == region.file)
            .expect("validated region file");
        let location = locate_region(file, baseline, region);
        let mut status = RegionStatus::Unchanged;
        let mut current_region = match &location {
            Location::Unique(range) => file.get(range.clone()).map(str::to_owned),
            Location::Missing | Location::Ambiguous => None,
        };
        if word != &region.word_baseline {
            match location {
                Location::Unique(range) => {
                    current_region = file.get(range.clone()).map(str::to_owned);
                    replacements.entry(&region.file).or_default().push((
                        range,
                        encode_region_text(&region.kind, word),
                        index,
                    ));
                    status = RegionStatus::Ready;
                }
                Location::Missing => {
                    current_region = None;
                    status = RegionStatus::Conflict(ConflictKind::LocalOverlap);
                }
                Location::Ambiguous => {
                    current_region = None;
                    status = RegionStatus::Conflict(ConflictKind::AmbiguousLocation);
                }
            }
        }
        reports.push(RegionReport {
            id: region.id.clone(),
            file: region.file.clone(),
            baseline: region.source.clone(),
            current: current_region,
            word: word.clone(),
            status,
        });
    }

    let mut files = Vec::new();
    for (path, mut edits) in replacements {
        edits.sort_by_key(|(range, _, _)| (range.start, range.end));
        for pair in edits.windows(2) {
            if pair[0].0.end > pair[1].0.start {
                reports[pair[0].2].status =
                    RegionStatus::Conflict(ConflictKind::OverlappingRegions);
                reports[pair[1].2].status =
                    RegionStatus::Conflict(ConflictKind::OverlappingRegions);
            }
        }
        if edits.iter().any(|(_, _, index)| {
            matches!(reports[*index].status, RegionStatus::Conflict(_))
        }) {
            continue;
        }
        let original = &current[path];
        let mut contents = original.clone();
        for (range, replacement, _) in edits.into_iter().rev() {
            contents.replace_range(range, &replacement);
        }
        if &contents != original {
            files.push(PlannedFile {
                path: path.to_owned(),
                expected_sha256: sha256(original.as_bytes()),
                contents,
            });
        }
    }
    Ok(MergeReport { regions: reports, files })
}

enum Location {
    Unique(Range<usize>),
    Missing,
    Ambiguous,
}

fn locate_region(current: &str, baseline: &BaselineFile, region: &Region) -> Location {
    let original = region.baseline_start..region.baseline_end;
    if sha256(current.as_bytes()) == baseline.sha256 {
        return Location::Unique(original);
    }

    // Map only through a baseline-to-current equal diff region. A global text
    // search can corrupt an unrelated occurrence when the student edits the
    // original region but the same words remain elsewhere in the file.
    let old_boundaries = char_boundaries(&baseline.text);
    let new_boundaries = char_boundaries(current);
    let Ok(old_start) = old_boundaries.binary_search(&original.start) else {
        return Location::Missing;
    };
    let Ok(old_end) = old_boundaries.binary_search(&original.end) else {
        return Location::Missing;
    };
    const CONTEXT_CHARS: usize = 8;
    let required_start = old_start.saturating_sub(CONTEXT_CHARS);
    let required_end = (old_end + CONTEXT_CHARS).min(old_boundaries.len() - 1);
    let diff = TextDiff::from_chars(&baseline.text, current);
    let mut mapped = None;
    for op in diff.ops().iter().filter(|op| op.tag() == DiffTag::Equal) {
        let old = op.old_range();
        if required_start < old.start || required_end > old.end {
            continue;
        }
        let new = op.new_range();
        let start_index = new.start + (old_start - old.start);
        let end_index = start_index + (old_end - old_start);
        let Some((&start, &end)) =
            new_boundaries.get(start_index).zip(new_boundaries.get(end_index))
        else {
            return Location::Missing;
        };
        if mapped.replace(start..end).is_some() {
            return Location::Ambiguous;
        }
    }
    mapped.map_or(Location::Missing, Location::Unique)
}

fn char_boundaries(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(index, _)| index)
        .chain([text.len()])
        .collect()
}

/// Escape Word text so it is inserted as literal Typst markup text.
pub fn encode_typst_text(text: &str) -> String {
    format!("#({})", encode_typst_string(text))
}

fn encode_region_text(kind: &RegionKind, text: &str) -> String {
    let _ = kind;
    encode_typst_text(text)
}

fn encode_typst_string(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 10);
    escaped.push('"');
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            _ => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

/// Atomically apply a conflict-free plan beneath `project_root`.
///
/// Every source is re-hashed immediately before any rename. Temporary files are
/// written and synced first; a changed source therefore aborts without applying
/// stale merge output.
pub fn apply_atomic(project_root: &Path, report: &MergeReport) -> Result<(), Error> {
    if !report.regions.iter().all(|region| {
        matches!(region.status, RegionStatus::Ready | RegionStatus::Unchanged)
    }) {
        return Err(Error::ConflictedPlan);
    }
    let root = project_root.canonicalize().map_err(Error::Io)?;
    let mut prepared = Vec::new();
    for (index, file) in report.files.iter().enumerate() {
        validate_relative_path(&file.path)?;
        let target = root.join(&file.path);
        if fs::symlink_metadata(&target)
            .map_err(Error::Io)?
            .file_type()
            .is_symlink()
        {
            cleanup_temps(&prepared);
            return Err(Error::InvalidState("review source must not be a symbolic link"));
        }
        let current = fs::read(&target).map_err(Error::Io)?;
        let permissions = fs::metadata(&target).map_err(Error::Io)?.permissions();
        if sha256(&current) != file.expected_sha256 {
            cleanup_temps(&prepared);
            return Err(Error::SourceChanged(file.path.clone()));
        }
        let parent = target.parent().ok_or(Error::InvalidState("file has no parent"))?;
        let canonical_parent = parent.canonicalize().map_err(Error::Io)?;
        if !canonical_parent.starts_with(&root) {
            cleanup_temps(&prepared);
            return Err(Error::InvalidState("source path escapes the project root"));
        }
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::InvalidState("non-UTF-8 target path"))?;
        let temporary = parent
            .join(format!(".{name}.typst-roundtrip-{}-{index}", std::process::id()));
        let result = (|| {
            let mut output =
                fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
            output.set_permissions(permissions)?;
            output.write_all(file.contents.as_bytes())?;
            output.sync_all()
        })();
        if let Err(error) = result {
            cleanup_temps(&prepared);
            let _ = fs::remove_file(&temporary);
            return Err(Error::Io(error));
        }
        let backup = parent.join(format!(
            ".{name}.typst-roundtrip-backup-{}-{index}",
            std::process::id()
        ));
        prepared.push((temporary, target, backup, file.path.clone()));
    }
    // Recheck every input before creating any backup or replacing any source.
    for (_, target, _, path) in &prepared {
        let planned = report.files.iter().find(|file| file.path == *path).unwrap();
        if sha256(&fs::read(target).map_err(Error::Io)?) != planned.expected_sha256 {
            cleanup_temps(&prepared);
            return Err(Error::SourceChanged(planned.path.clone()));
        }
    }
    // Hard-link backups are created beside every source before the first
    // replacement. If a later rename fails, committed files can be restored
    // without copying or losing their original permissions and metadata.
    for index in 0..prepared.len() {
        let (_, target, backup, _) = &prepared[index];
        if fs::symlink_metadata(backup).is_ok() {
            cleanup_temps(&prepared);
            cleanup_backups(&prepared[..index]);
            return Err(Error::InvalidState("round-trip backup path already exists"));
        }
        if let Err(error) = fs::hard_link(target, backup) {
            cleanup_temps(&prepared);
            cleanup_backups(&prepared[..index]);
            return Err(Error::Io(error));
        }
    }
    for index in 0..prepared.len() {
        let (temporary, target, _, _) = &prepared[index];
        if let Err(error) = fs::rename(temporary, target) {
            for (_, committed_target, backup, _) in prepared[..index].iter().rev() {
                let _ = fs::rename(backup, committed_target);
            }
            cleanup_prepared(&prepared);
            return Err(Error::Io(error));
        }
    }
    cleanup_backups(&prepared);
    Ok(())
}

fn cleanup_temps(prepared: &[(PathBuf, PathBuf, PathBuf, String)]) {
    for (temporary, _, _, _) in prepared {
        let _ = fs::remove_file(temporary);
    }
}

fn cleanup_backups(prepared: &[(PathBuf, PathBuf, PathBuf, String)]) {
    for (_, _, backup, _) in prepared {
        let _ = fs::remove_file(backup);
    }
}

fn cleanup_prepared(prepared: &[(PathBuf, PathBuf, PathBuf, String)]) {
    cleanup_temps(prepared);
    cleanup_backups(prepared);
}

fn validate_relative_path(path: &str) -> Result<(), Error> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidState(
            "paths must be normalized project-relative paths",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub enum Error {
    StateJson(serde_json::Error),
    InvalidState(&'static str),
    Zip(zip::result::ZipError),
    Xml(roxmltree::Error),
    Io(std::io::Error),
    UnsafeDocx(&'static str),
    ForeignControl(String),
    MissingControl(String),
    DuplicateControl(String),
    StructuralEdit(String),
    MissingCurrentFile(String),
    ConflictedPlan,
    SourceChanged(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateJson(error) => write!(f, "invalid round-trip state JSON: {error}"),
            Self::InvalidState(message) => {
                write!(f, "invalid round-trip state: {message}")
            }
            Self::Zip(error) => write!(f, "invalid DOCX ZIP: {error}"),
            Self::Xml(error) => write!(f, "invalid document XML: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::UnsafeDocx(message) => write!(f, "unsafe DOCX: {message}"),
            Self::ForeignControl(tag) => {
                write!(f, "foreign round-trip content control: {tag}")
            }
            Self::MissingControl(id) => {
                write!(f, "missing content control for region {id}")
            }
            Self::DuplicateControl(id) => {
                write!(f, "duplicate content control for region {id}")
            }
            Self::StructuralEdit(id) => {
                write!(f, "structural paragraph edit in region {id}")
            }
            Self::MissingCurrentFile(path) => {
                write!(f, "current source file is missing: {path}")
            }
            Self::ConflictedPlan => f.write_str("merge plan contains conflicts"),
            Self::SourceChanged(path) => {
                write!(f, "source changed since dry run: {path}")
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests;
