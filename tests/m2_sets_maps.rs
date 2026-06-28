//! M2 Wave 1 — map/set literals, the two empty forms (`{}` map vs `Set()`),
//! dedup, and nullable map lookup. End-to-end through the real CLI binary.

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

#[test]
fn empty_map_vs_empty_set_dedup_and_lookup() {
    let o = run_fixture("examples/features/sets_maps.adr");
    assert!(o.status.success(), "should run cleanly");
    let out: Vec<String> = stdout(&o).lines().map(|l| l.to_string()).collect();
    assert_eq!(out, vec!["{}", "Set()", "3", "10", "null", "2"]);
}
