//! M2 Wave 2-A acceptance tests — pattern & match enhancements (match guards,
//! or-patterns, nested / tuple destructuring), enforced end-to-end through the
//! real CLI binary (`lex → parse → check → run`) over `.adr` fixtures.
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
