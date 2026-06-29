//! Stage 4 — **interpretation** (runtime): the tree-walker.
//!
//! Owned by the *interpreter agent*. Walks the [`Program`] and executes it.
//!
//! ## Ownership (see `lib.rs` for the canonical notes)
//!
//! This stage **owns** the following runtime concerns. They are deliberately
//! *not* in [`crate::checks`]:
//! - **`val`-reassignment rejection** — reassigning an immutable `val` binding
//!   is a runtime error.
//! - **Non-`Bool` condition rejection** — `if`/`elif`/`while`/ternary
//!   conditions must be `Bool`; no truthiness coercion (runtime error).
//! - **`Show` rendering** — the default walk-the-value display used by `print`
//!   and string interpolation; every struct/enum prints without user code.
//! - **Structural `==`** — value equality produced by walking values (also
//!   backs `is` / `is not`).
//! - **Prelude** — seeds `print` and `panic` as ordinary bindings in the global
//!   scope before running (grammar §5.6).
//! - **Entry point** — run top-level statements in order, then call a zero-arg
//!   `main()` if one was declared (grammar §2).
//!
//! Contract: `Ok(())` on a clean run; `Err(Diagnostic)` on the first runtime
//! error (`panic`, bad condition type, `val` reassignment, division by zero,
//! etc.).
//!
//! ## Architecture notes
//!
//! - **Environment model.** [`Env`] is `Rc<RefCell<Scope>>`. Name resolution
//!   walks `parent`. Function calls create a fresh child scope of the
//!   *closure's captured* env (lexical scoping); `if`/`while`/`for`/`match`
//!   bodies run in a fresh child scope of the current env (block scoping).
//!   Closures capture the defining env by reference, so later mutations are
//!   visible.
//! - **Control-flow signaling.** Statement evaluation returns a [`Flow`] signal
//!   threaded up the call stack: `Normal(Value)` carries the last expression
//!   value (for implicit return); `Return`, `Break`, `Continue` unwind to the
//!   nearest enclosing construct that handles them. Runtime errors are a
//!   separate `Err(Diagnostic)` on the `Result`.
//! - **Declarations.** Struct/enum/impl declarations are collected into a
//!   per-run registry so construction calls and method resolution work
//!   regardless of source order (and so methods see top-level `fn`s).
//!
//! ## Key semantic decisions
//!
//! - **Int `/`** — integer division requires the result to be exact; a
//!   non-exact `Int / Int` is a runtime error (use float division for that).
//!   `%` is the remainder. Float `/` is true division.
//! - **Ranges** — `a..b` / `a..=b` are evaluated eagerly into a `List` of
//!   `Int`s for `for` iteration (and are usable as a list value generally).
//! - **Equality** — structural value equality by walking values; `Float` uses
//!   IEEE `==`; `Closure`/`Builtin` are never equal (even to themselves).
//! - **Show** — `Float` always renders with a decimal point (`9.0`, not `9`).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::ToPrimitive;

// Most AST types are referenced by the sibling submodules (which see them via
// their own `use super::*;`), not directly here; the full re-export is kept so
// those modules resolve them.
use crate::ast::{
    Arg, BinOp, EnumDecl, FnDecl, LitPattern, Param, Program, StmtKind, StructDecl, TraitDecl,
};
use crate::error::Diagnostic;
use crate::token::Span;

// Runtime data model (the `Value` enum and friends) lives in `value.rs`; the
// glob re-export lets every sibling submodule resolve `Value`, `Scope`, etc.
// through its own `use super::*;`.
mod value;
pub(crate) use value::*;

// Environment / scope-chain helpers (`env_define`/`env_get`/`env_assign`/
// `root_of`) live in `env.rs`.
mod env;
pub(crate) use env::*;

// Built-in functions and the built-in method table (`call_builtin`,
// `call_builtin_method`, the per-type dispatchers, and their helpers) live in
// `builtins.rs`.
mod builtins;
pub(crate) use builtins::*;

// The `impl Interp` blocks split out by topic. These hold only methods (no new
// public items to re-export), so each is a plain `mod` declaration; they see the
// shared items via their own `use super::*;`.
mod call;
mod eval;
mod exec;
mod matching;

// ===========================================================================
// Control-flow signaling
// ===========================================================================

