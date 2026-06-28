//! M2 definition-of-done — the showcase program from `spec/04-m2-scope.md` runs
//! end-to-end and prints the expected lines. Exercises (together, for the first
//! time) function-typed params, lambdas passed to a `fn`, an eager
//! filter/map/sum pipeline, a list comprehension with a filter, a `Map` literal,
//! tuple destructuring in a `for`, and a guarded `match` arm bound to a local
//! (`label = match …:` followed by a sibling statement).

mod common;
use common::{run_fixture, stderr, stdout};

#[test]
fn m2_showcase_runs_and_prints_expected() {
    let o = run_fixture("examples/m2_showcase.adr");
    assert!(o.status.success(), "showcase should run cleanly; stderr:\n{}", stderr(&o));
    let expected = "\
sum of even squares = 56
[1, 4, 16, 25]
apple: cheap
pear: cheap
fig: pricey
total = 10";
    assert_eq!(stdout(&o).trim_end(), expected);
}
