//! The `adder` CLI: read a file, run it through the pipeline
//! (`lex → parse → check → run`), and report any [`Diagnostic`]s against the
//! source. Exits non-zero on any error.
//!
//! With `--docs`, instead of running the program the CLI parses the file and
//! prints each declaration alongside its captured `##` doc comment (grammar
//! §1.1), so the doc-attachment feature is observable.

use std::process::ExitCode;

use adder::ast::{Program, StmtKind};
use adder::error::{render_all, Diagnostic};

fn main() -> ExitCode {
    let mut args = std::env::args();
    let prog = args.next().unwrap_or_else(|| "adder".to_string());

    // Collect a single optional `--docs` flag and the source path (in any order).
    let mut docs_mode = false;
    let mut path: Option<String> = None;
    for arg in args {
        match arg.as_str() {
            "--docs" => docs_mode = true,
            _ => {
                if path.is_none() {
                    path = Some(arg);
                }
            }
        }
    }

    let path = match path {
        Some(p) => p,
        None => {
            eprintln!("usage: {prog} [--docs] <file.adr>");
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

    if docs_mode {
        return match print_docs(&src) {
            Ok(()) => ExitCode::SUCCESS,
            Err(diags) => {
                eprint!("{}", render_all(&diags, &src));
                ExitCode::FAILURE
            }
        };
    }

    match run_pipeline(&src) {
        Ok(()) => ExitCode::SUCCESS,
        Err(diags) => {
            eprint!("{}", render_all(&diags, &src));
            ExitCode::FAILURE
        }
    }
}

/// Parse `src` and print each declaration with its attached `##` doc comment.
fn print_docs(src: &str) -> Result<(), Vec<Diagnostic>> {
    let tokens = adder::lexer::lex(src).map_err(|d| vec![d])?;
    let program = adder::parser::parse(&tokens)?;
    print_program_docs(&program);
    Ok(())
}

/// Render the doc comments of every declaration in `program` to stdout.
fn print_program_docs(program: &Program) {
    for stmt in &program.stmts {
        match &stmt.kind {
            StmtKind::Fn(f) => {
                print_doc("fn", &f.name, &f.doc);
            }
            StmtKind::Struct(s) => {
                print_doc("struct", &s.name, &s.doc);
                for field in &s.fields {
                    print_doc("  field", &field.name, &field.doc);
                }
            }
            StmtKind::Enum(e) => {
                print_doc("enum", &e.name, &e.doc);
                for variant in &e.variants {
                    print_doc("  variant", &variant.name, &variant.doc);
                }
            }
            StmtKind::Impl(i) => {
                println!("impl {}:", i.type_name);
                for method in &i.methods {
                    print_doc("  fn", &method.name, &method.doc);
                }
            }
            _ => {}
        }
    }
}

/// Print one declaration's kind/name and its doc (or `(no doc)`), indenting any
/// multi-line doc under the header.
fn print_doc(kind: &str, name: &str, doc: &Option<String>) {
    match doc {
        Some(d) => {
            println!("{kind} {name}:");
            for line in d.lines() {
                println!("    {line}");
            }
        }
        None => println!("{kind} {name}: (no doc)"),
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
