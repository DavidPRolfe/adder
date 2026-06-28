//! M2 Wave 2-B acceptance tests — **null sugar**: the `?.` safe-call and the
//! `.expect(msg)` assertion, enforced end-to-end through the real CLI binary
//! (`lex → parse → check → run`) over `.adr` fixture programs.
//!
//! These cover the spec §8 behaviors and the M2 definition-of-done bullet:
//! `x?.field` yields `null` on a null receiver (and chains short-circuit);
//! `x.expect("msg")` panics with `msg`; both satisfy the null-narrowing check —
//! and a `.expect` panic is a *runtime* error, distinct from the compile-time
//! null-narrowing error a plain un-narrowed `T?` use still produces.

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

// ===========================================================================
// `?.` short-circuiting (incl. a chain) + `.expect` on a present value all run
// cleanly and produce the expected output, and the program type-checks (the
// safe-call / `.expect` satisfy the null-narrowing check).
// ===========================================================================

#[test]
fn nullable_sugar_example_runs_and_prints() {
    let o = run_fixture("examples/features/nullable_sugar.adr");
    assert!(o.status.success(), "should run cleanly; stderr:\n{}", stderr(&o));
    assert_eq!(
        stdout(&o).lines().collect::<Vec<_>>(),
        vec!["London", "unknown", "unknown", "42"],
    );
}

// ===========================================================================
// `.expect` on a null value is a RUNTIME error (a panic), not a compile-time
// check error: it type-checks, runs, and aborts at run time with the message.
// ===========================================================================

#[test]
fn expect_on_null_is_a_runtime_error() {
    let o = run_fixture("examples/errors/expect_null_panics.adr");
    assert!(!o.status.success(), "`.expect` on null must abort nonzero");
    let err = stderr(&o);
    assert!(err.contains("runtime error"), "should be a runtime error:\n{err}");
    assert!(err.contains("panic"), "should report a panic:\n{err}");
    assert!(err.contains("name was required"), "should carry the message:\n{err}");
}

// ===========================================================================
// Contrast: a plain un-narrowed `T?` use is still a COMPILE-TIME check error
// (the sugar above did not weaken the M1 null-narrowing check). It must be
// rejected before anything runs.
// ===========================================================================

#[test]
fn plain_unnarrowed_nullable_is_still_a_check_error() {
    let o = run_fixture("examples/errors/null_unnarrowed.adr");
    assert!(!o.status.success(), "unnarrowed nullable use must be rejected");
    let err = stderr(&o);
    assert!(err.contains("check error"), "should be a compile-time check error:\n{err}");
    assert!(err.contains("nullable"), "should mention nullability:\n{err}");
    assert!(stdout(&o).trim().is_empty(), "should not have executed");
}