/// A control-flow signal threaded through statement/block evaluation.
///
/// `Normal(v)` carries the value of the last statement evaluated (used for
/// implicit return of a block / fn body / match arm). The other variants unwind
/// to the nearest construct that handles them.
enum Flow {
    /// Fell off the end normally, carrying the trailing value (or `Unit`).
    Normal(Value),
    /// `return [expr]` — unwinds to the enclosing call.
    Return(Value),
    /// `break` — unwinds to the enclosing loop.
    Break,
    /// `continue` — unwinds to the enclosing loop iteration.
    Continue,
}

/// Result of evaluating statements: either a runtime error, or a `Flow` signal.
type FlowResult = Result<Flow, Diagnostic>;
/// Result of evaluating an expression to a [`Value`].
type EvalResult = Result<Value, Diagnostic>;

/// Collapse the `Flow` coming out of a call/method body into an [`EvalResult`].
///
/// A function/method/lambda body either falls off the end (`Normal`) or
/// `return`s — both yield the call's value. A `break`/`continue` that reaches
/// here escaped its loop and is a runtime error; `message` and `span` are the
/// diagnostic that site would have produced.
fn finish_call(flow: Flow, message: &str, span: Span) -> EvalResult {
    match flow {
        Flow::Normal(v) | Flow::Return(v) => Ok(v),
        Flow::Break | Flow::Continue => Err(Diagnostic::runtime(message.to_string(), span)),
    }
}

/// The sentinel error a `try` (spec §9) raises to unwind to the nearest call
/// boundary along the `?` chain. It is always intercepted by
/// [`Interp::finish_body`] (which checks `propagating`); its message only
/// surfaces if a `try` escapes every enclosing function — e.g. at top level —
/// which is a misuse.
fn try_unwind_sentinel(span: Span) -> Diagnostic {
    Diagnostic::runtime(
        "`try` used outside a function returning a `Result`".to_string(),
        span,
    )
}

// ===========================================================================
// Declaration registry
// ===========================================================================

/// Collected top-level declarations, used for construction and method lookup.
#[derive(Default)]
pub(crate) struct Registry {
    structs: HashMap<String, Rc<StructDecl>>,
    enums: HashMap<String, Rc<EnumDecl>>,
    /// `Type::method` → the method's `FnDecl`. Trait-impl methods and inherited
    /// trait default methods are folded into this same table at collect
    /// time, so method dispatch is uniform — see [`Interp::collect_decls`].
    methods: HashMap<(String, String), Rc<FnDecl>>,
    /// `Trait name` → its declaration, used to inherit default methods into
    /// the `methods` table for each `impl Trait for Type`.
    traits: HashMap<String, Rc<TraitDecl>>,
    /// `Variant name` → `Enum name`, so a bare `Add(...)` resolves its enum.
    variant_to_enum: HashMap<String, String>,
    /// Names of struct/enum types that opted into ordering with `derive Ord`
    /// (spec §7.1). Comparison (`<`/`<=`/`>`/`>=`) and `.sort()` of a user
    /// type are allowed only when its name is in this set.
    ord_types: HashSet<String>,
}

/// The interpreter's per-run state.
///
/// Holds the declaration registry plus the output sink. Program output (from the
/// `print` builtin) is written through `out` — a caller-supplied writer — rather
/// than directly to process stdout, so output can be captured in-process (tests,
/// embedding) without touching real stdout.
struct Interp<'a> {
    registry: Registry,
    /// Where program output goes (`print`). Borrowed for the run's duration.
    out: &'a mut dyn Write,
    /// `try` propagation (spec §9). When `try` hits an `Err`, it stashes the
    /// whole `Err(..)` value here and unwinds via a sentinel error along the
    /// existing `?` chain; the nearest function/method/lambda boundary takes it
    /// and makes it that call's return value. `Some` only transiently, during an
    /// unwind. See [`Interp::eval_try`] and [`Interp::finish_body`].
    propagating: Option<Value>,
}

// ===========================================================================
// Entry point
// ===========================================================================

