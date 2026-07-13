//! `omml2typst` — read OMML (`<m:oMath>`) from stdin or a file, print idiomatic
//! Typst math source.
//!
//! Usage:
//!
//! ```text
//! omml2typst [FILE]
//! cat equation.xml | omml2typst
//! ```
//!
//! The output is what you would place between `$…$`.

use std::io::Read;
use std::process::ExitCode;

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let input = match arg.as_deref() {
        Some("-h") | Some("--help") => {
            eprintln!("usage: omml2typst [FILE]   (reads OMML from FILE or stdin)");
            return ExitCode::SUCCESS;
        }
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("omml2typst: cannot read {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("omml2typst: cannot read stdin: {e}");
                return ExitCode::FAILURE;
            }
            s
        }
    };

    println!("{}", typst_ooxml_math::omml_to_typst(&input));
    ExitCode::SUCCESS
}
