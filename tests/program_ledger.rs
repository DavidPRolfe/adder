//! Integration test for the `examples/ledger.adr` showcase program — a tiny
//! bank ledger exercising enums + exhaustive `match`, a struct with mutating
//! `impl` methods, nullable account lookup with `is not null` narrowing, loops
//! over a list of records, and string interpolation.
//!
//! Like `tests/acceptance.rs`, this spawns the compiled `adder` binary on the
//! fixture and asserts on its stdout, so it covers the whole pipeline.

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

#[test]
fn ledger_runs_and_prints_expected_lines() {
    let o = run_fixture("examples/ledger.adr");
    assert!(
        o.status.success(),
        "ledger should run cleanly; stderr:\n{}",
        stderr(&o)
    );

    let expected = vec![
        "Replaying alice's history:",
        "  open account -> balance 0",
        "  deposit 100 -> balance 100",
        "  withdraw 30 -> balance 70",
        "  transfer 20 to bob -> balance 50",
        "Credited transfer to bob: 70 credits",
        "Account for carol found? no",
        "Final balances:",
        "  alice: 50 credits",
        "  bob: 70 credits",
    ];

    assert_eq!(stdout(&o).lines().collect::<Vec<_>>(), expected);
}
