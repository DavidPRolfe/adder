//! M2 Wave 1 — map/set literals, the two empty forms (`{}` map vs `Set()`),
//! dedup, and nullable map lookup. End-to-end through the real CLI binary.

mod common;
use common::{run_fixture, stdout};

#[test]
fn empty_map_vs_empty_set_dedup_and_lookup() {
    let o = run_fixture("examples/features/sets_maps.adr");
    assert!(o.status.success(), "should run cleanly");
    let out: Vec<String> = stdout(&o).lines().map(|l| l.to_string()).collect();
    assert_eq!(out, vec!["{}", "Set()", "3", "10", "null", "2"]);
}
