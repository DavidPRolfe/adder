//! Feature-corpus integration tests — one per program under
//! `examples/features/`, each running the real `adder` binary end-to-end
//! (`lex → parse → check → run`) and asserting exact stdout.
//!
//! Mirrors the style of `tests/acceptance.rs`: spawn the compiled binary on a
//! fixture, then compare its stdout line-by-line. Using `.lines()` makes the
//! comparison robust to `\n` vs `\r\n` line endings across platforms.

mod common;
use common::{run_fixture, stderr, stdout};

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

// ===========================================================================
// Traits (M3) — required + default methods, `impl ... for`, an inherited
// default, an override, and a trait-typed parameter dispatched at runtime.
// ===========================================================================

#[test]
fn traits_corpus() {
    assert_lines(
        "examples/features/traits.adr",
        &[
            "Rex",
            "Hello, Rex!",
            "BEEP unit-7",
            "Hello, Rex!",
            "BEEP unit-7",
            "Hello, Rex!",
            "BEEP unit-7",
        ],
    );
}

// ===========================================================================
// Result + try (M3) — Ok/Err construction, `try` unwrap + early-return,
// a two-step `try` chain that short-circuits, bare and leading-dot patterns.
// ===========================================================================

#[test]
fn result_try_corpus() {
    assert_lines(
        "examples/features/result_try.adr",
        &["ok 5", "err DivByZero", "root 3", "err DivByZero", "err Negative"],
    );
}

// ===========================================================================
// derive Ord (M3) — lexicographic comparisons, in-place sort, min/max on a
// derived struct, and variant-order comparison + sort on a derived enum.
// ===========================================================================

#[test]
fn derive_ord_corpus() {
    assert_lines(
        "examples/features/derive_ord.adr",
        &[
            "true",
            "true",
            "true",
            "true",
            "Version(major: 1, minor: 2)",
            "Version(major: 1, minor: 5)",
            "Version(major: 2, minor: 0)",
            "Version(major: 1, minor: 2)",
            "Version(major: 2, minor: 0)",
            "true",
            "[Low, Mid, High]",
        ],
    );
}
