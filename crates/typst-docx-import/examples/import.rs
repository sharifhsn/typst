//! CLI: `cargo run -p typst-docx-import --example import -- in.docx out.typ`
//!
//! Imports a `.docx` to Typst source, writes `out.typ` and its image assets
//! (under `out`'s parent dir), and prints the loss report to stderr.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Flags (`--literal`, `--charts=..`) may appear anywhere among the
    // arguments, not just after the two positional paths — so positional
    // arguments are gathered separately rather than read off fixed indices.
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if positional.len() != 2 {
        eprintln!("usage: import [--literal] [--charts=table|plot] <in.docx> <out.typ>");
        return ExitCode::from(2);
    }
    let in_path = positional[0];
    let out_path = PathBuf::from(positional[1]);
    let literal = args.iter().any(|a| a == "--literal");
    let charts_plot = match args.iter().find_map(|a| a.strip_prefix("--charts=")) {
        None | Some("table") => false,
        Some("plot") => true,
        Some(other) => {
            eprintln!(
                "error: unknown --charts value {other:?} (expected \"table\" or \"plot\")"
            );
            return ExitCode::from(2);
        }
    };

    let bytes = match std::fs::read(in_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {in_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut options = typst_docx_import::ImportOptions::default();
    if literal {
        options.tier = typst_docx_import::Tier::Literal;
    }
    if charts_plot {
        options.charts = typst_docx_import::ChartStyle::Plot;
    }

    let result = match typst_docx_import::import_docx_with(&bytes, &options) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("import error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = std::fs::write(&out_path, &result.source) {
        eprintln!("error: cannot write {}: {e}", out_path.display());
        return ExitCode::FAILURE;
    }

    let base = out_path.parent().unwrap_or(Path::new("."));
    for (rel, data) in &result.assets {
        let target = base.join(rel);
        if let Some(dir) = target.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&target, data) {
            eprintln!("error: cannot write asset {}: {e}", target.display());
            return ExitCode::FAILURE;
        }
    }

    eprintln!(
        "wrote {} ({} bytes, {} assets)",
        out_path.display(),
        result.source.len(),
        result.assets.len()
    );
    if !result.report.notes.is_empty() {
        eprintln!("--- import notes ---\n{}", result.report.summary());
    }
    ExitCode::SUCCESS
}
