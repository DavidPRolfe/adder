//! M3 feature-level acceptance tests (spec/06-m3-scope.md), beyond the showcase:
//! the error model's static guarantee and the runtime conformance rules that
//! keep the milestone "typed-lite". Each spawns the compiled binary on a fixture.

mod common;
use common::{run_fixture, stderr, stdout};

/// A `match` over a `Result` whose type is known must cover `Ok` **and** `Err`;
/// dropping the `Err` arm is a compile-time exhaustiveness error (the prelude
/// `Result` enum participates like any user enum).
#[test]
fn result_match_must_be_exhaustive() {
    let o = run_fixture("examples/errors/result_nonexhaustive.adr");
    assert!(!o.status.success(), "a non-exhaustive Result match must be rejected");
    let err = stderr(&o);
    assert!(err.contains("check error"), "should be a compile-time check error:\n{err}");
    assert!(err.contains("Err"), "should name the missing `Err` variant:\n{err}");
    assert!(stdout(&o).trim().is_empty(), "should not have executed; stdout:\n{}", stdout(&o));
}
