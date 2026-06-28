//! # Adder — a tree-walking interpreter
//!
//! Adder is a Python-readable, Rust-expressive language. This crate is the Adder
//! interpreter: a tree-walker plus exactly two static checks. See the specs in
//! `spec/` (especially `03-mvp-grammar.md`, the authority for surface syntax).
//!
//! ## Pipeline
//!
//! ```text
//!   source ──lex──▶ tokens ──parse──▶ AST ──check──▶ AST ──run──▶ effects
//! ```
//!
//! | Stage | Entry point | Owns |
//! | ----- | ----------- | ---- |
//! | lex   | [`lexer::lex`]   | grammar §1: tokens + layout (`Indent`/`Dedent`/`Newline`), string interpolation re-lexing |
//! | parse | [`parser::parse`] | grammar §2–§7: token stream → [`ast::Program`]; parses interpolation sub-exprs |
//! | check | [`checks::check`] | **match-exhaustiveness** + **null-narrowing** (compile-time) |
//! | run   | [`interp::run`]   | the tree-walker + runtime enforcement (see below) |
//!
//! ## Explicit ownership decisions
//!
//! - **`checks/`** owns the two compile-time analyses and *only* those:
//!   match-exhaustiveness over enums, and null-narrowing of `T?` values.
//! - **`interp/`** owns all *runtime* enforcement and behavior:
//!   - `val`-immutability (reassigning a `val` is a runtime error),
//!   - `Bool`-condition enforcement (`if`/`elif`/`while`/ternary; no
//!     truthiness),
//!   - `Show` rendering (default value display for `print` / interpolation),
//!   - structural `==` (and `is` / `is not`),
//!   - the prelude (`print`, `panic`) seeded as ordinary bindings,
//!   - the entry point: run top-level statements, then call `main()` if a
//!     zero-arg `main` was declared.
//!
//! ## Shared contracts (do not change downstream)
//!
//! - [`token`] — the lexer↔parser contract ([`token::Token`], [`token::Span`],
//!   [`token::StrPart`]).
//! - [`ast`]   — the parser↔checks↔interp contract ([`ast::Program`] et al.).
//! - [`error`] — [`error::Diagnostic`] and its source renderer, used by every
//!   stage and the CLI.

pub mod ast;
pub mod checks;
pub mod error;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod token;

/// Run the whole pipeline (`lex → parse → check → run`) in-process on `src`,
/// writing program output (the `print` builtin) to `out`.
///
/// This is the in-process entry point that both the CLI and embedders use: the
/// CLI passes `&mut std::io::stdout().lock()`; a test or host can pass a
/// `&mut Vec<u8>` to capture output without touching real stdout. Each stage's
/// error shape is normalized into a `Vec<Diagnostic>` so callers can render them
/// uniformly.
pub fn run_source(
    src: &str,
    out: &mut dyn std::io::Write,
) -> Result<(), Vec<error::Diagnostic>> {
    let tokens = lexer::lex(src).map_err(|d| vec![d])?;
    let program = parser::parse(&tokens)?;
    checks::check(&program)?;
    interp::run(&program, out).map_err(|d| vec![d])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// In-process capture: running a tiny program through [`run_source`] into a
    /// `Vec<u8>` yields exactly the program's stdout. Guards the writer injection
    /// (output goes to the supplied writer, not process stdout).
    #[test]
    fn run_source_captures_print_output() {
        let mut buf: Vec<u8> = Vec::new();
        super::run_source(r#"print("hi")"#, &mut buf).expect("program should run");
        assert_eq!(String::from_utf8(buf).unwrap(), "hi\n");
    }
}
