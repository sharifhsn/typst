use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use ecow::eco_format;
use serde::Serialize;
use tempfile::NamedTempFile;
use typst::diag::{HintedStrResult, bail};
use typst_docx_import::report::{Note, Severity};

use crate::args::{DocxChartMode, DocxTrackedMode, ImportCommand, PptxFidelityMode};

// Keep the user-facing preflight aligned with `typst_ooxml_core::opc::Reader`.
// The shared reader independently enforces archive, expanded-size, entry-count,
// part-size, XML-depth, and XXE limits for both importers.
const MAX_OFFICE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum OfficeFormat {
    Docx,
    Pptx,
}

impl OfficeFormat {
    fn infer(path: &Path) -> HintedStrResult<Self> {
        match path.extension().and_then(OsStr::to_str).map(str::to_ascii_lowercase) {
            Some(ext) if ext == "docx" => Ok(Self::Docx),
            Some(ext) if ext == "pptx" => Ok(Self::Pptx),
            _ => bail!(
                "cannot infer Office format from {}; expected a .docx or .pptx input",
                path.display()
            ),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Pptx => "pptx",
        }
    }
}

#[derive(Serialize)]
struct LossReport<'a> {
    format: &'static str,
    input: String,
    output: String,
    approximated: usize,
    dropped: usize,
    entries: Vec<LossEntry<'a>>,
}

#[derive(Serialize)]
struct LossEntry<'a> {
    severity: &'static str,
    what: &'a str,
    detail: &'a str,
}

impl<'a> LossReport<'a> {
    fn new(
        format: OfficeFormat,
        input: &Path,
        output: &Path,
        entries: &'a [Note],
    ) -> Self {
        let approximated = entries
            .iter()
            .filter(|entry| entry.severity == Severity::Approximate)
            .count();
        let dropped = entries
            .iter()
            .filter(|entry| entry.severity == Severity::Drop)
            .count();
        let entries = entries
            .iter()
            .map(|entry| LossEntry {
                severity: match entry.severity {
                    Severity::Approximate => "approximated",
                    Severity::Drop => "dropped",
                },
                what: &entry.what,
                detail: &entry.detail,
            })
            .collect();
        Self {
            format: format.name(),
            input: input.display().to_string(),
            output: output.display().to_string(),
            approximated,
            dropped,
            entries,
        }
    }
}

pub fn import(command: &ImportCommand) -> HintedStrResult<()> {
    let format = OfficeFormat::infer(&command.input)?;
    if !command
        .output
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("typ"))
    {
        bail!("import output must have a .typ extension");
    }

    let metadata = fs::metadata(&command.input).map_err(|error| {
        eco_format!("failed to inspect {} ({error})", command.input.display())
    })?;
    if metadata.len() > MAX_OFFICE_BYTES {
        bail!("Office input exceeds the 128 MiB import limit");
    }
    let bytes = fs::read(&command.input).map_err(|error| {
        eco_format!("failed to read {} ({error})", command.input.display())
    })?;

    let (source, assets, entries) = match format {
        OfficeFormat::Docx => {
            let options = typst_docx_import::ImportOptions {
                charts: match command.docx_charts {
                    DocxChartMode::Table => typst_docx_import::ChartStyle::Table,
                    DocxChartMode::Plot => typst_docx_import::ChartStyle::Plot,
                },
                tracked: match command.docx_tracked {
                    DocxTrackedMode::Preserve => {
                        typst_docx_import::TrackedChanges::Preserve
                    }
                    DocxTrackedMode::Accept => typst_docx_import::TrackedChanges::Accept,
                },
                ..Default::default()
            };
            let result = typst_docx_import::import_docx_with(&bytes, &options)
                .map_err(|error| eco_format!("failed to import DOCX ({error})"))?;
            (result.source, result.assets, result.report.notes)
        }
        OfficeFormat::Pptx => {
            let options = typst_pptx_import::ImportOptions {
                fidelity: match command.pptx_fidelity {
                    PptxFidelityMode::Placed => typst_pptx_import::Fidelity::Placed,
                    PptxFidelityMode::Idiomatic => typst_pptx_import::Fidelity::Idiomatic,
                },
                ..Default::default()
            };
            let result = typst_pptx_import::import_pptx_with(&bytes, &options)
                .map_err(|error| eco_format!("failed to import PPTX ({error})"))?;
            let entries = result.report.entries().to_vec();
            (result.source, result.assets, entries)
        }
    };

    let asset_count = assets.len();
    let output_parent = parent_or_current(&command.output);
    let mut files = vec![(command.output.clone(), source.into_bytes())];
    for (relative, data) in assets {
        validate_asset_path(&relative)?;
        let target = output_parent.join(relative);
        ensure_no_symlink_parent(output_parent, &target)?;
        files.push((target, data));
    }

    let report = LossReport::new(format, &command.input, &command.output, &entries);
    let report_json = serde_json::to_vec_pretty(&report)
        .map_err(|error| eco_format!("failed to serialize import report ({error})"))?;
    if let Some(path) = command.report.as_deref().filter(|path| *path != Path::new("-")) {
        files.push((path.to_owned(), report_json.clone()));
    }

    write_all_noclobber(files)?;

    eprintln!(
        "wrote {} ({} assets, {} approximated, {} dropped)",
        command.output.display(),
        asset_count,
        report.approximated,
        report.dropped,
    );
    if !entries.is_empty() {
        eprintln!("import notes:");
        for entry in &entries {
            let severity = match entry.severity {
                Severity::Approximate => "approximated",
                Severity::Drop => "dropped",
            };
            eprintln!("- [{severity}] {}: {}", entry.what, entry.detail);
        }
    }
    if command.report.as_deref() == Some(Path::new("-")) {
        println!("{}", String::from_utf8_lossy(&report_json));
    }
    Ok(())
}

