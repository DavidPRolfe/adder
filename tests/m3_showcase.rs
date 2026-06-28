//! M3 definition-of-done — the showcase program from `spec/06-m3-scope.md` runs
//! end-to-end and prints the expected lines. Exercises (together, for the first
//! time) a trait with a required method and a default, two `impl … for` blocks
//! (one inheriting the default, one overriding it), runtime dispatch through a
//! trait-typed `List[Area]` parameter, the prelude generic `Result[T, E]`, a
//! `try` early-return, `match` over a `Result` with bare `Ok`/`Err` patterns,
//! and `derive Ord` + in-place `.sort()`.

mod common;
use common::{run_fixture, stderr, stdout};

#[test]
fn m3_showcase_runs_and_prints_expected() {
    let o = run_fixture("examples/m3_showcase.adr");
    assert!(o.status.success(), "showcase should run cleanly; stderr:\n{}", stderr(&o));
    let expected = "\
area = 3.14159
rect 2.0 x 3.0 = 6.0
total = 9.14159
scaled = 12.0
rejected: NegativeSize
Score(points: 2, name: a)";
    assert_eq!(stdout(&o).trim_end(), expected);
}
