//! Pattern & match-enhancement acceptance tests — match guards, or-patterns,
//! nested / tuple destructuring — enforced end-to-end through the real CLI
//! binary (`lex → parse → check → run`) over `.adr` fixtures.
//!
//! Mirrors `tests/acceptance.rs`: each test spawns the compiled `adder` binary
//! on a fixture and asserts on stdout / stderr / exit status.

mod common;
use common::{run_fixture, stderr, stdout};

/// The features example — a guard, an or-pattern, and a nested destructure —
/// runs cleanly and prints the expected lines.
#[test]
fn patterns_example_runs() {
    let o = run_fixture("examples/features/patterns.adr");
    assert!(o.status.success(), "patterns example should run; stderr:\n{}", stderr(&o));
    assert_eq!(
        stdout(&o).lines().collect::<Vec<_>>(),
        vec!["big", "zero", "small", "other", "12", "balanced", "node", "leaf"],
    );
}

/// Control flow propagates out of a `match` used in statement position: a
/// `return` inside an arm unwinds the enclosing function (rather than collapsing
/// into the match's value), and `break`/`continue` inside an arm target the
/// enclosing loop. Regression test for the match-as-statement flow bug.
#[test]
fn match_statement_propagates_control_flow() {
    let o = run_fixture("examples/features/match_control_flow.adr");
    assert!(o.status.success(), "example should run; stderr:\n{}", stderr(&o));
    assert_eq!(
        stdout(&o).lines().collect::<Vec<_>>(),
        // describe() returns from each arm; run_until_negative() prints
        // positives, skips the zero (continue), and stops at -1 (break).
        vec!["negative", "zero", "positive", "3", "5"],
    );
}

/// A guarded arm does NOT count toward exhaustiveness: a guard over the last
/// uncovered variant, with no `_`, is a compile-time `check error` and nothing
/// runs.
#[test]
fn guard_only_arm_is_nonexhaustive_compile_error() {
    let o = run_fixture("examples/errors/guard_only_nonexhaustive.adr");
    assert!(!o.status.success(), "a guard-only arm without `_` must be rejected");
    let err = stderr(&o);
    assert!(err.contains("check error"), "should be a compile-time check error:\n{err}");
    assert!(err.contains("Blue"), "should name the uncovered variant `Blue`:\n{err}");
    // It must fail *before* running — no program output.
    assert!(stdout(&o).trim().is_empty(), "should not have executed; stdout:\n{}", stdout(&o));
}