/// Run a checked [`Program`]: seed the prelude, execute top-level statements in
/// order, then call a zero-arg `main()` if one exists.
///
/// Program output (the `print` builtin) is written to `out`, a caller-supplied
/// writer. Pass `&mut std::io::stdout().lock()` for the normal CLI behaviour, or
/// a `&mut Vec<u8>` to capture output in-process.
pub fn run(program: &Program, out: &mut dyn Write) -> Result<(), Diagnostic> {
    let mut interp = Interp { registry: Registry::default(), out, propagating: None };
    let root = Scope::new_root();

    // 1. Seed the prelude.
    seed_prelude(&root);

    // 2. Collect declarations (struct/enum/impl/inline methods) so order does
    //    not matter for construction or method lookup.
    interp.collect_decls(program);

    // 3. Execute top-level statements in order.
    for stmt in &program.stmts {
        // Any top-level `return`/`break`/`continue` unwinding is intentionally
        // ignored (the parser/checks are expected to reject them, but we do not
        // crash); we only need to propagate runtime errors.
        interp.exec_stmt(stmt, &root)?;
    }

    // 4. Call zero-arg `main()` if declared.
    if let Some(Value::Closure(c)) = env_get(&root, "main") {
        if let ClosureKind::Function(f) = &c.kind {
            if f.params.is_empty() {
                interp.call_closure(&c, Vec::new(), program_main_span(program))?;
            }
        }
    }

    Ok(())
}

/// A best-effort span for the `main` call (the `main` declaration's span).
fn program_main_span(program: &Program) -> Span {
    for stmt in &program.stmts {
        if let StmtKind::Fn(f) = &stmt.kind {
            if f.name == "main" {
                return f.span;
            }
        }
    }
    Span::dummy()
}

/// Seed `print` and `panic` as ordinary bindings in the root scope.
fn seed_prelude(root: &Env) {
    env_define(root, "print", Value::Builtin(Builtin::Print), false);
    env_define(root, "panic", Value::Builtin(Builtin::Panic), false);
    env_define(root, "Set", Value::Builtin(Builtin::Set), false);
    // The prelude `Result` constructors (spec §9). The matching `Result`
    // enum metadata is seeded into the registry by `collect_decls`.
    env_define(root, "Ok", Value::Builtin(Builtin::Ok), false);
    env_define(root, "Err", Value::Builtin(Builtin::Err), false);
}

