//! Shared helpers for the integration tests.
//!
//! These tests deliberately exercise the **real compiled `adder` binary** (exit
//! codes, stderr phase labels), so the helper here spawns the binary as a
//! subprocess rather than calling the library in-process.
//!
//! Lives at `tests/common/mod.rs` (a subdirectory module) rather than
//! `tests/common.rs` so Cargo does not compile it as its own test binary; each
//! integration test file pulls it in with `mod common;`.
//!
//! `#![allow(dead_code)]`: this module is compiled fresh into every integration
//! test binary, and not every binary uses every helper (e.g. `sets_maps`
//! never asserts on `stderr`). Without this, an unused helper in some binary is
//! a dead-code warning.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Output};

/// Run the `adder` binary on a fixture file (path relative to the crate root).
pub fn run_fixture(rel: &str) -> Output {
    let bin = env!("CARGO_BIN_EXE_adder");
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push(rel);
    Command::new(bin)
        .arg(&path)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {bin} on {}: {e}", path.display()))
}

/// The program's captured standard output as a `String`.
pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The program's captured standard error as a `String`.
pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
