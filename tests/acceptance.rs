//! Milestone-1 acceptance tests — the **definition of done** from
//! `spec/02-mvp-scope.md`, enforced end-to-end through the real CLI binary
//! (`lex → parse → check → run`) over `.adr` fixture programs.
//!
//! Each test spawns the compiled `adder` binary on a fixture and asserts on its
//! stdout / stderr / exit status, so it exercises the entire pipeline exactly as
//! a user would.

mod common;
use common::{run_fixture, stderr, stdout};

// ===========================================================================
// DoD #1 — the showcase program runs and prints `= 9.0`.
// ===========================================================================

#[test]
fn showcase_prints_9_0() {
    let o = run_fixture("examples/eval.adr");
    assert!(o.status.success(), "showcase should run cleanly; stderr:\n{}", stderr(&o));
    assert_eq!(stdout(&o).trim(), "= 9.0");
}

// ===========================================================================
// DoD #2 — removing the `Div` arm is a COMPILE-TIME exhaustiveness error.
// ===========================================================================

#[test]
fn missing_match_arm_is_a_compile_error() {
    let o = run_fixture("examples/errors/exhaustive_missing.adr");
    assert!(!o.status.success(), "missing Div arm must be rejected");
    let err = stderr(&o);
    assert!(err.contains("check error"), "should be a compile-time check error:\n{err}");
    assert!(err.contains("Div"), "should name the missing variant `Div`:\n{err}");
    // It must fail *before* running — no program output.
    assert!(stdout(&o).trim().is_empty(), "should not have executed; stdout:\n{}", stdout(&o));
}

// ===========================================================================
// DoD #3 — a `T?` used as `T` without narrowing is a COMPILE-TIME error;
//          the same code guarded by `is not null` compiles and runs.
// ===========================================================================

#[test]
fn nullable_used_as_non_null_is_a_compile_error() {
    let o = run_fixture("examples/errors/null_unnarrowed.adr");
    assert!(!o.status.success(), "unnarrowed nullable use must be rejected");
    let err = stderr(&o);
    assert!(err.contains("check error"), "should be a compile-time check error:\n{err}");
    assert!(err.contains("nullable"), "should mention nullability:\n{err}");
    assert!(stdout(&o).trim().is_empty(), "should not have executed");
}

#[test]
fn narrowed_nullable_compiles_and_runs() {
    let o = run_fixture("examples/narrowed.adr");
    assert!(o.status.success(), "narrowed code should compile and run; stderr:\n{}", stderr(&o));
    assert_eq!(stdout(&o).lines().collect::<Vec<_>>(), vec!["42", "0"]);
}

// ===========================================================================
// DoD #4 — a `val` reassignment is rejected; a non-`Bool` condition is rejected.
// ===========================================================================

#[test]
fn val_reassignment_is_rejected() {
    let o = run_fixture("examples/errors/val_reassign.adr");
    assert!(!o.status.success(), "reassigning a `val` must be rejected");
    assert!(stderr(&o).contains("val"), "error should mention the `val` binding:\n{}", stderr(&o));
}

#[test]
fn non_bool_condition_is_rejected() {
    let o = run_fixture("examples/errors/non_bool_cond.adr");
    assert!(!o.status.success(), "a non-Bool condition must be rejected");
    assert!(stderr(&o).contains("Bool"), "error should mention Bool:\n{}", stderr(&o));
}

// ===========================================================================
// Regression — cross-scope mutable reassignment (loop accumulator) works.
// A bare `x = e` is parsed as a Binding, not an Assign; the interpreter must
// still reassign the outer binding rather than shadow it in the loop scope.
// ===========================================================================

#[test]
fn loop_accumulator_mutates_outer_binding() {
    let o = run_fixture("examples/errors/accumulator.adr");
    assert!(o.status.success(), "accumulator should run; stderr:\n{}", stderr(&o));
    assert_eq!(stdout(&o).trim(), "10");
}

// ===========================================================================
// Structs & methods — a method mutates `self` in place (via `self.field = …`),
// and the caller observes the change. Methods are defined only in `impl`.
// ===========================================================================

#[test]
fn method_mutates_self_in_place() {
    let o = run_fixture("examples/shapes.adr");
    assert!(o.status.success(), "shapes should run; stderr:\n{}", stderr(&o));
    let out = stdout(&o);
    let lines: Vec<&str> = out.lines().collect();
    // Before grow: area 12.0; a returned copy: area 48.0; after grow: area 1200.0.
    assert!(lines[0].ends_with("area 12.0"), "got {:?}", lines);
    assert!(lines[1].ends_with("area 48.0"), "got {:?}", lines);
    assert!(lines[2].ends_with("area 1200.0"), "got {:?}", lines);
}

// ===========================================================================
// Enums — namespaced variants: qualified construction (`Shape.Circle(...)`),
// leading-dot match arms (`.Circle(r)`), niladic variants, and structural `==`.
// (The niladic case — `Shape.Unit` as a value — was previously unusable.)
// ===========================================================================

#[test]
fn qualified_enum_variants_construct_and_match() {
    let o = run_fixture("examples/shapes_enum.adr");
    assert!(o.status.success(), "enum example should run; stderr:\n{}", stderr(&o));
    assert_eq!(stdout(&o).lines().collect::<Vec<_>>(), vec!["12.56636", "12.0", "1.0", "true"]);
}