impl<'a> Interp<'a> {
    /// Collect all top-level declarations into the registry.
    ///
    /// Two passes so declaration order never matters: pass 1 registers types and
    /// traits; pass 2 registers `impl` blocks (inherent **and** trait impls).
    /// For a trait impl, the trait's default methods that the impl does not
    /// override are folded into the method table under the implementing type, so
    /// `call_method` dispatch stays uniform and trait dispatch needs no special
    /// path.
    fn collect_decls(&mut self, program: &Program) {
        // Pass 1: types and traits.
        for stmt in &program.stmts {
            match &stmt.kind {
                StmtKind::Struct(s) => {
                    // Methods live only in `impl` blocks; a struct body is fields.
                    self.registry.structs.insert(s.name.clone(), Rc::new(s.clone()));
                    if s.derives.iter().any(|d| d == "Ord") {
                        self.registry.ord_types.insert(s.name.clone());
                    }
                }
                StmtKind::Enum(e) => {
                    for v in &e.variants {
                        self.registry
                            .variant_to_enum
                            .insert(v.name.clone(), e.name.clone());
                    }
                    self.registry.enums.insert(e.name.clone(), Rc::new(e.clone()));
                    if e.derives.iter().any(|d| d == "Ord") {
                        self.registry.ord_types.insert(e.name.clone());
                    }
                }
                StmtKind::Trait(t) => {
                    self.registry.traits.insert(t.name.clone(), Rc::new(t.clone()));
                }
                _ => {}
            }
        }
        // Seed the prelude `Result` enum's metadata (spec §9) so qualified
        // construction, matching, and `variant → enum` resolution work. The bare
        // `Ok`/`Err` constructors are seeded separately in `seed_prelude`.
        let result = crate::ast::result_enum_decl();
        for v in &result.variants {
            self.registry
                .variant_to_enum
                .insert(v.name.clone(), result.name.clone());
        }
        self.registry.enums.insert(result.name.clone(), Rc::new(result));
        // Pass 2: impls (resolve trait defaults against the traits from pass 1).
        for stmt in &program.stmts {
            if let StmtKind::Impl(i) = &stmt.kind {
                let mut defined: Vec<String> = Vec::with_capacity(i.methods.len());
                for m in &i.methods {
                    defined.push(m.name.clone());
                    self.registry
                        .methods
                        .insert((i.type_name.clone(), m.name.clone()), Rc::new(m.clone()));
                }
                // Trait impl: inherit each default method this impl did not
                // override. An unknown trait name is tolerated; the
                // impl's own methods still register, default inheritance is just
                // skipped.
                if let Some(trait_name) = &i.trait_name {
                    if let Some(tr) = self.registry.traits.get(trait_name).cloned() {
                        for d in &tr.defaults {
                            if !defined.contains(&d.name) {
                                self.registry.methods.insert(
                                    (i.type_name.clone(), d.name.clone()),
                                    Rc::new(d.clone()),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

// ===========================================================================
// Free helpers (no &self)
// ===========================================================================

/// The number of leading required (no-default) params among a positional param
/// list. Defaults are only honoured when *trailing*, matching the surface rule.
fn required_count(positional: &[&Param]) -> usize {
    positional
        .iter()
        .take_while(|p| matches!(p, Param::Named { default: None, .. }))
        .count()
}

/// Build a uniform arity-mismatch diagnostic. When `min == max` the count is
/// exact; otherwise it reads as a range (some params have defaults).
fn arity_error(fn_name: &str, min: usize, max: usize, got: usize, span: Span) -> Diagnostic {
    let expects = if min == max {
        format!("{}", max)
    } else {
        format!("{} to {}", min, max)
    };
    Diagnostic::runtime(
        format!("`{}` expects {} argument(s), got {}", fn_name, expects, got),
        span,
    )
}

/// The uniform "this receiver type has no such method" runtime error, shared by
/// user-type method dispatch and the built-in method table so every receiver
/// type reports a missing method the same way.
fn no_method(type_label: &str, method: &str, span: Span) -> Diagnostic {
    Diagnostic::runtime(format!("`{}` has no method `{}`", type_label, method), span)
}

/// Destructure a [`Value::Tuple`] into `names` (matching arity), binding each
/// element into `scope`. A non-tuple value or an arity mismatch is a runtime
/// error. Shared by tuple binders in `val`, `for`, and comprehensions.
fn destructure_tuple(
    names: &[String],
    value: Value,
    scope: &Env,
    span: Span,
) -> Result<(), Diagnostic> {
    match value {
        Value::Tuple(items) => {
            if items.len() != names.len() {
                return Err(Diagnostic::runtime(
                    format!(
                        "cannot destructure a {}-tuple into {} name(s)",
                        items.len(),
                        names.len()
                    ),
                    span,
                ));
            }
            for (n, v) in names.iter().zip(items.iter()) {
                env_define(scope, n, v.clone(), false);
            }
            Ok(())
        }
        other => Err(Diagnostic::runtime(
            format!("cannot destructure {} as a tuple", type_name(&other)),
            span,
        )),
    }
}

/// Coerce a value to a Bool for `and`/`or`, erroring otherwise.
fn as_bool(v: &Value, span: Span, op: &str) -> Result<bool, Diagnostic> {
    match v {
        Value::Bool(b) => Ok(*b),
        other => Err(Diagnostic::runtime(
            format!("`{}` requires Bool operands, found {}", op, type_name(other)),
            span,
        )),
    }
}

/// A short type label for error messages.
pub(crate) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::Bool(_) => "Bool",
        Value::Str(_) => "String",
        Value::List(_) => "List",
        Value::Map(_) => "Map",
        Value::Set(_) => "Set",
        Value::Tuple(_) => "tuple",
        Value::Unit => "()",
        Value::Null => "null",
        Value::Struct(_) => "struct",
        Value::Enum(_) => "enum",
        Value::Closure(_) => "function",
        Value::Builtin(_) => "builtin",
    }
}

fn binop_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Pow => "**",
        _ => "?",
    }
}

fn bigint_to_f64(n: &BigInt) -> f64 {
    n.to_f64().unwrap_or(f64::NAN)
}

// `Show` rendering (`show`/`format_float`) and structural equality / ordering
// (`values_equal`/`lit_matches`/`compare_values`) live in `show.rs`.
mod show;
pub(crate) use show::*;

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests;
