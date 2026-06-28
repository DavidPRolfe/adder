//! Feature-corpus integration tests — one per program under
//! `examples/features/`, each running the real `adder` binary end-to-end
//! (`lex → parse → check → run`) and asserting exact stdout.
//!
//! Mirrors the style of `tests/acceptance.rs`: spawn the compiled binary on a
//! fixture, then compare its stdout line-by-line. Using `.lines()` makes the
//! comparison robust to `\n` vs `\r\n` line endings across platforms.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Run the `adder` binary on a fixture file (path relative to the crate root).
fn run_fixture(rel: &str) -> Output {
    let bin = env!("CARGO_BIN_EXE_adder");
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push(rel);
    Command::new(bin)
        .arg(&path)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {bin} on {}: {e}", path.display()))
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Assert a fixture runs cleanly and emits exactly `expected` lines on stdout.
fn assert_lines(rel: &str, expected: &[&str]) {
    let o = run_fixture(rel);
    assert!(
        o.status.success(),
        "{rel} should run cleanly; stderr:\n{}",
        stderr(&o)
    );
    let out = stdout(&o);
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(got, expected, "{rel} stdout mismatch");
}

// ===========================================================================
// Enums + match — niladic / positional / named variants, qualified
// construction, leading-dot AND explicit-qualified arms, a catch-all binding
// arm, default Show rendering, and structural equality.
// ===========================================================================

#[test]
fn enums_match_corpus() {
    assert_lines(
        "examples/features/enums_match.adr",
        &[
            "1.0",
            "12.56",
            "12.0",
            "4.0",
            "unit",
            "non-unit",
            "Unit",
            "Circle(radius: 2.0)",
            "Pair(1.5, 2.5)",
            "true",
            "false",
            "true",
        ],
    );
}

// ===========================================================================
// Structs + methods — positional + named construction, field access, impl
// methods, a method that mutates `self`, and default Show rendering.
// ===========================================================================

#[test]
fn structs_methods_corpus() {
    assert_lines(
        "examples/features/structs_methods.adr",
        &[
            "1.0",
            "2.0",
            "3.0",
            "33.0",
            "3.0",
            "11.0",
            "3",
            "Point(x: 1.0, y: 2.0)",
            "Counter(value: 3)",
        ],
    );
}

// ===========================================================================
// Nullability — T?, null, `is not null` narrowing, .or_else(default).
// ===========================================================================

#[test]
fn nullability_corpus() {
    assert_lines(
        "examples/features/nullability.adr",
        &[
            "42",
            "0",
            "7",
            "-1",
            "-1",
            "100",
            "hello Ada",
            "anonymous",
        ],
    );
}

// ===========================================================================
// Control flow — if/elif/else, ternary, while, for over ranges (0..n, 0..=n)
// and lists, break, continue.
// ===========================================================================

#[test]
fn control_flow_corpus() {
    assert_lines(
        "examples/features/control_flow.adr",
        &[
            "negative",
            "zero",
            "positive",
            "big",
            "10",
            "10",
            "15",
            "17",
            "3",
            "20",
        ],
    );
}

// ===========================================================================
// Values & operators — Int (incl. big-int `2 ** 100`), Float, Bool, string
// interpolation + `{{ }}` escapes + escape sequences, lists + indexing +
// nesting, `**` precedence/associativity, comparisons, and/or/not, structural ==.
// ===========================================================================

#[test]
fn values_ops_corpus() {
    assert_lines(
        "examples/features/values_ops.adr",
        &[
            "14",
            "20",
            "2",
            "1024",
            "1267650600228229401496703205376",
            "5",
            "-4",
            "512",
            "3.5",
            "2.5",
            "6.0",
            "false",
            "true",
            "true",
            "false",
            "true",
            "true",
            "true",
            "false",
            "true",
            "true",
            "true",
            "true",
            "hello Ada, n = 42",
            "braces: {literal} and 42",
            "tab\tend",
            "quote: \" backslash: \\",
            "10",
            "30",
            "[10, 20, 30]",
            "2",
            "3",
            "[[1, 2], [3, 4]]",
            "true",
            "false",
            "true",
            "true",
            "true",
        ],
    );
}
