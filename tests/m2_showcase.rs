//! M2 definition-of-done — the showcase program from `spec/04-m2-scope.md` runs
//! end-to-end and prints the expected lines. Exercises (together, for the first
//! time) function-typed params, lambdas passed to a `fn`, an eager
//! filter/map/sum pipeline, a list comprehension with a filter, a `Map` literal,
//! tuple destructuring in a `for`, and a guarded `match` arm bound to a local
//! (`label = match …:` followed by a sibling statement).

use std::path::PathBuf;
use std::process::{Command, Output};

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
