//! Stage 4 — **interpretation** (runtime): the tree-walker.
//!
//! Owned by the *interpreter agent*. Walks the [`Program`] and executes it.
//!
//! ## Ownership (per spec / `02-mvp-scope.md`)
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

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::ast::{
    Arg, BinOp, Block, EnumDecl, Expr, ExprKind, FnDecl, ForStmt, IfStmt, LitPattern,
    MatchExpr, Param, Pattern, PatternKind, Payload, Program, Stmt, StmtKind, StrSeg, StructDecl,
    Target, TargetSeg, UnOp, WhileStmt,
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

// ===========================================================================
// Declaration registry
// ===========================================================================

/// Collected top-level declarations, used for construction and method lookup.
#[derive(Default)]
struct Registry {
    structs: HashMap<String, Rc<StructDecl>>,
    enums: HashMap<String, Rc<EnumDecl>>,
    /// `Type::method` → the method's `FnDecl`.
    methods: HashMap<(String, String), Rc<FnDecl>>,
    /// `Variant name` → `Enum name`, so a bare `Add(...)` resolves its enum.
    variant_to_enum: HashMap<String, String>,
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
    let mut interp = Interp { registry: Registry::default(), out };
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
}

impl<'a> Interp<'a> {
    /// Collect all top-level declarations into the registry.
    fn collect_decls(&mut self, program: &Program) {
        for stmt in &program.stmts {
            match &stmt.kind {
                StmtKind::Struct(s) => {
                    // Methods live only in `impl` blocks; a struct body is fields.
                    self.registry.structs.insert(s.name.clone(), Rc::new(s.clone()));
                }
                StmtKind::Enum(e) => {
                    for v in &e.variants {
                        self.registry
                            .variant_to_enum
                            .insert(v.name.clone(), e.name.clone());
                    }
                    self.registry.enums.insert(e.name.clone(), Rc::new(e.clone()));
                }
                StmtKind::Impl(i) => {
                    for m in &i.methods {
                        self.registry
                            .methods
                            .insert((i.type_name.clone(), m.name.clone()), Rc::new(m.clone()));
                    }
                }
                _ => {}
            }
        }
    }

    // -----------------------------------------------------------------------
    // Statement execution
    // -----------------------------------------------------------------------

    /// Execute a statement in `env`, returning a `Flow` signal. For an
    /// expression statement, `Flow::Normal` carries the expression's value
    /// (this is how a block / fn body / match arm produces an implicit value).
    fn exec_stmt(&mut self, stmt: &Stmt, env: &Env) -> FlowResult {
        match &stmt.kind {
            StmtKind::Binding(b) => {
                let v = self.eval(&b.value, env)?;
                match &b.binder {
                    // A tuple destructure `val (a, b) = e` always declares fresh
                    // names in this scope (never a bare-name reassignment).
                    crate::ast::Binder::Tuple(names) => {
                        destructure_tuple(names, v, env, stmt.span)?;
                    }
                    crate::ast::Binder::Name(name) => {
                        if !b.is_val && b.ty.is_none() {
                            // Bare `x = e` (grammar §4.1): reassign if the name
                            // already resolves in an accessible scope, otherwise
                            // introduce it here. A bare-name reassignment is
                            // parsed as a `Binding`, not an `Assign`, so the
                            // `val`-immutability and cross-scope mutation rules
                            // must be enforced on this path.
                            match env_assign(env, name, v.clone()) {
                                Ok(true) => {}
                                Ok(false) => env_define(env, name, v, false),
                                Err(()) => {
                                    return Err(Diagnostic::runtime(
                                        format!("cannot reassign `val` binding `{}`", name),
                                        stmt.span,
                                    ));
                                }
                            }
                        } else {
                            // `val x = e` or typed `x: T = e`: a declaration here.
                            env_define(env, name, v, b.is_val);
                        }
                    }
                }
                Ok(Flow::Normal(Value::Unit))
            }
            StmtKind::Assign(a) => {
                self.exec_assign(a, env)?;
                Ok(Flow::Normal(Value::Unit))
            }
            StmtKind::Return(opt) => {
                let v = match opt {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Unit,
                };
                Ok(Flow::Return(v))
            }
            StmtKind::Break => Ok(Flow::Break),
            StmtKind::Continue => Ok(Flow::Continue),
            StmtKind::Expr(e) => {
                // A `match` in statement position runs its arms as statements:
                // a `return`/`break`/`continue` inside the chosen arm must unwind
                // past the match (not collapse into its value), so route it
                // through the `Flow`-preserving path. Every other expression
                // evaluates to a value and falls off the end normally.
                if let ExprKind::Match(m) = &e.kind {
                    self.exec_match(m, e.span, env)
                } else {
                    let v = self.eval(e, env)?;
                    Ok(Flow::Normal(v))
                }
            }
            StmtKind::If(s) => self.exec_if(s, env),
            StmtKind::While(s) => self.exec_while(s, env),
            StmtKind::For(s) => self.exec_for(s, env),
            StmtKind::Fn(f) => {
                let closure = Closure {
                    kind: ClosureKind::Function(Rc::new(f.clone())),
                    env: Rc::clone(env),
                };
                env_define(env, &f.name, Value::Closure(Rc::new(closure)), false);
                Ok(Flow::Normal(Value::Unit))
            }
            // Type declarations have no runtime effect beyond registry
            // collection (already done before execution).
            StmtKind::Struct(_) | StmtKind::Enum(_) | StmtKind::Impl(_) => {
                Ok(Flow::Normal(Value::Unit))
            }
        }
    }

    /// Execute a block: a fresh child scope, statements in order. The returned
    /// `Flow::Normal` carries the *last* statement's value (block value). Any
    /// `Return`/`Break`/`Continue` short-circuits and is propagated.
    fn exec_block(&mut self, block: &Block, parent: &Env) -> FlowResult {
        let scope = Scope::child(parent);
        self.exec_stmts(&block.stmts, &scope)
    }

    /// Execute statements in the given (already-created) scope.
    fn exec_stmts(&mut self, stmts: &[Stmt], env: &Env) -> FlowResult {
        let mut last = Value::Unit;
        for stmt in stmts {
            match self.exec_stmt(stmt, env)? {
                Flow::Normal(v) => last = v,
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal(last))
    }

    fn exec_if(&mut self, s: &IfStmt, env: &Env) -> FlowResult {
        for (cond, body) in &s.arms {
            if self.eval_bool_cond(cond, env)? {
                return self.exec_block(body, env);
            }
        }
        if let Some(else_body) = &s.else_body {
            return self.exec_block(else_body, env);
        }
        Ok(Flow::Normal(Value::Unit))
    }

    fn exec_while(&mut self, s: &WhileStmt, env: &Env) -> FlowResult {
        while self.eval_bool_cond(&s.cond, env)? {
            match self.exec_block(&s.body, env)? {
                Flow::Normal(_) => {}
                Flow::Continue => continue,
                Flow::Break => break,
                ret @ Flow::Return(_) => return Ok(ret),
            }
        }
        Ok(Flow::Normal(Value::Unit))
    }

    fn exec_for(&mut self, s: &ForStmt, env: &Env) -> FlowResult {
        let iter_val = self.eval(&s.iter, env)?;
        let items = self.iterable_items(&iter_val, s.iter.span)?;
        for item in items {
            // Each iteration gets a fresh scope holding the loop variable(s).
            let scope = Scope::child(env);
            match &s.binder {
                crate::ast::Binder::Name(name) => env_define(&scope, name, item, false),
                // `for (k, v) in …` destructures each element as a tuple.
                crate::ast::Binder::Tuple(names) => {
                    destructure_tuple(names, item, &scope, s.iter.span)?;
                }
            }
            match self.exec_stmts(&s.body.stmts, &scope)? {
                Flow::Normal(_) => {}
                Flow::Continue => continue,
                Flow::Break => break,
                ret @ Flow::Return(_) => return Ok(ret),
            }
        }
        Ok(Flow::Normal(Value::Unit))
    }

    /// Materialize a `for`-iterable into a vector of element values. M1 supports
    /// ranges (already evaluated to lists) and lists.
    fn iterable_items(&self, v: &Value, span: Span) -> Result<Vec<Value>, Diagnostic> {
        match v {
            Value::List(items) => Ok(items.borrow().clone()),
            other => Err(Diagnostic::runtime(
                format!("cannot iterate over {}", type_name(other)),
                span,
            )),
        }
    }

    /// Evaluate a condition that must be `Bool` (no truthiness). Runtime error
    /// otherwise.
    fn eval_bool_cond(&mut self, cond: &Expr, env: &Env) -> Result<bool, Diagnostic> {
        match self.eval(cond, env)? {
            Value::Bool(b) => Ok(b),
            other => Err(Diagnostic::runtime(
                format!(
                    "condition must be Bool, found {}",
                    type_name(&other)
                ),
                cond.span,
            )),
        }
    }

    /// Execute an assignment to a `target` l-value.
    fn exec_assign(&mut self, a: &crate::ast::Assign, env: &Env) -> Result<(), Diagnostic> {
        let new_val = self.eval(&a.value, env)?;
        let target = &a.target;

        if target.path.is_empty() {
            // Plain name reassignment.
            match env_assign(env, &target.base, new_val) {
                Ok(true) => Ok(()),
                Ok(false) => Err(Diagnostic::runtime(
                    format!("cannot assign to undefined name `{}`", target.base),
                    target.span,
                )),
                Err(()) => Err(Diagnostic::runtime(
                    format!("cannot reassign `val` binding `{}`", target.base),
                    target.span,
                )),
            }
        } else {
            // Path assignment: resolve the base, then walk to the penultimate
            // container and write the last segment through shared references.
            let base = env_get(env, &target.base).ok_or_else(|| {
                Diagnostic::runtime(
                    format!("cannot assign to undefined name `{}`", target.base),
                    target.span,
                )
            })?;
            self.assign_path(base, &target.path, new_val, target, env)
        }
    }

    /// Walk a target path on `container`, writing `new_val` at the final
    /// segment. Container is followed by-reference (struct/list are `Rc`s).
    fn assign_path(
        &mut self,
        mut container: Value,
        path: &[TargetSeg],
        new_val: Value,
        target: &Target,
        env: &Env,
    ) -> Result<(), Diagnostic> {
        // Navigate to the container holding the final segment.
        for seg in &path[..path.len() - 1] {
            container = self.navigate_seg(&container, seg, target, env)?;
        }
        let last = &path[path.len() - 1];
        match last {
            TargetSeg::Field(name) => match &container {
                Value::Struct(s) => {
                    let mut inst = s.borrow_mut();
                    if !inst.fields.contains_key(name) {
                        return Err(Diagnostic::runtime(
                            format!("struct `{}` has no field `{}`", inst.type_name, name),
                            target.span,
                        ));
                    }
                    inst.fields.insert(name.clone(), new_val);
                    Ok(())
                }
                other => Err(Diagnostic::runtime(
                    format!("cannot set field `{}` on {}", name, type_name(other)),
                    target.span,
                )),
            },
            TargetSeg::Index(idx_expr) => {
                let idx = self.eval(idx_expr, env)?;
                match &container {
                    Value::List(items) => {
                        let i = self.list_index(&idx, items.borrow().len(), target.span)?;
                        items.borrow_mut()[i] = new_val;
                        Ok(())
                    }
                    other => Err(Diagnostic::runtime(
                        format!("cannot index-assign into {}", type_name(other)),
                        target.span,
                    )),
                }
            }
        }
    }

    /// Read one navigation segment (for intermediate path steps).
    fn navigate_seg(
        &mut self,
        container: &Value,
        seg: &TargetSeg,
        target: &Target,
        env: &Env,
    ) -> EvalResult {
        match seg {
            TargetSeg::Field(name) => match container {
                Value::Struct(s) => s
                    .borrow()
                    .fields
                    .get(name)
                    .cloned()
                    .ok_or_else(|| {
                        Diagnostic::runtime(
                            format!("struct has no field `{}`", name),
                            target.span,
                        )
                    }),
                other => Err(Diagnostic::runtime(
                    format!("cannot access field `{}` on {}", name, type_name(other)),
                    target.span,
                )),
            },
            TargetSeg::Index(idx_expr) => {
                let idx = self.eval(idx_expr, env)?;
                match container {
                    Value::List(items) => {
                        let i = self.list_index(&idx, items.borrow().len(), target.span)?;
                        Ok(items.borrow()[i].clone())
                    }
                    other => Err(Diagnostic::runtime(
                        format!("cannot index {}", type_name(other)),
                        target.span,
                    )),
                }
            }
        }
    }

    /// Convert an index value into a usable `usize`, bounds-checking.
    fn list_index(&self, idx: &Value, len: usize, span: Span) -> Result<usize, Diagnostic> {
        let i = match idx {
            Value::Int(n) => n,
            other => {
                return Err(Diagnostic::runtime(
                    format!("list index must be Int, found {}", type_name(other)),
                    span,
                ));
            }
        };
        if i.is_negative() {
            return Err(Diagnostic::runtime(
                format!("list index out of bounds: {}", i),
                span,
            ));
        }
        let i = i.to_usize().ok_or_else(|| {
            Diagnostic::runtime(format!("list index out of bounds: {}", i), span)
        })?;
        if i >= len {
            return Err(Diagnostic::runtime(
                format!("list index out of bounds: {} (len {})", i, len),
                span,
            ));
        }
        Ok(i)
    }

    // -----------------------------------------------------------------------
    // Expression evaluation
    // -----------------------------------------------------------------------

    fn eval(&mut self, expr: &Expr, env: &Env) -> EvalResult {
        match &expr.kind {
            ExprKind::Int(n) => Ok(Value::Int(n.clone())),
            ExprKind::Float(f) => Ok(Value::Float(*f)),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Null => Ok(Value::Null),
            ExprKind::Str(lit) => self.eval_string_lit(lit, env),
            ExprKind::Name(name) => env_get(env, name).ok_or_else(|| {
                Diagnostic::runtime(format!("undefined name `{}`", name), expr.span)
            }),
            ExprKind::SelfExpr => env_get(env, "self").ok_or_else(|| {
                Diagnostic::runtime("`self` is not bound here".to_string(), expr.span)
            }),
            ExprKind::List(elems) => {
                let mut vals = Vec::with_capacity(elems.len());
                for e in elems {
                    vals.push(self.eval(e, env)?);
                }
                Ok(Value::List(Rc::new(RefCell::new(vals))))
            }
            ExprKind::Lambda(l) => {
                let closure = Closure {
                    kind: ClosureKind::Lambda(Rc::new(l.clone())),
                    env: Rc::clone(env),
                };
                Ok(Value::Closure(Rc::new(closure)))
            }
            ExprKind::Ternary { then, cond, otherwise } => {
                if self.eval_bool_cond(cond, env)? {
                    self.eval(then, env)
                } else {
                    self.eval(otherwise, env)
                }
            }
            ExprKind::Unary { op, operand } => self.eval_unary(*op, operand, env),
            ExprKind::Binary { op, lhs, rhs } => self.eval_binary(*op, lhs, rhs, expr.span, env),
            ExprKind::Call { callee, args } => self.eval_call(callee, args, expr.span, env),
            ExprKind::Index { base, index } => self.eval_index(base, index, expr.span, env),
            ExprKind::Member { base, name, safe } => {
                if *safe {
                    // `?.` safe access: if the receiver is `null`, short-circuit
                    // the whole access to `null` (the member is never read). This
                    // also makes chains `a?.b?.c` propagate `null` link by link.
                    let base_v = self.eval(base, env)?;
                    if matches!(base_v, Value::Null) {
                        return Ok(Value::Null);
                    }
                    return self.member_value(&base_v, name, expr.span);
                }
                self.eval_member(base, name, expr.span, env)
            }
            ExprKind::Match(m) => self.eval_match(m, expr.span, env),
            // ----- M2 collections / comprehensions (Wave 1) -----
            ExprKind::Map(pairs) => {
                let mut entries: Vec<(Value, Value)> = Vec::with_capacity(pairs.len());
                for (k_expr, v_expr) in pairs {
                    let k = self.eval(k_expr, env)?;
                    let v = self.eval(v_expr, env)?;
                    map_insert(&mut entries, k, v);
                }
                Ok(Value::Map(Rc::new(RefCell::new(entries))))
            }
            ExprKind::Set(items) => {
                let mut elems: Vec<Value> = Vec::with_capacity(items.len());
                for e in items {
                    let v = self.eval(e, env)?;
                    set_insert(&mut elems, v);
                }
                Ok(Value::Set(Rc::new(RefCell::new(elems))))
            }
            ExprKind::Tuple(items) => {
                let mut vals = Vec::with_capacity(items.len());
                for e in items {
                    vals.push(self.eval(e, env)?);
                }
                Ok(Value::Tuple(Rc::new(vals)))
            }
            ExprKind::Comprehension(c) => self.eval_comprehension(c, expr.span, env),
        }
    }

    /// Evaluate a comprehension by desugaring to a loop over `iter`: bind the
    /// binder to each element, apply the optional `if` filter (which must be
    /// `Bool` — no truthiness), and collect per the output kind into a `List`,
    /// `Set`, or `Map`. The binder is scoped to each iteration.
    fn eval_comprehension(
        &mut self,
        c: &crate::ast::Comprehension,
        span: Span,
        env: &Env,
    ) -> EvalResult {
        let iter_val = self.eval(&c.iter, env)?;
        let items = self.iterable_items(&iter_val, c.iter.span)?;

        let mut list_out: Vec<Value> = Vec::new();
        let mut set_out: Vec<Value> = Vec::new();
        let mut map_out: Vec<(Value, Value)> = Vec::new();

        for item in items {
            let scope = Scope::child(env);
            self.bind_binder(&c.binder, item, &scope, span)?;
            if let Some(cond) = &c.cond {
                if !self.eval_bool_cond(cond, &scope)? {
                    continue;
                }
            }
            match &c.output {
                crate::ast::ComprehensionOutput::List(e) => {
                    list_out.push(self.eval(e, &scope)?);
                }
                crate::ast::ComprehensionOutput::Set(e) => {
                    let v = self.eval(e, &scope)?;
                    set_insert(&mut set_out, v);
                }
                crate::ast::ComprehensionOutput::Map { key, value } => {
                    let k = self.eval(key, &scope)?;
                    let v = self.eval(value, &scope)?;
                    map_insert(&mut map_out, k, v);
                }
            }
        }

        Ok(match &c.output {
            crate::ast::ComprehensionOutput::List(_) => {
                Value::List(Rc::new(RefCell::new(list_out)))
            }
            crate::ast::ComprehensionOutput::Set(_) => {
                Value::Set(Rc::new(RefCell::new(set_out)))
            }
            crate::ast::ComprehensionOutput::Map { .. } => {
                Value::Map(Rc::new(RefCell::new(map_out)))
            }
        })
    }

    /// Bind a [`ComprehensionBinder`] to an iterated element in `scope`. A single
    /// name binds the whole element; a tuple binder destructures a `Value::Tuple`
    /// of matching arity (a non-tuple or mismatched arity is a runtime error).
    fn bind_binder(
        &self,
        binder: &crate::ast::ComprehensionBinder,
        value: Value,
        scope: &Env,
        span: Span,
    ) -> Result<(), Diagnostic> {
        match binder {
            crate::ast::ComprehensionBinder::Name(n) => {
                env_define(scope, n, value, false);
                Ok(())
            }
            crate::ast::ComprehensionBinder::Tuple(names) => {
                destructure_tuple(names, value, scope, span)
            }
        }
    }

    /// Render a string literal by rendering each segment (text verbatim,
    /// expressions via `Show`).
    fn eval_string_lit(
        &mut self,
        lit: &crate::ast::StringLit,
        env: &Env,
    ) -> EvalResult {
        let mut out = String::new();
        for part in &lit.parts {
            match part {
                StrSeg::Text(t) => out.push_str(t),
                StrSeg::Expr(e) => {
                    let v = self.eval(e, env)?;
                    out.push_str(&show(&v));
                }
            }
        }
        Ok(Value::Str(out))
    }

    fn eval_unary(&mut self, op: UnOp, operand: &Expr, env: &Env) -> EvalResult {
        let v = self.eval(operand, env)?;
        match op {
            UnOp::Not => match v {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                other => Err(Diagnostic::runtime(
                    format!("`not` requires Bool, found {}", type_name(&other)),
                    operand.span,
                )),
            },
            UnOp::Neg => match v {
                Value::Int(n) => Ok(Value::Int(-n)),
                Value::Float(f) => Ok(Value::Float(-f)),
                other => Err(Diagnostic::runtime(
                    format!("unary `-` requires Int or Float, found {}", type_name(&other)),
                    operand.span,
                )),
            },
        }
    }

    fn eval_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        env: &Env,
    ) -> EvalResult {
        // Short-circuiting logical operators (require Bool operands).
        match op {
            BinOp::And => {
                let l = self.eval(lhs, env)?;
                let lb = as_bool(&l, lhs.span, "and")?;
                if !lb {
                    return Ok(Value::Bool(false));
                }
                let r = self.eval(rhs, env)?;
                let rb = as_bool(&r, rhs.span, "and")?;
                return Ok(Value::Bool(rb));
            }
            BinOp::Or => {
                let l = self.eval(lhs, env)?;
                let lb = as_bool(&l, lhs.span, "or")?;
                if lb {
                    return Ok(Value::Bool(true));
                }
                let r = self.eval(rhs, env)?;
                let rb = as_bool(&r, rhs.span, "or")?;
                return Ok(Value::Bool(rb));
            }
            _ => {}
        }

        let l = self.eval(lhs, env)?;
        let r = self.eval(rhs, env)?;

        match op {
            BinOp::And | BinOp::Or => unreachable!("handled above"),

            // Equality (structural) — and `is`/`is not`.
            BinOp::Eq | BinOp::Is => Ok(Value::Bool(values_equal(&l, &r))),
            BinOp::NotEq | BinOp::IsNot => Ok(Value::Bool(!values_equal(&l, &r))),

            // Ordering comparisons.
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                self.eval_compare(op, &l, &r, span)
            }

            // Ranges — eager list of Ints.
            BinOp::Range | BinOp::RangeIncl => self.eval_range(op, &l, &r, span),

            // Arithmetic.
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
                self.eval_arith(op, &l, &r, span)
            }
        }
    }

    fn eval_compare(&self, op: BinOp, l: &Value, r: &Value, span: Span) -> EvalResult {
        let ord = match (l, r) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Int(a), Value::Float(b)) => bigint_to_f64(a).partial_cmp(b),
            (Value::Float(a), Value::Int(b)) => a.partial_cmp(&bigint_to_f64(b)),
            (Value::Str(a), Value::Str(b)) => a.partial_cmp(b),
            _ => {
                return Err(Diagnostic::runtime(
                    format!(
                        "cannot compare {} and {}",
                        type_name(l),
                        type_name(r)
                    ),
                    span,
                ));
            }
        };
        let ord = match ord {
            Some(o) => o,
            // NaN: all ordering comparisons are false.
            None => return Ok(Value::Bool(false)),
        };
        use std::cmp::Ordering::*;
        let res = match op {
            BinOp::Lt => ord == Less,
            BinOp::LtEq => ord == Less || ord == Equal,
            BinOp::Gt => ord == Greater,
            BinOp::GtEq => ord == Greater || ord == Equal,
            _ => unreachable!(),
        };
        Ok(Value::Bool(res))
    }

    fn eval_range(&self, op: BinOp, l: &Value, r: &Value, span: Span) -> EvalResult {
        let (start, end) = match (l, r) {
            (Value::Int(a), Value::Int(b)) => (a.clone(), b.clone()),
            _ => {
                return Err(Diagnostic::runtime(
                    format!(
                        "range bounds must be Int, found {} and {}",
                        type_name(l),
                        type_name(r)
                    ),
                    span,
                ));
            }
        };
        let mut items = Vec::new();
        let mut cur = start;
        let inclusive = matches!(op, BinOp::RangeIncl);
        while (inclusive && cur <= end) || (!inclusive && cur < end) {
            items.push(Value::Int(cur.clone()));
            cur += 1;
        }
        Ok(Value::List(Rc::new(RefCell::new(items))))
    }

    fn eval_arith(&self, op: BinOp, l: &Value, r: &Value, span: Span) -> EvalResult {
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => self.int_arith(op, a, b, span),
            (Value::Float(a), Value::Float(b)) => Ok(self.float_arith(op, *a, *b)),
            // No implicit Int->Float coercion for + - * (would silently lose
            // BigInt precision). Mixed operands are a runtime error to keep the
            // numeric story honest in M1.
            (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
                Err(Diagnostic::runtime(
                    "mixed Int/Float arithmetic is not allowed (convert explicitly)".to_string(),
                    span,
                ))
            }
            // String concatenation with `+`.
            (Value::Str(a), Value::Str(b)) if matches!(op, BinOp::Add) => {
                Ok(Value::Str(format!("{}{}", a, b)))
            }
            _ => Err(Diagnostic::runtime(
                format!(
                    "cannot apply `{}` to {} and {}",
                    binop_symbol(op),
                    type_name(l),
                    type_name(r)
                ),
                span,
            )),
        }
    }

    fn int_arith(&self, op: BinOp, a: &BigInt, b: &BigInt, span: Span) -> EvalResult {
        match op {
            BinOp::Add => Ok(Value::Int(a + b)),
            BinOp::Sub => Ok(Value::Int(a - b)),
            BinOp::Mul => Ok(Value::Int(a * b)),
            BinOp::Div => {
                if b.is_zero() {
                    return Err(Diagnostic::runtime("division by zero".to_string(), span));
                }
                // Int / Int requires an exact result.
                let (q, rem) = num_integer::div_rem(a.clone(), b.clone());
                if !rem.is_zero() {
                    return Err(Diagnostic::runtime(
                        format!(
                            "Int division `{} / {}` is not exact (use Float division)",
                            a, b
                        ),
                        span,
                    ));
                }
                Ok(Value::Int(q))
            }
            BinOp::Rem => {
                if b.is_zero() {
                    return Err(Diagnostic::runtime(
                        "remainder by zero".to_string(),
                        span,
                    ));
                }
                Ok(Value::Int(a % b))
            }
            BinOp::Pow => {
                if b.is_negative() {
                    return Err(Diagnostic::runtime(
                        "negative Int exponent (use Float)".to_string(),
                        span,
                    ));
                }
                let exp = b.to_u32().ok_or_else(|| {
                    Diagnostic::runtime("exponent too large".to_string(), span)
                })?;
                Ok(Value::Int(a.pow(exp)))
            }
            _ => unreachable!(),
        }
    }

    fn float_arith(&self, op: BinOp, a: f64, b: f64) -> Value {
        let r = match op {
            BinOp::Add => a + b,
            BinOp::Sub => a - b,
            BinOp::Mul => a * b,
            BinOp::Div => a / b, // true division (IEEE; inf/nan on /0).
            BinOp::Rem => a % b,
            BinOp::Pow => a.powf(b),
            _ => unreachable!(),
        };
        Value::Float(r)
    }

    fn eval_index(&mut self, base: &Expr, index: &Expr, span: Span, env: &Env) -> EvalResult {
        let base_v = self.eval(base, env)?;
        let idx_v = self.eval(index, env)?;
        match &base_v {
            Value::List(items) => {
                let i = self.list_index(&idx_v, items.borrow().len(), span)?;
                Ok(items.borrow()[i].clone())
            }
            other => Err(Diagnostic::runtime(
                format!("cannot index {}", type_name(other)),
                span,
            )),
        }
    }

    /// Member access `base.name`. Used for struct field reads. (Method calls
    /// flow through `eval_call` which special-cases a `Member` callee.)
    fn eval_member(&mut self, base: &Expr, name: &str, span: Span, env: &Env) -> EvalResult {
        // Qualified niladic enum-variant value: `Enum.Variant` with no call.
        if let ExprKind::Name(enum_name) = &base.kind {
            if env_get(env, enum_name).is_none()
                && self.registry.enums.contains_key(enum_name)
            {
                return self.construct_variant(enum_name, name, &[], span, env);
            }
        }
        let base_v = self.eval(base, env)?;
        self.member_value(&base_v, name, span)
    }

    fn member_value(&self, base_v: &Value, name: &str, span: Span) -> EvalResult {
        match base_v {
            Value::Struct(s) => s
                .borrow()
                .fields
                .get(name)
                .cloned()
                .ok_or_else(|| {
                    Diagnostic::runtime(
                        format!(
                            "struct `{}` has no field `{}`",
                            s.borrow().type_name,
                            name
                        ),
                        span,
                    )
                }),
            other => Err(Diagnostic::runtime(
                format!("cannot access `.{}` on {}", name, type_name(other)),
                span,
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Calls / construction / methods
    // -----------------------------------------------------------------------

    fn eval_call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> EvalResult {
        if let ExprKind::Member { base, name, safe } = &callee.kind {
            // Qualified enum-variant construction: `Enum.Variant(args)`. Only
            // when the base names a known enum that isn't shadowed by a value.
            // `?.` never qualifies an enum (its receiver is a value), so this is
            // limited to the plain-`.` case.
            if let ExprKind::Name(enum_name) = &base.kind {
                if !*safe
                    && env_get(env, enum_name).is_none()
                    && self.registry.enums.contains_key(enum_name)
                {
                    return self.construct_variant(enum_name, name, args, span, env);
                }
            }

            // Otherwise it is a method call `recv.method(args)`.
            let recv = self.eval(base, env)?;

            // `?.method(...)` safe-call: a `null` receiver short-circuits the
            // whole call to `null` — the args are never evaluated and the method
            // never runs (so a chain `a?.b()?.c()` propagates `null`).
            if *safe && matches!(recv, Value::Null) {
                return Ok(Value::Null);
            }

            // Built-in `.or_else(default)` on a possibly-null value.
            if name == "or_else" {
                let arg_vals = self.eval_positional_args(args, span, env)?;
                if arg_vals.len() != 1 {
                    return Err(Diagnostic::runtime(
                        "`.or_else` takes exactly one argument".to_string(),
                        span,
                    ));
                }
                return Ok(match recv {
                    Value::Null => arg_vals.into_iter().next().unwrap(),
                    other => other,
                });
            }

            return self.call_method(recv, name, args, span, env);
        }

        // Construction: a bare name referring to a struct. (Enum variants are
        // qualified — `Enum.Variant(...)` — handled above, not bare.)
        if let ExprKind::Name(name) = &callee.kind {
            // A local binding shadows construction (e.g. a closure named the
            // same). Only treat as construction if NOT bound to a value.
            if env_get(env, name).is_none() && self.registry.structs.contains_key(name) {
                return self.construct_struct(name, args, span, env);
            }
        }

        // Otherwise: an ordinary function/closure/builtin call.
        let callee_v = self.eval(callee, env)?;
        // A named `fn` supports named call args and default values (M2 Wave 1);
        // bind directly from the raw `Arg`s so names/defaults are honoured.
        if let Value::Closure(c) = &callee_v {
            if let ClosureKind::Function(f) = &c.kind {
                let f = Rc::clone(f);
                let c = Rc::clone(c);
                let call_scope = Scope::child(&c.env);
                self.bind_call(&f.params, args, &call_scope, &f.name, span, env)?;
                let flow = self.exec_stmts(&f.body.stmts, &call_scope)?;
                return finish_call(flow, "`break`/`continue` outside a loop", span);
            }
        }
        // Lambdas / builtins take positional args only (no names/defaults).
        let arg_vals = self.eval_positional_args(args, span, env)?;
        self.apply(callee_v, arg_vals, span)
    }

    /// Evaluate args, requiring all positional (for function calls).
    fn eval_positional_args(
        &mut self,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> Result<Vec<Value>, Diagnostic> {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            match a {
                Arg::Positional(e) => vals.push(self.eval(e, env)?),
                Arg::Named { name, .. } => {
                    return Err(Diagnostic::runtime(
                        format!("named argument `{}` is only valid in construction", name),
                        span,
                    ));
                }
            }
        }
        Ok(vals)
    }

    /// Apply a callable value to already-evaluated arguments.
    fn apply(&mut self, callee: Value, args: Vec<Value>, span: Span) -> EvalResult {
        match callee {
            Value::Closure(c) => self.call_closure(&c, args, span),
            Value::Builtin(b) => self.call_builtin(b, args, span),
            other => Err(Diagnostic::runtime(
                format!("{} is not callable", type_name(&other)),
                span,
            )),
        }
    }

    /// Invoke a user closure (function or lambda) with positional args.
    fn call_closure(
        &mut self,
        closure: &Closure,
        args: Vec<Value>,
        span: Span,
    ) -> EvalResult {
        let call_scope = Scope::child(&closure.env);
        match &closure.kind {
            ClosureKind::Function(f) => {
                self.bind_params(&f.params, &args, &call_scope, &f.name, span)?;
                let flow = self.exec_stmts(&f.body.stmts, &call_scope)?;
                finish_call(flow, "`break`/`continue` outside a loop", span)
            }
            ClosureKind::Lambda(l) => {
                if args.len() != l.params.len() {
                    return Err(Diagnostic::runtime(
                        format!(
                            "lambda expects {} argument(s), got {}",
                            l.params.len(),
                            args.len()
                        ),
                        span,
                    ));
                }
                for (p, v) in l.params.iter().zip(args.into_iter()) {
                    env_define(&call_scope, p, v, false);
                }
                // A lambda body is a single expression.
                self.eval(&l.body, &call_scope)
            }
        }
    }

    /// Bind already-evaluated **positional** args to params (the `self` receiver,
    /// if any, is pre-bound by the caller). Trailing params that have a default
    /// value (M2 Wave 1) may be omitted; their defaults are evaluated in `scope`.
    /// Used by the value-call path (`apply` — lambdas-as-functions, `main`).
    fn bind_params(
        &mut self,
        params: &[Param],
        args: &[Value],
        scope: &Env,
        fn_name: &str,
        span: Span,
    ) -> Result<(), Diagnostic> {
        // Non-self params, in declaration order.
        let positional: Vec<&Param> = params
            .iter()
            .filter(|p| !matches!(p, Param::SelfRecv))
            .collect();
        let required = positional
            .iter()
            .take_while(|p| matches!(p, Param::Named { default: None, .. }))
            .count();
        if args.len() < required || args.len() > positional.len() {
            return Err(arity_error(fn_name, required, positional.len(), args.len(), span));
        }
        for (i, p) in positional.iter().enumerate() {
            if let Param::Named { name, default, .. } = p {
                let v = match args.get(i) {
                    Some(v) => v.clone(),
                    None => match default {
                        Some(e) => self.eval(e, scope)?,
                        None => {
                            return Err(arity_error(
                                fn_name, required, positional.len(), args.len(), span,
                            ));
                        }
                    },
                };
                env_define(scope, name, v, false);
            }
        }
        Ok(())
    }

    /// Bind a function/method call's raw [`Arg`]s to `params`, honouring
    /// **named arguments** and **default values** (M2 Wave 1). Positional args
    /// fill params left-to-right; named args match by parameter name; any param
    /// left unfilled uses its default or is an arity/missing-argument error.
    /// Args are evaluated in `caller_env`; defaults in the new `scope`.
    fn bind_call(
        &mut self,
        params: &[Param],
        args: &[Arg],
        scope: &Env,
        fn_name: &str,
        span: Span,
        caller_env: &Env,
    ) -> Result<(), Diagnostic> {
        let positional_params: Vec<&Param> = params
            .iter()
            .filter(|p| !matches!(p, Param::SelfRecv))
            .collect();

        // Split call args into positional values and a name → value map. A named
        // arg may not be followed by a positional one (keeps the mapping clear).
        let mut pos_vals: Vec<Value> = Vec::new();
        let mut named_vals: HashMap<String, Value> = HashMap::new();
        let mut seen_named = false;
        for a in args {
            match a {
                Arg::Positional(e) => {
                    if seen_named {
                        return Err(Diagnostic::runtime(
                            format!(
                                "`{}`: positional argument after a named argument",
                                fn_name
                            ),
                            span,
                        ));
                    }
                    pos_vals.push(self.eval(e, caller_env)?);
                }
                Arg::Named { name, value } => {
                    seen_named = true;
                    if named_vals.contains_key(name) {
                        return Err(Diagnostic::runtime(
                            format!("`{}`: duplicate argument `{}`", fn_name, name),
                            span,
                        ));
                    }
                    named_vals.insert(name.clone(), self.eval(value, caller_env)?);
                }
            }
        }

        if pos_vals.len() > positional_params.len() {
            return Err(arity_error(
                fn_name,
                required_count(&positional_params),
                positional_params.len(),
                pos_vals.len(),
                span,
            ));
        }

        // Bind each param: a positional value (by position), else a named value
        // (by name), else its default, else an error.
        let mut pos_iter = pos_vals.into_iter();
        for p in &positional_params {
            if let Param::Named { name, default, .. } = p {
                let v = if let Some(v) = pos_iter.next() {
                    if named_vals.contains_key(name) {
                        return Err(Diagnostic::runtime(
                            format!(
                                "`{}`: argument `{}` given both positionally and by name",
                                fn_name, name
                            ),
                            span,
                        ));
                    }
                    v
                } else if let Some(v) = named_vals.remove(name) {
                    v
                } else if let Some(e) = default {
                    self.eval(e, scope)?
                } else {
                    return Err(Diagnostic::runtime(
                        format!("`{}`: missing argument `{}`", fn_name, name),
                        span,
                    ));
                };
                env_define(scope, name, v, false);
            }
        }

        // Any leftover named args don't correspond to a parameter.
        if let Some(extra) = named_vals.keys().next() {
            return Err(Diagnostic::runtime(
                format!("`{}` has no parameter named `{}`", fn_name, extra),
                span,
            ));
        }
        Ok(())
    }

    /// Resolve and call a method `recv.name(args)`.
    ///
    /// `.expect(msg)` is intercepted first: it is the null-assertion sugar valid
    /// on *any* receiver (including `Null` itself), so it cannot live in a
    /// per-type method table. Only user `Struct`/`Enum` receivers then resolve to
    /// declared `impl` methods. All other receiver types (`List`, `Str`, `Map`,
    /// `Set`, `Tuple`, and range-lists) route to [`Self::call_builtin_method`] —
    /// the built-in method table that Wave 1-A fills in.
    fn call_method(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> EvalResult {
        // `.expect(msg)` — assert non-null. Intercepted before any type-based
        // routing because it applies to a nullable value of any underlying type
        // (and to `Null`). A `null` receiver `panic`s with `msg` (a runtime
        // error, like the `panic` builtin); otherwise the value passes through
        // unchanged, now known non-null.
        if name == "expect" {
            let arg_vals = self.eval_positional_args(args, span, env)?;
            if arg_vals.len() != 1 {
                return Err(Diagnostic::runtime(
                    "`.expect` takes exactly one argument".to_string(),
                    span,
                ));
            }
            if matches!(recv, Value::Null) {
                let msg = match arg_vals.first() {
                    Some(Value::Str(s)) => s.clone(),
                    Some(v) => show(v),
                    None => "expect".to_string(),
                };
                return Err(Diagnostic::runtime(format!("panic: {}", msg), span));
            }
            return Ok(recv);
        }

        let type_name_str = match &recv {
            Value::Struct(s) => s.borrow().type_name.clone(),
            Value::Enum(e) => e.enum_name.clone(),
            // Built-in receiver types: dispatch to the built-in method table.
            _ => return self.call_builtin_method(recv, name, args, span, env),
        };

        let method = self
            .registry
            .methods
            .get(&(type_name_str.clone(), name.to_string()))
            .cloned()
            .ok_or_else(|| {
                Diagnostic::runtime(
                    format!("type `{}` has no method `{}`", type_name_str, name),
                    span,
                )
            })?;

        // Method scope is a child of the *global* root env (captured via the
        // env chain). Methods resolve other top-level names through the call
        // env's root; we use the call-site env's chain root so globals (other
        // fns) are visible. Bind `self` first, then params (named args + default
        // values honoured, M2 Wave 1).
        let method_scope = Scope::child(&root_of(env));
        env_define(&method_scope, "self", recv, false);
        self.bind_call(&method.params, args, &method_scope, &method.name, span, env)?;

        let flow = self.exec_stmts(&method.body.stmts, &method_scope)?;
        finish_call(flow, "`break`/`continue` outside a loop", span)
    }

    fn construct_struct(
        &mut self,
        name: &str,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> EvalResult {
        let decl = self.registry.structs.get(name).cloned().unwrap();
        let field_order: Vec<String> = decl.fields.iter().map(|f| f.name.clone()).collect();
        let mut fields: HashMap<String, Value> = HashMap::new();

        let all_named = args.iter().all(|a| matches!(a, Arg::Named { .. }));
        let all_positional = args.iter().all(|a| matches!(a, Arg::Positional(_)));

        if !args.is_empty() && !all_named && !all_positional {
            return Err(Diagnostic::runtime(
                format!("`{}` construction mixes positional and named arguments", name),
                span,
            ));
        }

        if all_named && !args.is_empty() {
            for a in args {
                if let Arg::Named { name: fname, value } = a {
                    if !field_order.contains(fname) {
                        return Err(Diagnostic::runtime(
                            format!("struct `{}` has no field `{}`", name, fname),
                            span,
                        ));
                    }
                    let v = self.eval(value, env)?;
                    fields.insert(fname.clone(), v);
                }
            }
        } else {
            if args.len() != field_order.len() {
                return Err(Diagnostic::runtime(
                    format!(
                        "struct `{}` expects {} field(s), got {}",
                        name,
                        field_order.len(),
                        args.len()
                    ),
                    span,
                ));
            }
            for (fdecl, a) in decl.fields.iter().zip(args.iter()) {
                if let Arg::Positional(e) = a {
                    let v = self.eval(e, env)?;
                    fields.insert(fdecl.name.clone(), v);
                }
            }
        }

        // Ensure all fields are present.
        for f in &field_order {
            if !fields.contains_key(f) {
                return Err(Diagnostic::runtime(
                    format!("struct `{}` is missing field `{}`", name, f),
                    span,
                ));
            }
        }

        Ok(Value::Struct(Rc::new(RefCell::new(StructInstance {
            type_name: name.to_string(),
            fields,
            field_order,
        }))))
    }

    fn construct_variant(
        &mut self,
        enum_name: &str,
        variant: &str,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> EvalResult {
        let decl = self.registry.enums.get(enum_name).cloned().unwrap();
        let vdecl = match decl.variants.iter().find(|v| v.name == variant) {
            Some(v) => v,
            None => {
                return Err(Diagnostic::runtime(
                    format!("enum `{}` has no variant `{}`", enum_name, variant),
                    span,
                ));
            }
        };

        let mut payload = Vec::new();
        let mut payload_names = Vec::new();

        match &vdecl.payload {
            None => {
                if !args.is_empty() {
                    return Err(Diagnostic::runtime(
                        format!("variant `{}` takes no payload", variant),
                        span,
                    ));
                }
            }
            Some(Payload::Positional(types)) => {
                if args.len() != types.len() {
                    return Err(Diagnostic::runtime(
                        format!(
                            "variant `{}` expects {} value(s), got {}",
                            variant,
                            types.len(),
                            args.len()
                        ),
                        span,
                    ));
                }
                for a in args {
                    match a {
                        Arg::Positional(e) => payload.push(self.eval(e, env)?),
                        Arg::Named { name, .. } => {
                            return Err(Diagnostic::runtime(
                                format!(
                                    "variant `{}` has positional payload; `{}:` is invalid",
                                    variant, name
                                ),
                                span,
                            ));
                        }
                    }
                }
            }
            Some(Payload::Named(named_types)) => {
                // Accept named (matched by name) or positional (by order).
                let all_named = args.iter().all(|a| matches!(a, Arg::Named { .. }));
                if args.len() != named_types.len() {
                    return Err(Diagnostic::runtime(
                        format!(
                            "variant `{}` expects {} field(s), got {}",
                            variant,
                            named_types.len(),
                            args.len()
                        ),
                        span,
                    ));
                }
                if all_named && !args.is_empty() {
                    // Build by declaration order, looking each up by name.
                    for (fname, _ty) in named_types {
                        let arg = args.iter().find_map(|a| match a {
                            Arg::Named { name, value } if name == fname => Some(value),
                            _ => None,
                        });
                        match arg {
                            Some(e) => {
                                payload.push(self.eval(e, env)?);
                                payload_names.push(fname.clone());
                            }
                            None => {
                                return Err(Diagnostic::runtime(
                                    format!(
                                        "variant `{}` is missing field `{}`",
                                        variant, fname
                                    ),
                                    span,
                                ));
                            }
                        }
                    }
                } else {
                    for ((fname, _ty), a) in named_types.iter().zip(args.iter()) {
                        match a {
                            Arg::Positional(e) => {
                                payload.push(self.eval(e, env)?);
                                payload_names.push(fname.clone());
                            }
                            Arg::Named { name, value } => {
                                payload.push(self.eval(value, env)?);
                                payload_names.push(name.clone());
                            }
                        }
                    }
                }
            }
        }

        Ok(Value::Enum(Rc::new(EnumInstance {
            enum_name: enum_name.to_string(),
            variant: variant.to_string(),
            payload,
            payload_names,
        })))
    }

    // -----------------------------------------------------------------------
    // Match
    // -----------------------------------------------------------------------

    /// Run a `match` and return the chosen arm body's [`Flow`] verbatim: a
    /// `return`/`break`/`continue` inside the arm propagates out unchanged, so a
    /// `match` in statement position behaves like the block it wraps. The
    /// expression form ([`eval_match`]) collapses that flow into a value.
    fn exec_match(&mut self, m: &MatchExpr, span: Span, env: &Env) -> FlowResult {
        let scrutinee = self.eval(&m.scrutinee, env)?;
        for arm in &m.arms {
            let arm_scope = Scope::child(env);
            if !self.try_match(&arm.pattern, &scrutinee, &arm_scope)? {
                continue;
            }
            // A guard (`pattern if cond:`) is evaluated in the arm scope, so it
            // sees the pattern's bindings. It must be `Bool` (no truthiness); a
            // false guard falls through to the next arm (M2 Wave 2).
            if let Some(guard) = &arm.guard {
                if !self.eval_bool_cond(guard, &arm_scope)? {
                    continue;
                }
            }
            return self.exec_stmts(&arm.body.stmts, &arm_scope);
        }
        Err(Diagnostic::runtime(
            "no match arm matched (non-exhaustive match)".to_string(),
            span,
        ))
    }

    /// Evaluate a `match` used as an expression: run it and collapse the chosen
    /// arm's flow into a value. A trailing-expression arm yields its value; an
    /// explicit `return` collapses to the arm value here, since an expression
    /// cannot unwind the enclosing call — use the statement form (handled in
    /// [`exec_stmt`] via [`exec_match`]) when a real `return` is intended. A
    /// `break`/`continue` has no loop to target through an expression and is a
    /// runtime error.
    fn eval_match(&mut self, m: &MatchExpr, span: Span, env: &Env) -> EvalResult {
        match self.exec_match(m, span, env)? {
            Flow::Normal(v) | Flow::Return(v) => Ok(v),
            Flow::Break | Flow::Continue => Err(Diagnostic::runtime(
                "`break`/`continue` not allowed in a match arm".to_string(),
                span,
            )),
        }
    }

    /// Try a pattern against a value, binding names into `scope` on success.
    fn try_match(
        &mut self,
        pat: &Pattern,
        val: &Value,
        scope: &Env,
    ) -> Result<bool, Diagnostic> {
        match &pat.kind {
            PatternKind::Wildcard => Ok(true),
            PatternKind::Null => Ok(matches!(val, Value::Null)),
            PatternKind::Literal(lit) => Ok(lit_matches(lit, val)),
            PatternKind::Binding(name) => {
                env_define(scope, name, val.clone(), false);
                Ok(true)
            }
            PatternKind::Variant { enum_name, name, subs } => {
                let inst = match val {
                    Value::Enum(e) => e,
                    _ => return Ok(false),
                };
                // A qualified pattern `Enum.Variant` must match the value's enum.
                if let Some(en) = enum_name {
                    if &inst.enum_name != en {
                        return Ok(false);
                    }
                }
                if &inst.variant != name {
                    return Ok(false);
                }
                if subs.len() != inst.payload.len() {
                    return Ok(false);
                }
                // Sub-patterns are full patterns (recursive as of M2); the flat
                // M1 cases (`_`, name, `null`, literal) recurse here unchanged.
                for (sub, payload_v) in subs.iter().zip(inst.payload.iter()) {
                    if !self.try_match(sub, payload_v, scope)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            // An or-pattern matches if ANY alternative matches; the first
            // matching alternative's bindings stick (M2). Alternatives are tried
            // left-to-right against the *same* arm scope. A non-firing arm is
            // discarded by `eval_match` (it builds a fresh scope per arm), so no
            // stray binding from a failed alternative ever escapes.
            PatternKind::Or(alts) => {
                for alt in alts {
                    if self.try_match(alt, val, scope)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            // A tuple pattern matches a `Value::Tuple` of equal arity, recursing
            // element-wise (M2). Nested patterns thus destructure deeply.
            PatternKind::Tuple(elems) => {
                let items = match val {
                    Value::Tuple(items) => items,
                    _ => return Ok(false),
                };
                if elems.len() != items.len() {
                    return Ok(false);
                }
                for (sub, item) in elems.iter().zip(items.iter()) {
                    if !self.try_match(sub, item, scope)? {
                        return Ok(false);
                    }
                }
                Ok(true)
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
