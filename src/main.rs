//! The `adder` CLI: read a file, run it through the pipeline
//! (`lex → parse → check → run`), and report any [`Diagnostic`]s against the
//! source. Exits non-zero on any error.

use std::process::ExitCode;

use adder::error::{render_all, Diagnostic};

fn main() -> ExitCode {
    let mut args = std::env::args();
    let prog = args.next().unwrap_or_else(|| "adder".to_string());
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: {prog} <file.adr>");
            return ExitCode::FAILURE;
        }
    };

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    match run_pipeline(&src) {
        Ok(()) => ExitCode::SUCCESS,
        Err(diags) => {
            eprint!("{}", render_all(&diags, &src));
            ExitCode::FAILURE
        }
    }
}

/// Drive all four stages, normalizing each stage's error shape into a
/// `Vec<Diagnostic>` so they render uniformly.
fn run_pipeline(src: &str) -> Result<(), Vec<Diagnostic>> {
    let tokens = adder::lexer::lex(src).map_err(|d| vec![d])?;
    let program = adder::parser::parse(&tokens)?;
    adder::checks::check(&program)?;
    adder::interp::run(&program).map_err(|d| vec![d])?;
    Ok(())
}