fn validate_asset_path(path: &Path) -> HintedStrResult<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("importer produced an unsafe asset path: {}", path.display());
    }
    Ok(())
}

fn ensure_no_symlink_parent(base: &Path, target: &Path) -> HintedStrResult<()> {
    let relative = target.strip_prefix(base).map_err(|_| {
        eco_format!("import asset escapes the output directory: {}", target.display())
    })?;
    let mut current = base.to_owned();
    for component in relative.components().take(relative.components().count() - 1) {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "refusing to write import assets through symbolic link {}",
                    current.display()
                )
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                bail!(
                    "failed to inspect import destination {} ({error})",
                    current.display()
                )
            }
        }
    }
    Ok(())
}

fn write_all_noclobber(files: Vec<(PathBuf, Vec<u8>)>) -> HintedStrResult<()> {
    let mut destinations = HashSet::new();
    for (path, _) in &files {
        if !destinations.insert(path.clone()) {
            bail!("multiple import outputs resolve to {}", path.display());
        }
        if path.exists() {
            bail!("refusing to overwrite existing file {}", path.display());
        }
    }

    let mut staged = Vec::with_capacity(files.len());
    for (path, data) in files {
        let parent = parent_or_current(&path);
        fs::create_dir_all(parent).map_err(|error| {
            eco_format!(
                "failed to create import output directory {} ({error})",
                parent.display()
            )
        })?;
        let mut file = NamedTempFile::new_in(parent).map_err(|error| {
            eco_format!("failed to stage import output in {} ({error})", parent.display())
        })?;
        file.write_all(&data).map_err(|error| {
            eco_format!("failed to stage import output {} ({error})", path.display())
        })?;
        staged.push((path, file));
    }

    let mut committed = Vec::new();
    for (path, file) in staged {
        if let Err(error) = file.persist_noclobber(&path) {
            for committed_path in committed {
                let _ = fs::remove_file(committed_path);
            }
            bail!("failed to create import output {} ({})", path.display(), error.error);
        }
        committed.push(path);
    }
    Ok(())
}

fn parent_or_current(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_office_format_case_insensitively() {
        assert_eq!(
            OfficeFormat::infer(Path::new("report.DOCX")).unwrap(),
            OfficeFormat::Docx
        );
        assert_eq!(
            OfficeFormat::infer(Path::new("slides.PpTx")).unwrap(),
            OfficeFormat::Pptx
        );
        assert!(OfficeFormat::infer(Path::new("notes.pdf")).is_err());
    }

    #[test]
    fn rejects_asset_path_traversal() {
        assert!(validate_asset_path(Path::new("assets/image.png")).is_ok());
        assert!(validate_asset_path(Path::new("../outside.png")).is_err());
        assert!(validate_asset_path(Path::new("/outside.png")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_asset_writes_through_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("assets")).unwrap();
        let target = root.path().join("assets/image.png");

        assert!(ensure_no_symlink_parent(root.path(), &target).is_err());
    }
}
