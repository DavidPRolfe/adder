//! Eager iterator-pipeline acceptance tests — built-in method dispatch
//! (`filter`/`map`/`fold`/…), exercised end-to-end through the real CLI binary
//! (`lex → parse → check → run`) over a `.adr` fixture.
//!
//! Self-contained: the tiny `run_fixture`/`stdout` helpers below mirror
//! `tests/acceptance.rs` so this file stands alone.

mod common;
use common::{run_fixture, stderr, stdout};

/// The headline pipeline: `filter(...).map(...).sum()` over a list literal with
/// passable lambdas yields `56`, plus the rest of the showcase lines.
#[test]
fn pipelines_example_runs() {
    let o = run_fixture("examples/features/pipelines.adr");
    assert!(o.status.success(), "pipelines example should run cleanly; stderr:\n{}", stderr(&o));
    let out = stdout(&o);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec![
            "56",        // [1..6].filter(even).map(square).sum()
            "30",        // (1..=4).map(square).sum()  — range is a list
            "120",       // [1..5].fold(1, *)
            "3",         // filtered count
            "true",      // any > 100
            "true",      // all > 0
            "12",        // find first > 10
            "[1, 2]",    // take 2
            "[4, 5]",    // skip 3
            "[1, 2, 3]", // sorted
            "[3, 2, 1]", // reverse
        ],
        "unexpected pipeline output:\n{out}"
    );
}
