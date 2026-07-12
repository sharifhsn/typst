use std::collections::HashMap;
use std::fs;
use std::path::Path;

use ecow::eco_format;
use typst::diag::{HintedStrResult, bail};
use typst_docx_roundtrip::{RoundtripState, apply_atomic, dry_run, parse_docx};

use crate::args::ReviewCommand;

pub fn review(command: &ReviewCommand) -> HintedStrResult<()> {
    let root = command
        .root
        .canonicalize()
        .map_err(|error| eco_format!("failed to resolve project root ({error})"))?;
    let state = RoundtripState::from_json(
        &fs::read(&command.state)
            .map_err(|error| eco_format!("failed to read review state ({error})"))?,
    )
    .map_err(|error| eco_format!("{error}"))?;
    const MAX_REVIEW_DOCX_BYTES: u64 = 64 * 1024 * 1024;
    let docx_metadata = fs::metadata(&command.docx)
        .map_err(|error| eco_format!("failed to inspect edited DOCX ({error})"))?;
    if docx_metadata.len() > MAX_REVIEW_DOCX_BYTES {
        bail!("edited DOCX exceeds the 64 MiB review limit");
    }
    let edits = parse_docx(
        &fs::read(&command.docx)
            .map_err(|error| eco_format!("failed to read edited DOCX ({error})"))?,
        &state,
    )
    .map_err(|error| eco_format!("{error}"))?;

    let current = load_current_files(&root, &state)?;
    let report =
        dry_run(&state, &edits, &current).map_err(|error| eco_format!("{error}"))?;
    let json = serde_json::to_vec_pretty(&report)
        .map_err(|error| eco_format!("failed to serialize merge report ({error})"))?;

    if let Some(path) = &command.report {
        let report_path = canonical_destination(path).map_err(|error| {
            eco_format!("failed to resolve merge report path ({error})")
        })?;
        for baseline in &state.files {
            let source = root.join(&baseline.path).canonicalize().map_err(|error| {
                eco_format!("failed to resolve review source {} ({error})", baseline.path)
            })?;
            if source == report_path {
                bail!("merge report path aliases a Typst source file");
            }
        }
        fs::write(path, &json)
            .map_err(|error| eco_format!("failed to write merge report ({error})"))?;
    } else {
        println!("{}", String::from_utf8_lossy(&json));
    }

    if command.apply {
        if !report.can_apply() {
            bail!("review contains conflicts; no source files were changed");
        }
        apply_atomic(&root, &report).map_err(|error| eco_format!("{error}"))?;
    }
    Ok(())
}

fn canonical_destination(path: &Path) -> std::io::Result<std::path::PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    Ok(parent.canonicalize()?.join(absolute.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
    })?))
}

fn load_current_files(
    root: &Path,
    state: &RoundtripState,
) -> HintedStrResult<HashMap<String, String>> {
    let mut current = HashMap::new();
    for baseline in &state.files {
        let path = root.join(&baseline.path);
        if fs::symlink_metadata(&path)
            .map_err(|error| {
                eco_format!("failed to inspect {} ({error})", path.display())
            })?
            .file_type()
            .is_symlink()
        {
            bail!("review source must not be a symbolic link: {}", baseline.path);
        }
        let canonical = path.canonicalize().map_err(|error| {
            eco_format!("failed to resolve {} ({error})", path.display())
        })?;
        if !canonical.starts_with(root) {
            bail!("review source escapes the project root: {}", baseline.path);
        }
        let text = fs::read_to_string(&canonical)
            .map_err(|error| eco_format!("failed to read {} ({error})", baseline.path))?;
        current.insert(baseline.path.clone(), text);
    }
    Ok(current)
}
