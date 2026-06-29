//! Collection / comprehension acceptance tests — surface syntax and call
//! plumbing, run end-to-end through the real `adder` binary
//! (`lex → parse → check → run`).
//!
//! Mirrors `tests/acceptance.rs`: spawn the compiled binary on a fixture, then
//! assert on stdout / stderr / exit status. `.lines()` keeps comparisons robust
//! to `\n` vs `\r\n` across platforms.
//!
//! Scope note: the iterator method table (`map`/`filter`/`.items()`/…) and the
//! `Map`/`Set` `Show` rendering are exercised by the `pipelines` and `sets_maps`
//! suites, so these tests do not assert on `Map`/`Set` *printed* form — they
//! exercise literals, tuples, comprehensions, destructuring, default/named args,
//! and passable lambdas, and assert on the scalar / `List` results those produce.

mod common;
use common::{run_fixture, stderr, stdout};

// ===========================================================================
// The collections feature example runs cleanly and prints the expected lines.
// ===========================================================================

#[test]
fn collections_example_runs() {
    let o = run_fixture("examples/features/collections.adr");
    assert!(
        o.status.success(),
        "collections.adr should run cleanly; stderr:\n{}",
        stderr(&o)
    );
    let out = stdout(&o);
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        vec![
            "7",
            "9",
            "[1, 4, 16, 25]",
            "[2, 4, 6]",
            "11",
            "15",
            "apple: 3",
            "pear: 2",
            "fig: 5",
            "total = 10",
            "true",
        ]
    );
}

// ===========================================================================
// A tuple-binder arity mismatch is a runtime error (and nothing extra prints).
// ===========================================================================

#[test]
fn tuple_destructure_arity_mismatch_is_runtime_error() {
    let o = run_fixture("examples/errors/tuple_arity.adr");
    assert!(!o.status.success(), "mismatched tuple destructure must fail");
    assert!(
        stderr(&o).contains("runtime error"),
        "should be a runtime error:\n{}",
        stderr(&o)
    );
}
