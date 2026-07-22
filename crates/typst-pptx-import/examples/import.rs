//! CLI: `cargo run -p typst-pptx-import --example import -- in.pptx out.typ`

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if positional.len() != 2 {
        eprintln!("usage: import [--idiomatic] <in.pptx> <out.typ>");
        return ExitCode::from(2);
    }
    let out_path = PathBuf::from(positional[1]);

    let mut options = typst_pptx_import::ImportOptions::default();
    if args.iter().any(|a| a == "--idiomatic") {
        options.fidelity = typst_pptx_import::Fidelity::Idiomatic;
    }

    let bytes = match std::fs::read(positional[0]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", positional[0]);
            return ExitCode::FAILURE;
        }
    };
    let result = match typst_pptx_import::import_pptx_with(&bytes, &options) {
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
    let dir = out_path.parent().unwrap_or(std::path::Path::new("."));
    for (rel, bytes) in &result.assets {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, bytes) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    }
    println!(
        "wrote {} ({} bytes, {} assets)",
        out_path.display(),
        result.source.len(),
        result.assets.len()
    );
    if !result.report.is_empty() {
        eprintln!("--- import notes ---");
        eprint!("{}", result.report);
    }
    ExitCode::SUCCESS
}
