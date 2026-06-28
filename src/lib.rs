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
//! - **`checks.rs`** owns the two compile-time analyses and *only* those:
//!   match-exhaustiveness over enums, and null-narrowing of `T?` values.
//! - **`interp.rs`** owns all *runtime* enforcement and behavior:
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
