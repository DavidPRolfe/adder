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
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::ast::{
    Arg, BinOp, Block, EnumDecl, Expr, ExprKind, FnDecl, ForStmt, IfStmt, Lambda, LitPattern,
    MatchExpr, Param, Pattern, PatternKind, Payload, Program, Stmt, StmtKind, StrSeg, StructDecl,
    Target, TargetSeg, UnOp, WhileStmt,
};
use crate::error::Diagnostic;
use crate::token::Span;

/// A runtime value (grammar §5 / scope "Values & types").
///
/// `Clone` is cheap for the heap-backed variants because they wrap `Rc`. The
/// reference-counted/interior-mutable shapes (`List`, `Struct`, `Closure`,
/// `Env`) give closures capture-by-reference and structs mutable fields.
///
/// `PartialEq` is derived for convenience, but the language's structural `==`
/// (and `is`/`is not`) is implemented separately by the interpreter agent —
/// e.g. `Closure`/`Builtin` equality is not meaningful and `Float` follows IEEE
/// rules. Do not rely on the derived impl for language semantics.
#[derive(Debug, Clone)]
pub enum Value {
    /// Arbitrary-precision integer.
    Int(BigInt),
    /// 64-bit float.
    Float(f64),
    /// Boolean.
    Bool(bool),
    /// String (already-rendered text; not interpolation parts).
    Str(String),
    /// A list, mutable and shared by reference.
    List(Rc<RefCell<Vec<Value>>>),
    /// A map (M2), insertion-ordered for stable `Show`. Stored as a `Vec` of
    /// key/value pairs rather than a hash map because keys are arbitrary
    /// (structurally-hashed) runtime values and insertion order must be
    /// preserved; mutable and shared by reference.
    /// TODO(W1-B): produced and consumed by map literals / comprehensions /
    /// built-in methods.
    Map(Rc<RefCell<Vec<(Value, Value)>>>),
    /// A set (M2), insertion-ordered, deduplicated by structural equality.
    /// Stored as a `Vec` for the same reasons as [`Value::Map`]; mutable and
    /// shared by reference.
    /// TODO(W1-B): produced and consumed by set literals / comprehensions /
    /// built-in methods.
    Set(Rc<RefCell<Vec<Value>>>),
    /// A tuple (M2): a fixed, immutable sequence of values. Shared by reference
    /// (the contents never mutate, so no `RefCell`).
    /// TODO(W1-B): produced and consumed by tuple literals / patterns.
    Tuple(Rc<Vec<Value>>),
    /// The unit value `()`.
    Unit,
    /// The `null` value.
    Null,
    /// A struct instance: its type name plus its (mutable) named fields.
    Struct(Rc<RefCell<StructInstance>>),
    /// An enum instance: its enum/variant plus payload values.
    Enum(Rc<EnumInstance>),
    /// A user closure capturing its defining environment by reference.
    Closure(Rc<Closure>),
    /// A built-in (prelude) function such as `print` / `panic`.
    Builtin(Builtin),
}

/// A struct instance's runtime data.
#[derive(Debug, Clone)]
pub struct StructInstance {
    /// The struct type's name.
    pub type_name: String,
    /// Field values keyed by field name (mutable).
    pub fields: HashMap<String, Value>,
    /// Field order, for stable `Show` rendering.
    pub field_order: Vec<String>,
}

/// An enum instance's runtime data.
#[derive(Debug, Clone)]
pub struct EnumInstance {
    /// The enum type's name.
    pub enum_name: String,
    /// The active variant's name.
    pub variant: String,
    /// Payload values. For named payloads, order matches the declaration; the
    /// interpreter agent may also track field names if needed for `Show`.
    pub payload: Vec<Value>,
    /// For named payloads, the field names (parallel to `payload`). Empty for
    /// positional or niladic variants.
    pub payload_names: Vec<String>,
}

/// A user-defined closure.
///
/// Holds the function/lambda body and the environment captured **by
/// reference** at creation time (closures see later mutations to captured
/// bindings).
#[derive(Debug)]
pub struct Closure {
    /// The closure's source. A named `fn` or an anonymous lambda.
    pub kind: ClosureKind,
    /// The captured defining environment (shared by reference).
    pub env: Env,
}

/// What a [`Closure`] wraps.
#[derive(Debug, Clone)]
pub enum ClosureKind {
    /// A declared function (carries params, returns, body).
    Function(Rc<FnDecl>),
    /// An anonymous lambda.
    Lambda(Rc<Lambda>),
}

/// A built-in prelude function, identified by tag. The interpreter dispatches
/// on the tag; new built-ins can be added without grammar changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// `print(args, …)` — writes the `Show` rendering of each argument.
    Print,
    /// `panic(msg)` — aborts with `msg`; never returns normally.
    Panic,
    /// `Set()` — constructs an empty set (the `{}` literal is an empty *map*,
    /// so the empty set needs its own spelling; see spec §3).
    Set,
}

/// A lexical environment / scope, shared and mutable by reference.
///
/// `Rc<RefCell<Scope>>` so closures capture by reference and nested scopes can
/// chain to their parent. Walk `parent` to resolve a name; the innermost scope
/// holding it wins.
pub type Env = Rc<RefCell<Scope>>;

/// One scope frame: its bindings and an optional parent.
#[derive(Debug, Default)]
pub struct Scope {
    /// Name → (value, is_immutable). The `is_val` flag backs the runtime
    /// `val`-reassignment check.
    pub vars: HashMap<String, Binding>,
    /// Enclosing scope, if any.
    pub parent: Option<Env>,
}

/// A single binding slot in a [`Scope`].
#[derive(Debug, Clone)]
pub struct Binding {
    pub value: Value,
    /// `true` if bound with `val` (reassignment is a runtime error).
    pub is_val: bool,
}

impl Scope {
    /// A fresh empty root scope wrapped as an [`Env`].
    pub fn new_root() -> Env {
        Rc::new(RefCell::new(Scope::default()))
    }

    /// A fresh child scope whose parent is `parent`.
    pub fn child(parent: &Env) -> Env {
        Rc::new(RefCell::new(Scope {
            vars: HashMap::new(),
            parent: Some(Rc::clone(parent)),
        }))
    }
}

// ===========================================================================
// Environment helpers
// ===========================================================================

/// Insert (or overwrite) a binding in *this* scope frame.
fn env_define(env: &Env, name: &str, value: Value, is_val: bool) {
    env.borrow_mut()
        .vars
        .insert(name.to_string(), Binding { value, is_val });
}

/// Resolve a name, walking the parent chain. Returns the bound value.
fn env_get(env: &Env, name: &str) -> Option<Value> {
    let scope = env.borrow();
    if let Some(b) = scope.vars.get(name) {
        return Some(b.value.clone());
    }
    let parent = scope.parent.clone();
    drop(scope);
    match parent {
        Some(p) => env_get(&p, name),
        None => None,
    }
}

/// Reassign an existing name (walking parents). Returns:
/// - `Ok(true)`  — reassigned successfully.
/// - `Ok(false)` — name not found anywhere.
/// - `Err(())`   — found but bound as `val` (immutable).
fn env_assign(env: &Env, name: &str, value: Value) -> Result<bool, ()> {
    let mut scope = env.borrow_mut();
    if let Some(b) = scope.vars.get_mut(name) {
        if b.is_val {
            return Err(());
        }
        b.value = value;
        return Ok(true);
    }
    let parent = scope.parent.clone();
    drop(scope);
    match parent {
        Some(p) => env_assign(&p, name, value),
        None => Ok(false),
    }
}

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
struct Interp {
    registry: Registry,
}

// ===========================================================================
// Entry point
// ===========================================================================

/// Run a checked [`Program`]: seed the prelude, execute top-level statements in
/// order, then call a zero-arg `main()` if one exists.
pub fn run(program: &Program) -> Result<(), Diagnostic> {
    let mut interp = Interp { registry: Registry::default() };
    let root = Scope::new_root();

    // 1. Seed the prelude.
    seed_prelude(&root);

    // 2. Collect declarations (struct/enum/impl/inline methods) so order does
    //    not matter for construction or method lookup.
    interp.collect_decls(program);

    // 3. Execute top-level statements in order.
    for stmt in &program.stmts {
        match interp.exec_stmt(stmt, &root)? {
            Flow::Normal(_) => {}
            // `return`/`break`/`continue` at top level are meaningless; ignore
            // the unwind (the parser/checks are expected to reject them, but we
            // do not crash).
            _ => {}
        }
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

impl Interp {
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
                let v = self.eval(e, env)?;
                Ok(Flow::Normal(v))
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
                return match self.exec_stmts(&f.body.stmts, &call_scope)? {
                    Flow::Normal(v) | Flow::Return(v) => Ok(v),
                    Flow::Break | Flow::Continue => Err(Diagnostic::runtime(
                        "`break`/`continue` outside a loop".to_string(),
                        span,
                    )),
                };
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
                match self.exec_stmts(&f.body.stmts, &call_scope)? {
                    Flow::Normal(v) => Ok(v),
                    Flow::Return(v) => Ok(v),
                    Flow::Break | Flow::Continue => Err(Diagnostic::runtime(
                        "`break`/`continue` outside a loop".to_string(),
                        span,
                    )),
                }
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

    fn call_builtin(&mut self, b: Builtin, args: Vec<Value>, span: Span) -> EvalResult {
        match b {
            Builtin::Print => {
                let rendered: Vec<String> = args.iter().map(show).collect();
                println!("{}", rendered.join(" "));
                Ok(Value::Unit)
            }
            Builtin::Panic => {
                let msg = match args.first() {
                    Some(Value::Str(s)) => s.clone(),
                    Some(v) => show(v),
                    None => "panic".to_string(),
                };
                Err(Diagnostic::runtime(format!("panic: {}", msg), span))
            }
            Builtin::Set => {
                if !args.is_empty() {
                    return Err(Diagnostic::runtime(
                        "`Set()` takes no arguments; use `{a, b, …}` for a non-empty set"
                            .to_string(),
                        span,
                    ));
                }
                Ok(Value::Set(Rc::new(RefCell::new(Vec::new()))))
            }
        }
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

        match self.exec_stmts(&method.body.stmts, &method_scope)? {
            Flow::Normal(v) => Ok(v),
            Flow::Return(v) => Ok(v),
            Flow::Break | Flow::Continue => Err(Diagnostic::runtime(
                "`break`/`continue` outside a loop".to_string(),
                span,
            )),
        }
    }

    /// The **built-in method table** for non-user receiver types — `List`,
    /// `Str`, `Map`, `Set`, `Tuple`, and range-lists (M2 Wave 1-A). This is the
    /// home for the eager iterator pipeline (`map`/`filter`/`fold`/…) and the
    /// `Map`/`Set` methods (`get`/`insert`/`keys`/…).
    ///
    /// Dispatch is eager: transforming stages return a fresh `Value::List`,
    /// terminal stages return scalars. Args are evaluated here (named arguments
    /// are rejected — built-in methods take positional args only). Higher-order
    /// methods (`map`/`filter`/…) receive a callable `Value` (a `Closure` or
    /// `Builtin`) and invoke it per element through [`Self::apply`], the same
    /// path an ordinary call uses (so wrong-arity is the usual runtime error).
    ///
    /// Ranges (`0..n`) are already materialized to `Value::List` by
    /// [`Self::eval`], so the `List` arm covers them for free.
    fn call_builtin_method(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> EvalResult {
        // Built-in methods take positional args only; reject named ones with a
        // clear message before we touch the receiver.
        let arg_vals = self.eval_builtin_args(name, args, span, env)?;

        match &recv {
            Value::List(items) => self.list_method(items, name, arg_vals, span),
            Value::Map(pairs) => self.map_method(pairs, name, arg_vals, span),
            Value::Set(items) => self.set_method(items, name, arg_vals, span),
            Value::Str(s) => Self::str_method(s, name, &arg_vals, span),
            other => Err(Diagnostic::runtime(
                format!("type `{}` has no method `{}`", type_name(other), name),
                span,
            )),
        }
    }

    /// Evaluate built-in-method arguments, rejecting *named* arguments (which
    /// are only meaningful in struct/enum construction).
    fn eval_builtin_args(
        &mut self,
        name: &str,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> Result<Vec<Value>, Diagnostic> {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            match a {
                Arg::Positional(e) => vals.push(self.eval(e, env)?),
                Arg::Named { name: arg, .. } => {
                    return Err(Diagnostic::runtime(
                        format!(
                            "built-in method `.{}` takes positional arguments only; \
                             named argument `{}` is not allowed",
                            name, arg
                        ),
                        span,
                    ));
                }
            }
        }
        Ok(vals)
    }

    // ---- List methods (also cover ranges, which are lists) ----------------

    /// Dispatch a built-in `List` method. Transforms return a fresh
    /// `Value::List`; terminals return scalars or `T?`.
    fn list_method(
        &mut self,
        items: &Rc<RefCell<Vec<Value>>>,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> EvalResult {
        match name {
            // -- transforms: map / filter ----------------------------------
            "map" => {
                let f = arg1(name, args, span)?;
                let src = items.borrow().clone();
                let mut out = Vec::with_capacity(src.len());
                for v in src {
                    out.push(self.apply(f.clone(), vec![v], span)?);
                }
                Ok(list_value(out))
            }
            "filter" => {
                let p = arg1(name, args, span)?;
                let src = items.borrow().clone();
                let mut out = Vec::new();
                for v in src {
                    if self.apply_predicate(&p, v.clone(), span)? {
                        out.push(v);
                    }
                }
                Ok(list_value(out))
            }
            // -- terminals: fold / reduce ----------------------------------
            "fold" => {
                let (init, f) = arg2(name, args, span)?;
                let src = items.borrow().clone();
                let mut acc = init;
                for v in src {
                    acc = self.apply(f.clone(), vec![acc, v], span)?;
                }
                Ok(acc)
            }
            "reduce" => {
                let f = arg1(name, args, span)?;
                let src = items.borrow().clone();
                let mut iter = src.into_iter();
                let mut acc = iter.next().ok_or_else(|| {
                    Diagnostic::runtime(
                        "`reduce` on an empty list has no result".to_string(),
                        span,
                    )
                })?;
                for v in iter {
                    acc = self.apply(f.clone(), vec![acc, v], span)?;
                }
                Ok(acc)
            }
            // -- terminals: numeric / size ---------------------------------
            "sum" => {
                arg0(name, &args, span)?;
                sum_values(&items.borrow(), span)
            }
            "count" | "len" => {
                arg0(name, &args, span)?;
                Ok(Value::Int(BigInt::from(items.borrow().len())))
            }
            "is_empty" => {
                arg0(name, &args, span)?;
                Ok(Value::Bool(items.borrow().is_empty()))
            }
            // -- terminals: predicates -------------------------------------
            "any" => {
                let p = arg1(name, args, span)?;
                let src = items.borrow().clone();
                for v in src {
                    if self.apply_predicate(&p, v, span)? {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            "all" => {
                let p = arg1(name, args, span)?;
                let src = items.borrow().clone();
                for v in src {
                    if !self.apply_predicate(&p, v, span)? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            "find" => {
                let p = arg1(name, args, span)?;
                let src = items.borrow().clone();
                for v in src {
                    if self.apply_predicate(&p, v.clone(), span)? {
                        return Ok(v);
                    }
                }
                Ok(Value::Null)
            }
            "contains" => {
                let x = arg1(name, args, span)?;
                let found = items.borrow().iter().any(|v| values_equal(v, &x));
                Ok(Value::Bool(found))
            }
            // -- terminals: positional -------------------------------------
            "first" => {
                arg0(name, &args, span)?;
                Ok(items.borrow().first().cloned().unwrap_or(Value::Null))
            }
            "last" => {
                arg0(name, &args, span)?;
                Ok(items.borrow().last().cloned().unwrap_or(Value::Null))
            }
            "min" => {
                arg0(name, &args, span)?;
                extreme(&items.borrow(), Ordering::Less, span)
            }
            "max" => {
                arg0(name, &args, span)?;
                extreme(&items.borrow(), Ordering::Greater, span)
            }
            // -- transforms: slicing ---------------------------------------
            "take" => {
                let n = arg1(name, args, span)?;
                let n = as_count(&n, "take", span)?;
                let out: Vec<Value> = items.borrow().iter().take(n).cloned().collect();
                Ok(list_value(out))
            }
            "skip" => {
                let n = arg1(name, args, span)?;
                let n = as_count(&n, "skip", span)?;
                let out: Vec<Value> = items.borrow().iter().skip(n).cloned().collect();
                Ok(list_value(out))
            }
            // -- transforms: structural ------------------------------------
            "enumerate" => {
                arg0(name, &args, span)?;
                let out: Vec<Value> = items
                    .borrow()
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        Value::Tuple(Rc::new(vec![Value::Int(BigInt::from(i)), v.clone()]))
                    })
                    .collect();
                Ok(list_value(out))
            }
            "zip" => {
                let other = arg1(name, args, span)?;
                let other = as_list(&other, "zip", span)?;
                let out: Vec<Value> = items
                    .borrow()
                    .iter()
                    .zip(other.borrow().iter())
                    .map(|(a, b)| Value::Tuple(Rc::new(vec![a.clone(), b.clone()])))
                    .collect();
                Ok(list_value(out))
            }
            "reverse" => {
                arg0(name, &args, span)?;
                let mut out = items.borrow().clone();
                out.reverse();
                Ok(list_value(out))
            }
            "sorted" => {
                arg0(name, &args, span)?;
                let mut out = items.borrow().clone();
                sort_values(&mut out, span)?;
                Ok(list_value(out))
            }
            // `collect` is the eager identity — the pipeline already produced a
            // concrete list. It returns a fresh list (a copy) for parity with
            // the lazy spelling where `collect` forces materialization.
            "collect" => {
                arg0(name, &args, span)?;
                Ok(list_value(items.borrow().clone()))
            }
            // -- in-place mutation -----------------------------------------
            "append" => {
                let x = arg1(name, args, span)?;
                items.borrow_mut().push(x);
                Ok(Value::Unit)
            }
            "pop_last" => {
                arg0(name, &args, span)?;
                Ok(items.borrow_mut().pop().unwrap_or(Value::Null))
            }
            _ => Err(Diagnostic::runtime(
                format!("`List` has no method `{}`", name),
                span,
            )),
        }
    }

    // ---- Map methods -------------------------------------------------------

    /// Dispatch a built-in `Map` method. Insertion-ordered `Vec` of pairs;
    /// lookups and key overwrites use structural equality via linear scan.
    fn map_method(
        &mut self,
        pairs: &Rc<RefCell<Vec<(Value, Value)>>>,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> EvalResult {
        match name {
            "get" => {
                let k = arg1(name, args, span)?;
                let found = pairs
                    .borrow()
                    .iter()
                    .find(|(key, _)| values_equal(key, &k))
                    .map(|(_, v)| v.clone());
                Ok(found.unwrap_or(Value::Null))
            }
            "insert" => {
                let (k, v) = arg2(name, args, span)?;
                let mut map = pairs.borrow_mut();
                if let Some(slot) = map.iter_mut().find(|(key, _)| values_equal(key, &k)) {
                    slot.1 = v; // overwrite existing key, preserving its position
                } else {
                    map.push((k, v));
                }
                Ok(Value::Unit)
            }
            "contains" | "has" => {
                let k = arg1(name, args, span)?;
                let found = pairs.borrow().iter().any(|(key, _)| values_equal(key, &k));
                Ok(Value::Bool(found))
            }
            "keys" => {
                arg0(name, &args, span)?;
                let out: Vec<Value> = pairs.borrow().iter().map(|(k, _)| k.clone()).collect();
                Ok(list_value(out))
            }
            "values" => {
                arg0(name, &args, span)?;
                let out: Vec<Value> = pairs.borrow().iter().map(|(_, v)| v.clone()).collect();
                Ok(list_value(out))
            }
            "items" => {
                arg0(name, &args, span)?;
                let out: Vec<Value> = pairs
                    .borrow()
                    .iter()
                    .map(|(k, v)| Value::Tuple(Rc::new(vec![k.clone(), v.clone()])))
                    .collect();
                Ok(list_value(out))
            }
            "len" => {
                arg0(name, &args, span)?;
                Ok(Value::Int(BigInt::from(pairs.borrow().len())))
            }
            _ => Err(Diagnostic::runtime(
                format!("`Map` has no method `{}`", name),
                span,
            )),
        }
    }

    // ---- Set methods -------------------------------------------------------

    /// Dispatch a built-in `Set` method. Insertion-ordered `Vec`, deduplicated
    /// by structural equality via linear scan.
    fn set_method(
        &mut self,
        items: &Rc<RefCell<Vec<Value>>>,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> EvalResult {
        match name {
            "insert" => {
                let x = arg1(name, args, span)?;
                let mut set = items.borrow_mut();
                if !set.iter().any(|v| values_equal(v, &x)) {
                    set.push(x);
                }
                Ok(Value::Unit)
            }
            "contains" => {
                let x = arg1(name, args, span)?;
                let found = items.borrow().iter().any(|v| values_equal(v, &x));
                Ok(Value::Bool(found))
            }
            "union" => {
                let other = arg1(name, args, span)?;
                let other = as_set(&other, "union", span)?;
                let mut out = items.borrow().clone();
                for v in other.borrow().iter() {
                    if !out.iter().any(|u| values_equal(u, v)) {
                        out.push(v.clone());
                    }
                }
                Ok(set_value(out))
            }
            "intersect" => {
                let other = arg1(name, args, span)?;
                let other = as_set(&other, "intersect", span)?;
                let rhs = other.borrow();
                let out: Vec<Value> = items
                    .borrow()
                    .iter()
                    .filter(|v| rhs.iter().any(|u| values_equal(u, v)))
                    .cloned()
                    .collect();
                Ok(set_value(out))
            }
            "len" => {
                arg0(name, &args, span)?;
                Ok(Value::Int(BigInt::from(items.borrow().len())))
            }
            _ => Err(Diagnostic::runtime(
                format!("`Set` has no method `{}`", name),
                span,
            )),
        }
    }

    // ---- String methods (minimal) -----------------------------------------

    /// Dispatch a built-in `String` method. Minimal in M2 Wave 1: `len()`.
    fn str_method(s: &str, name: &str, args: &[Value], span: Span) -> EvalResult {
        match name {
            "len" => {
                arg0(name, args, span)?;
                // Length in Unicode scalar values (chars), not bytes.
                Ok(Value::Int(BigInt::from(s.chars().count())))
            }
            _ => Err(Diagnostic::runtime(
                format!("`String` has no method `{}`", name),
                span,
            )),
        }
    }

    /// Invoke a predicate callable and require a `Bool` result (used by
    /// `filter`/`any`/`all`/`find`). A non-`Bool` result is a runtime error —
    /// no truthiness, matching the language's condition rules.
    fn apply_predicate(&mut self, p: &Value, v: Value, span: Span) -> Result<bool, Diagnostic> {
        match self.apply(p.clone(), vec![v], span)? {
            Value::Bool(b) => Ok(b),
            other => Err(Diagnostic::runtime(
                format!("predicate must return Bool, found {}", type_name(&other)),
                span,
            )),
        }
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

    fn eval_match(&mut self, m: &MatchExpr, span: Span, env: &Env) -> EvalResult {
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
            return match self.exec_stmts(&arm.body.stmts, &arm_scope)? {
                Flow::Normal(v) => Ok(v),
                Flow::Return(v) => {
                    // A `return` inside a match arm propagates as a return.
                    // But `eval` cannot carry a Flow; the surrounding fn
                    // body handles return via statement evaluation. Since
                    // match is an expression, treat the arm's `return` as
                    // its value at expression level is wrong; instead we
                    // surface it. In practice arms end in an expression.
                    Ok(v)
                }
                Flow::Break | Flow::Continue => Err(Diagnostic::runtime(
                    "`break`/`continue` not allowed in a match arm".to_string(),
                    arm.span,
                )),
            };
        }
        Err(Diagnostic::runtime(
            "no match arm matched (non-exhaustive match)".to_string(),
            span,
        ))
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

/// Insert a key/value into an insertion-ordered map vector, deduplicating by
/// structural key equality (a re-inserted key overwrites its value in place,
/// preserving first-seen order). Mirrors the `Map` literal / comprehension rule.
fn map_insert(entries: &mut Vec<(Value, Value)>, key: Value, value: Value) {
    if let Some(slot) = entries.iter_mut().find(|(k, _)| values_equal(k, &key)) {
        slot.1 = value;
    } else {
        entries.push((key, value));
    }
}

/// Insert an element into an insertion-ordered set vector, deduplicating by
/// structural equality (a duplicate is dropped, keeping the first occurrence).
fn set_insert(elems: &mut Vec<Value>, value: Value) {
    if !elems.iter().any(|e| values_equal(e, &value)) {
        elems.push(value);
    }
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

/// The root (outermost) env of a chain — used so methods see top-level names.
fn root_of(env: &Env) -> Env {
    let parent = env.borrow().parent.clone();
    match parent {
        Some(p) => root_of(&p),
        None => Rc::clone(env),
    }
}

// ---- Built-in-method argument arity (M2 Wave 1-A) ------------------------

/// Require a built-in method to receive **no** arguments.
fn arg0(name: &str, args: &[Value], span: Span) -> Result<(), Diagnostic> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(Diagnostic::runtime(
            format!("`{}` takes no arguments, got {}", name, args.len()),
            span,
        ))
    }
}

/// Require **exactly one** argument, returning it by value.
fn arg1(name: &str, args: Vec<Value>, span: Span) -> Result<Value, Diagnostic> {
    if args.len() != 1 {
        return Err(Diagnostic::runtime(
            format!("`{}` takes exactly one argument, got {}", name, args.len()),
            span,
        ));
    }
    Ok(args.into_iter().next().unwrap())
}

/// Require **exactly two** arguments, returning them in order.
fn arg2(name: &str, args: Vec<Value>, span: Span) -> Result<(Value, Value), Diagnostic> {
    if args.len() != 2 {
        return Err(Diagnostic::runtime(
            format!("`{}` takes exactly two arguments, got {}", name, args.len()),
            span,
        ));
    }
    let mut it = args.into_iter();
    Ok((it.next().unwrap(), it.next().unwrap()))
}

// ---- Built-in-method value constructors / coercions ---------------------

/// Wrap a `Vec<Value>` as a fresh, reference-shared `Value::List`.
fn list_value(items: Vec<Value>) -> Value {
    Value::List(Rc::new(RefCell::new(items)))
}

/// Wrap a `Vec<Value>` as a fresh, reference-shared `Value::Set` (caller must
/// have already deduplicated).
fn set_value(items: Vec<Value>) -> Value {
    Value::Set(Rc::new(RefCell::new(items)))
}

/// Require a `List` receiver-arg, returning its shared store.
fn as_list<'a>(
    v: &'a Value,
    method: &str,
    span: Span,
) -> Result<&'a Rc<RefCell<Vec<Value>>>, Diagnostic> {
    match v {
        Value::List(items) => Ok(items),
        other => Err(Diagnostic::runtime(
            format!("`{}` expects a List argument, found {}", method, type_name(other)),
            span,
        )),
    }
}

/// Require a `Set` receiver-arg, returning its shared store.
fn as_set<'a>(
    v: &'a Value,
    method: &str,
    span: Span,
) -> Result<&'a Rc<RefCell<Vec<Value>>>, Diagnostic> {
    match v {
        Value::Set(items) => Ok(items),
        other => Err(Diagnostic::runtime(
            format!("`{}` expects a Set argument, found {}", method, type_name(other)),
            span,
        )),
    }
}

/// Coerce an `Int` count argument (e.g. for `take`/`skip`) to a `usize`. A
/// negative count is treated as zero (take/skip nothing).
fn as_count(v: &Value, method: &str, span: Span) -> Result<usize, Diagnostic> {
    match v {
        Value::Int(n) => Ok(n.to_usize().unwrap_or(if n.is_negative() { 0 } else { usize::MAX })),
        other => Err(Diagnostic::runtime(
            format!("`{}` expects an Int count, found {}", method, type_name(other)),
            span,
        )),
    }
}

// ---- Built-in-method numeric / ordering ---------------------------------

/// Sum a list of values. Empty sum is `Int(0)`; an all-`Float` (or mixed-empty)
/// list sums as `Float`. Mixing numeric kinds or summing a non-number is a
/// runtime error (no implicit Int/Float coercion).
fn sum_values(items: &[Value], span: Span) -> EvalResult {
    if items.is_empty() {
        return Ok(Value::Int(BigInt::from(0)));
    }
    match &items[0] {
        Value::Int(_) => {
            let mut acc = BigInt::from(0);
            for v in items {
                match v {
                    Value::Int(n) => acc += n,
                    other => {
                        return Err(Diagnostic::runtime(
                            format!("`sum` cannot add {} to an Int total", type_name(other)),
                            span,
                        ));
                    }
                }
            }
            Ok(Value::Int(acc))
        }
        Value::Float(_) => {
            let mut acc = 0.0_f64;
            for v in items {
                match v {
                    Value::Float(f) => acc += f,
                    other => {
                        return Err(Diagnostic::runtime(
                            format!("`sum` cannot add {} to a Float total", type_name(other)),
                            span,
                        ));
                    }
                }
            }
            Ok(Value::Float(acc))
        }
        other => Err(Diagnostic::runtime(
            format!("`sum` requires numbers, found {}", type_name(other)),
            span,
        )),
    }
}

/// Pick the extreme element by structural ordering: `Ordering::Less` for `min`,
/// `Ordering::Greater` for `max`. Errors on an empty list or incomparable
/// elements.
fn extreme(items: &[Value], want: Ordering, span: Span) -> EvalResult {
    let mut iter = items.iter();
    let label = if want == Ordering::Less { "min" } else { "max" };
    let mut best = iter
        .next()
        .ok_or_else(|| {
            Diagnostic::runtime(format!("`{}` on an empty list has no result", label), span)
        })?
        .clone();
    for v in iter {
        if compare_values(v, &best, span)? == want {
            best = v.clone();
        }
    }
    Ok(best)
}

/// Sort a slice of values ascending by structural ordering, surfacing the first
/// incomparable pair as a runtime error.
fn sort_values(items: &mut [Value], span: Span) -> Result<(), Diagnostic> {
    // `sort_by` can't carry a `Result`, so capture the first error out-of-band.
    let mut err: Option<Diagnostic> = None;
    items.sort_by(|a, b| match compare_values(a, b, span) {
        Ok(ord) => ord,
        Err(d) => {
            if err.is_none() {
                err = Some(d);
            }
            Ordering::Equal
        }
    });
    match err {
        Some(d) => Err(d),
        None => Ok(()),
    }
}

/// Structural ordering over comparable scalars (`Int`, `Float`, `String`,
/// `Bool`). Comparison is only defined within a single type; comparing across
/// types — or comparing a non-scalar (`List`/`Map`/`Set`/struct/…) — is a
/// runtime error (used by `sorted`/`min`/`max`).
fn compare_values(a: &Value, b: &Value, span: Span) -> Result<Ordering, Diagnostic> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).ok_or_else(|| {
            Diagnostic::runtime("cannot order NaN Float values".to_string(), span)
        }),
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        (x, y) => Err(Diagnostic::runtime(
            format!("cannot order {} against {}", type_name(x), type_name(y)),
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
fn type_name(v: &Value) -> &'static str {
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

/// Does a literal pattern match a runtime value (by value equality)?
fn lit_matches(lit: &LitPattern, val: &Value) -> bool {
    match (lit, val) {
        (LitPattern::Int(a), Value::Int(b)) => a == b,
        (LitPattern::Float(a), Value::Float(b)) => a == b,
        (LitPattern::Bool(a), Value::Bool(b)) => a == b,
        (LitPattern::Str(a), Value::Str(b)) => a == b,
        _ => false,
    }
}

/// Structural value equality — backs `==`/`!=` and `is`/`is not`.
///
/// `Float` uses IEEE `==` (so `NaN != NaN`). `Closure`/`Builtin` are never
/// equal. Lists/structs/enums recurse element-wise.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        (Value::Null, Value::Null) => true,
        (Value::List(x), Value::List(y)) => {
            let xs = x.borrow();
            let ys = y.borrow();
            xs.len() == ys.len()
                && xs.iter().zip(ys.iter()).all(|(p, q)| values_equal(p, q))
        }
        // Tuples: element-wise structural equality (correct and final).
        (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| values_equal(p, q))
        }
        // Maps are order-insensitive: equal iff every key in `x` is present in
        // `y` with a structurally-equal value (and the sizes match). Keys are
        // matched structurally via linear scan — no hashing, consistent with the
        // `Vec`-backed store.
        (Value::Map(x), Value::Map(y)) => {
            let xs = x.borrow();
            let ys = y.borrow();
            xs.len() == ys.len()
                && xs.iter().all(|(kx, vx)| {
                    ys.iter()
                        .find(|(ky, _)| values_equal(kx, ky))
                        .map_or(false, |(_, vy)| values_equal(vx, vy))
                })
        }
        // Sets are order-insensitive: equal iff same size and every element of
        // `x` appears in `y` (each side is already deduplicated, so this is a
        // mutual-containment check).
        (Value::Set(x), Value::Set(y)) => {
            let xs = x.borrow();
            let ys = y.borrow();
            xs.len() == ys.len()
                && xs.iter().all(|p| ys.iter().any(|q| values_equal(p, q)))
        }
        (Value::Struct(x), Value::Struct(y)) => {
            let xi = x.borrow();
            let yi = y.borrow();
            xi.type_name == yi.type_name
                && xi.fields.len() == yi.fields.len()
                && xi.fields.iter().all(|(k, v)| {
                    yi.fields.get(k).map_or(false, |w| values_equal(v, w))
                })
        }
        (Value::Enum(x), Value::Enum(y)) => {
            x.enum_name == y.enum_name
                && x.variant == y.variant
                && x.payload.len() == y.payload.len()
                && x.payload
                    .iter()
                    .zip(y.payload.iter())
                    .all(|(p, q)| values_equal(p, q))
        }
        // Cross-type and Closure/Builtin: never equal.
        _ => false,
    }
}

/// The default `Show` rendering, walking any value (no user code needed).
///
/// `Float` always shows a decimal point (`9.0`, not `9`) — required by the
/// showcase. Strings render as their text (no quotes), matching `print`.
fn show(v: &Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format_float(*f),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Str(s) => s.clone(),
        Value::Unit => "()".to_string(),
        Value::Null => "null".to_string(),
        Value::List(items) => {
            let inner: Vec<String> = items.borrow().iter().map(show).collect();
            format!("[{}]", inner.join(", "))
        }
        // Tuples render as `(a, b, …)` (final).
        Value::Tuple(items) => {
            let inner: Vec<String> = items.iter().map(show).collect();
            format!("({})", inner.join(", "))
        }
        // A `Map` renders `{k: v, k2: v2}`; an empty map is `{}` — matching the
        // surface literal (the empty `{}` is a `Map`).
        Value::Map(pairs) => {
            let inner: Vec<String> = pairs
                .borrow()
                .iter()
                .map(|(k, v)| format!("{}: {}", show(k), show(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        // A `Set` renders `{a, b}`; the *empty* set renders as `Set()` rather
        // than `{}` (which is the empty `Map`), matching the surface literal.
        Value::Set(items) => {
            let items = items.borrow();
            if items.is_empty() {
                "Set()".to_string()
            } else {
                let inner: Vec<String> = items.iter().map(show).collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
        Value::Struct(s) => {
            let inst = s.borrow();
            let parts: Vec<String> = inst
                .field_order
                .iter()
                .map(|f| {
                    let val = inst.fields.get(f).cloned().unwrap_or(Value::Unit);
                    format!("{}: {}", f, show(&val))
                })
                .collect();
            format!("{}({})", inst.type_name, parts.join(", "))
        }
        Value::Enum(e) => {
            if e.payload.is_empty() {
                e.variant.clone()
            } else if !e.payload_names.is_empty()
                && e.payload_names.len() == e.payload.len()
            {
                let parts: Vec<String> = e
                    .payload_names
                    .iter()
                    .zip(e.payload.iter())
                    .map(|(n, v)| format!("{}: {}", n, show(v)))
                    .collect();
                format!("{}({})", e.variant, parts.join(", "))
            } else {
                let parts: Vec<String> = e.payload.iter().map(show).collect();
                format!("{}({})", e.variant, parts.join(", "))
            }
        }
        Value::Closure(_) => "<function>".to_string(),
        Value::Builtin(_) => "<builtin>".to_string(),
    }
}

/// Render a float so it always carries a decimal point.
///
/// `9.0` → `"9.0"`, `2.5` → `"2.5"`, `1e30` → uses Rust's shortest round-trip
/// then ensures a `.0` if it came out integral.
fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    let s = format!("{}", f);
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{}.0", s)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;
    // `ast::Binding` collides with the runtime `super::Binding`; in tests we
    // only ever build the AST node, so import it explicitly to disambiguate.
    use crate::ast::Binding;
    use num_bigint::BigInt;

    // ---- tiny AST constructors -------------------------------------------

    fn sp() -> Span {
        Span::dummy()
    }

    fn ex(kind: ExprKind) -> Expr {
        Expr { kind, span: sp() }
    }

    fn int(n: i64) -> Expr {
        ex(ExprKind::Int(BigInt::from(n)))
    }

    fn float(f: f64) -> Expr {
        ex(ExprKind::Float(f))
    }

    fn boolean(b: bool) -> Expr {
        ex(ExprKind::Bool(b))
    }

    fn name(n: &str) -> Expr {
        ex(ExprKind::Name(n.to_string()))
    }

    fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
        ex(ExprKind::Binary { op, lhs: Box::new(l), rhs: Box::new(r) })
    }

    fn st(kind: StmtKind) -> Stmt {
        Stmt { kind, span: sp() }
    }

    fn expr_stmt(e: Expr) -> Stmt {
        st(StmtKind::Expr(e))
    }

    fn block(stmts: Vec<Stmt>) -> Block {
        Block { stmts, span: sp() }
    }

    /// Build a fresh interpreter + root env with the prelude seeded.
    fn fresh() -> (Interp, Env) {
        let interp = Interp { registry: Registry::default() };
        let root = Scope::new_root();
        seed_prelude(&root);
        (interp, root)
    }

    /// Evaluate a single expression in a fresh env.
    fn eval_expr(e: &Expr) -> EvalResult {
        let (mut interp, root) = fresh();
        interp.eval(e, &root)
    }

    /// Lex + parse + run a source program, then evaluate its **final**
    /// expression statement, returning that value. Earlier statements run for
    /// their effects (bindings, fn decls). Used by the M2 surface tests, which
    /// are far clearer as source than as hand-built AST.
    fn eval_src(src: &str) -> Value {
        let toks = crate::lexer::lex(src).expect("source should lex");
        let program = crate::parser::parse(&toks).expect("source should parse");
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let (last, init) = program
            .stmts
            .split_last()
            .expect("program should have at least one statement");
        for stmt in init {
            match interp.exec_stmt(stmt, &root) {
                Ok(_) => {}
                Err(d) => panic!("setup statement failed: {}", d.message),
            }
        }
        match &last.kind {
            StmtKind::Expr(e) => interp.eval(e, &root).expect("final expr should evaluate"),
            other => panic!("expected a trailing expr statement, got {:?}", other),
        }
    }

    /// Like [`eval_src`], but expect the **final** expression to fail at runtime;
    /// return the diagnostic. Earlier statements must still succeed.
    fn eval_src_err(src: &str) -> Diagnostic {
        let toks = crate::lexer::lex(src).expect("source should lex");
        let program = crate::parser::parse(&toks).expect("source should parse");
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let (last, init) = program
            .stmts
            .split_last()
            .expect("program should have at least one statement");
        for stmt in init {
            match interp.exec_stmt(stmt, &root) {
                Ok(_) => {}
                Err(d) => panic!("setup statement failed: {}", d.message),
            }
        }
        match &last.kind {
            StmtKind::Expr(e) => interp
                .eval(e, &root)
                .expect_err("final expr should fail at runtime"),
            other => panic!("expected a trailing expr statement, got {:?}", other),
        }
    }

    // ---- arithmetic -------------------------------------------------------

    #[test]
    fn int_arithmetic() {
        // 2 + 3 * 4 — precedence is encoded by the AST shape.
        let e = bin(BinOp::Add, int(2), bin(BinOp::Mul, int(3), int(4)));
        match eval_expr(&e).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(14)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    #[test]
    fn power_right_assoc_eval() {
        // 2 ** (3 ** 2) = 2 ** 9 = 512 — the AST already encodes right-assoc.
        let e = bin(BinOp::Pow, int(2), bin(BinOp::Pow, int(3), int(2)));
        match eval_expr(&e).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(512)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    #[test]
    fn float_true_division() {
        // 9.0 / 2.0 = 4.5
        let e = bin(BinOp::Div, float(9.0), float(2.0));
        match eval_expr(&e).unwrap() {
            Value::Float(f) => assert_eq!(f, 4.5),
            v => panic!("expected Float, got {:?}", v),
        }
    }

    #[test]
    fn int_division_exact_ok_inexact_errs() {
        // 6 / 3 = 2 (exact)
        let ok = bin(BinOp::Div, int(6), int(3));
        match eval_expr(&ok).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(2)),
            v => panic!("expected Int, got {:?}", v),
        }
        // 7 / 2 is not exact -> error.
        let bad = bin(BinOp::Div, int(7), int(2));
        assert!(eval_expr(&bad).is_err());
    }

    #[test]
    fn division_by_zero_errs() {
        let e = bin(BinOp::Div, int(1), int(0));
        assert!(eval_expr(&e).is_err());
    }

    // ---- short-circuit logic ---------------------------------------------

    #[test]
    fn and_short_circuits() {
        // false and <undefined name>  -> false, never touches rhs.
        let e = bin(BinOp::And, boolean(false), name("nope"));
        match eval_expr(&e).unwrap() {
            Value::Bool(b) => assert!(!b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    #[test]
    fn or_short_circuits() {
        // true or <undefined name>  -> true.
        let e = bin(BinOp::Or, boolean(true), name("nope"));
        match eval_expr(&e).unwrap() {
            Value::Bool(b) => assert!(b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    #[test]
    fn and_requires_bool() {
        // 1 and true -> error (non-Bool operand).
        let e = bin(BinOp::And, int(1), boolean(true));
        assert!(eval_expr(&e).is_err());
    }

    // ---- conditions must be Bool -----------------------------------------

    #[test]
    fn non_bool_condition_errs() {
        // if 1: print(...) else: ...  — condition is Int -> runtime error.
        let if_stmt = st(StmtKind::If(IfStmt {
            arms: vec![(int(1), block(vec![expr_stmt(int(0))]))],
            else_body: None,
        }));
        let (mut interp, root) = fresh();
        let r = interp.exec_stmt(&if_stmt, &root);
        assert!(r.is_err());
    }

    #[test]
    fn ternary_non_bool_errs() {
        // (1 if 5 else 2) — cond is Int.
        let e = ex(ExprKind::Ternary {
            then: Box::new(int(1)),
            cond: Box::new(int(5)),
            otherwise: Box::new(int(2)),
        });
        assert!(eval_expr(&e).is_err());
    }

    // ---- structural equality ---------------------------------------------

    #[test]
    fn structural_eq_on_lists() {
        let l1 = ex(ExprKind::List(vec![int(1), int(2), int(3)]));
        let l2 = ex(ExprKind::List(vec![int(1), int(2), int(3)]));
        let e = bin(BinOp::Eq, l1, l2);
        match eval_expr(&e).unwrap() {
            Value::Bool(b) => assert!(b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    #[test]
    fn structural_eq_on_structs() {
        // Build two Point structs by hand and compare with values_equal.
        let mut f1 = HashMap::new();
        f1.insert("x".to_string(), Value::Int(BigInt::from(1)));
        f1.insert("y".to_string(), Value::Int(BigInt::from(2)));
        let s1 = Value::Struct(Rc::new(RefCell::new(StructInstance {
            type_name: "Point".to_string(),
            fields: f1,
            field_order: vec!["x".to_string(), "y".to_string()],
        })));

        let mut f2 = HashMap::new();
        f2.insert("x".to_string(), Value::Int(BigInt::from(1)));
        f2.insert("y".to_string(), Value::Int(BigInt::from(2)));
        let s2 = Value::Struct(Rc::new(RefCell::new(StructInstance {
            type_name: "Point".to_string(),
            fields: f2,
            field_order: vec!["x".to_string(), "y".to_string()],
        })));

        assert!(values_equal(&s1, &s2));

        // Differ a field -> not equal.
        if let Value::Struct(s) = &s2 {
            s.borrow_mut()
                .fields
                .insert("y".to_string(), Value::Int(BigInt::from(99)));
        }
        assert!(!values_equal(&s1, &s2));
    }

    #[test]
    fn is_not_value_inequality() {
        let e = bin(BinOp::IsNot, int(1), int(2));
        match eval_expr(&e).unwrap() {
            Value::Bool(b) => assert!(b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    // ---- val reassignment -------------------------------------------------

    #[test]
    fn val_reassignment_errs() {
        // val x = 1 ; x = 2  -> runtime error on the reassignment.
        let bind = st(StmtKind::Binding(Binding {
            name: "x".to_string(),
            binder: Binder::Name("x".to_string()),
            is_val: true,
            ty: None,
            value: int(1),
        }));
        let reassign = st(StmtKind::Assign(Assign {
            target: Target { base: "x".to_string(), path: vec![], span: sp() },
            value: int(2),
        }));
        let (mut interp, root) = fresh();
        interp.exec_stmt(&bind, &root).unwrap();
        let r = interp.exec_stmt(&reassign, &root);
        assert!(r.is_err());
    }

    #[test]
    fn mutable_reassignment_ok() {
        let bind = st(StmtKind::Binding(Binding {
            name: "x".to_string(),
            binder: Binder::Name("x".to_string()),
            is_val: false,
            ty: None,
            value: int(1),
        }));
        let reassign = st(StmtKind::Assign(Assign {
            target: Target { base: "x".to_string(), path: vec![], span: sp() },
            value: int(2),
        }));
        let (mut interp, root) = fresh();
        interp.exec_stmt(&bind, &root).unwrap();
        interp.exec_stmt(&reassign, &root).unwrap();
        match env_get(&root, "x").unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(2)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    // ---- for loop ---------------------------------------------------------

    #[test]
    fn for_loop_accumulates_over_range() {
        // sum = 0 ; for x in 0..5: sum = sum + x   -> 0+1+2+3+4 = 10
        let init = st(StmtKind::Binding(Binding {
            name: "sum".to_string(),
            binder: Binder::Name("sum".to_string()),
            is_val: false,
            ty: None,
            value: int(0),
        }));
        let for_stmt = st(StmtKind::For(ForStmt {
            var: "x".to_string(),
            binder: Binder::Name("x".to_string()),
            iter: bin(BinOp::Range, int(0), int(5)),
            body: block(vec![st(StmtKind::Assign(Assign {
                target: Target { base: "sum".to_string(), path: vec![], span: sp() },
                value: bin(BinOp::Add, name("sum"), name("x")),
            }))]),
        }));
        let (mut interp, root) = fresh();
        interp.exec_stmt(&init, &root).unwrap();
        interp.exec_stmt(&for_stmt, &root).unwrap();
        match env_get(&root, "sum").unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(10)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    #[test]
    fn for_loop_inclusive_range() {
        // for x in 0..=3 -> [0,1,2,3], sum 6
        let init = st(StmtKind::Binding(Binding {
            name: "sum".to_string(),
            binder: Binder::Name("sum".to_string()),
            is_val: false,
            ty: None,
            value: int(0),
        }));
        let for_stmt = st(StmtKind::For(ForStmt {
            var: "x".to_string(),
            binder: Binder::Name("x".to_string()),
            iter: bin(BinOp::RangeIncl, int(0), int(3)),
            body: block(vec![st(StmtKind::Assign(Assign {
                target: Target { base: "sum".to_string(), path: vec![], span: sp() },
                value: bin(BinOp::Add, name("sum"), name("x")),
            }))]),
        }));
        let (mut interp, root) = fresh();
        interp.exec_stmt(&init, &root).unwrap();
        interp.exec_stmt(&for_stmt, &root).unwrap();
        match env_get(&root, "sum").unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(6)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    // ---- match over a hand-built enum ------------------------------------

    #[test]
    fn match_enum_variant_returns_arm() {
        // enum E: A  B(Int) ; match B(7): A: 0 ; B(n): n  -> 7
        let scrut = Value::Enum(Rc::new(EnumInstance {
            enum_name: "E".to_string(),
            variant: "B".to_string(),
            payload: vec![Value::Int(BigInt::from(7))],
            payload_names: vec![],
        }));

        let m = MatchExpr {
            scrutinee: Box::new(int(0)), // placeholder; we'll match a value directly
            arms: vec![
                MatchArm {
                    pattern: Pattern {
                        kind: PatternKind::Variant {
                            enum_name: None,
                            name: "A".to_string(),
                            subs: vec![],
                        },
                        span: sp(),
                    },
                    guard: None,
                    body: block(vec![expr_stmt(int(0))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: Pattern {
                        kind: PatternKind::Variant {
                            enum_name: None,
                            name: "B".to_string(),
                            subs: vec![Pattern {
                                kind: PatternKind::Binding("n".to_string()),
                                span: sp(),
                            }],
                        },
                        span: sp(),
                    },
                    guard: None,
                    body: block(vec![expr_stmt(name("n"))]),
                    span: sp(),
                },
            ],
        };

        // Drive eval_match by binding the scrutinee into a name and matching it.
        let (mut interp, root) = fresh();
        env_define(&root, "scrut", scrut, false);
        let m = MatchExpr { scrutinee: Box::new(name("scrut")), ..m };
        let v = interp.eval_match(&m, sp(), &root).unwrap();
        match v {
            Value::Int(n) => assert_eq!(n, BigInt::from(7)),
            v => panic!("expected Int(7), got {:?}", v),
        }
    }

    #[test]
    fn match_wildcard_fallback() {
        let scrut = Value::Int(BigInt::from(42));
        let m = MatchExpr {
            scrutinee: Box::new(name("scrut")),
            arms: vec![
                MatchArm {
                    pattern: Pattern {
                        kind: PatternKind::Literal(LitPattern::Int(BigInt::from(1))),
                        span: sp(),
                    },
                    guard: None,
                    body: block(vec![expr_stmt(int(100))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: Pattern { kind: PatternKind::Wildcard, span: sp() },
                    guard: None,
                    body: block(vec![expr_stmt(int(999))]),
                    span: sp(),
                },
            ],
        };
        let (mut interp, root) = fresh();
        env_define(&root, "scrut", scrut, false);
        match interp.eval_match(&m, sp(), &root).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(999)),
            v => panic!("expected Int(999), got {:?}", v),
        }
    }

    // ---- Show rendering ---------------------------------------------------

    #[test]
    fn show_float_has_decimal_point() {
        assert_eq!(show(&Value::Float(9.0)), "9.0");
        assert_eq!(show(&Value::Float(2.5)), "2.5");
        assert_eq!(show(&Value::Float(-1.0)), "-1.0");
    }

    #[test]
    fn show_int_and_bool_and_null() {
        assert_eq!(show(&Value::Int(BigInt::from(42))), "42");
        assert_eq!(show(&Value::Bool(true)), "true");
        assert_eq!(show(&Value::Bool(false)), "false");
        assert_eq!(show(&Value::Null), "null");
        assert_eq!(show(&Value::Unit), "()");
    }

    #[test]
    fn show_struct() {
        let mut fields = HashMap::new();
        fields.insert("x".to_string(), Value::Int(BigInt::from(1)));
        fields.insert("y".to_string(), Value::Float(2.0));
        let s = Value::Struct(Rc::new(RefCell::new(StructInstance {
            type_name: "Point".to_string(),
            fields,
            field_order: vec!["x".to_string(), "y".to_string()],
        })));
        assert_eq!(show(&s), "Point(x: 1, y: 2.0)");
    }

    #[test]
    fn show_enum() {
        let e = Value::Enum(Rc::new(EnumInstance {
            enum_name: "Expr".to_string(),
            variant: "Num".to_string(),
            payload: vec![Value::Float(1.0)],
            payload_names: vec![],
        }));
        assert_eq!(show(&e), "Num(1.0)");

        let empty = Value::Enum(Rc::new(EnumInstance {
            enum_name: "E".to_string(),
            variant: "Empty".to_string(),
            payload: vec![],
            payload_names: vec![],
        }));
        assert_eq!(show(&empty), "Empty");
    }

    #[test]
    fn show_list() {
        let l = Value::List(Rc::new(RefCell::new(vec![
            Value::Int(BigInt::from(1)),
            Value::Int(BigInt::from(2)),
        ])));
        assert_eq!(show(&l), "[1, 2]");
    }

    // ---- panic ------------------------------------------------------------

    #[test]
    fn panic_produces_err() {
        let (mut interp, root) = fresh();
        let panic_call = ex(ExprKind::Call {
            callee: Box::new(name("panic")),
            args: vec![Arg::Positional(ex(ExprKind::Str(StringLit {
                parts: vec![StrSeg::Text("boom".to_string())],
            })))],
        });
        let r = interp.eval(&panic_call, &root);
        assert!(r.is_err());
        let d = r.unwrap_err();
        assert!(d.message.contains("boom"));
    }

    // ---- calling a hand-built main + functions ---------------------------

    #[test]
    fn run_program_calls_main() {
        // fn main(): x = 1 ; (no assertion on stdout, just that it runs clean)
        let main = FnDecl {
            name: "main".to_string(),
            params: vec![],
            returns: None,
            body: block(vec![st(StmtKind::Binding(Binding {
                name: "x".to_string(),
                binder: Binder::Name("x".to_string()),
                is_val: false,
                ty: None,
                value: int(1),
            }))]),
            doc: None,
            span: sp(),
        };
        let program = Program { stmts: vec![st(StmtKind::Fn(main))] };
        assert!(run(&program).is_ok());
    }

    #[test]
    fn function_call_and_implicit_return() {
        // fn double(n: Int) returns Int: n + n
        // double(21) -> 42
        let double = FnDecl {
            name: "double".to_string(),
            params: vec![Param::Named {
                name: "n".to_string(),
                ty: Type {
                    base: BaseType::Named { name: "Int".to_string(), args: vec![] },
                    nullable: false,
                    span: sp(),
                },
                default: None,
            }],
            returns: None,
            body: block(vec![expr_stmt(bin(BinOp::Add, name("n"), name("n")))]),
            doc: None,
            span: sp(),
        };
        let (mut interp, root) = fresh();
        interp.exec_stmt(&st(StmtKind::Fn(double)), &root).unwrap();
        let call = ex(ExprKind::Call {
            callee: Box::new(name("double")),
            args: vec![Arg::Positional(int(21))],
        });
        match interp.eval(&call, &root).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(42)),
            v => panic!("expected Int(42), got {:?}", v),
        }
    }

    #[test]
    fn or_else_defaults_null() {
        // null.or_else(5) -> 5 ; 3.or_else(5) -> 3
        let (mut interp, root) = fresh();
        let e1 = ex(ExprKind::Call {
            callee: Box::new(ex(ExprKind::Member {
                base: Box::new(ex(ExprKind::Null)),
                name: "or_else".to_string(),
                safe: false,
            })),
            args: vec![Arg::Positional(int(5))],
        });
        match interp.eval(&e1, &root).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("expected Int(5), got {:?}", v),
        }
        let e2 = ex(ExprKind::Call {
            callee: Box::new(ex(ExprKind::Member {
                base: Box::new(int(3)),
                name: "or_else".to_string(),
                safe: false,
            })),
            args: vec![Arg::Positional(int(5))],
        });
        match interp.eval(&e2, &root).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("expected Int(3), got {:?}", v),
        }
    }

    // ---- the MVP showcase, built by hand ---------------------------------

    /// Reconstructs the §13 evaluator from `02-mvp-scope.md` as a hand-built
    /// `Program` and confirms `eval((1+2)*3)` is `9.0` and `Show`s as `"9.0"`.
    ///
    /// This is the Milestone-1 definition-of-done for the interpreter: enums
    /// with data, recursion, an exhaustive `match` (with a `panic` guard arm),
    /// `fn` signatures, `val`-style bindings, and float arithmetic together.
    #[test]
    fn showcase_evaluator_yields_9_0() {
        // Helpers to spell out the AST tersely.
        fn ty_float() -> Type {
            Type {
                base: BaseType::Named { name: "Float".to_string(), args: vec![] },
                nullable: false,
                span: sp(),
            }
        }
        fn variant_pat(name: &str, subs: Vec<&str>) -> Pattern {
            Pattern {
                kind: PatternKind::Variant {
                    enum_name: None,
                    name: name.to_string(),
                    subs: subs
                        .into_iter()
                        .map(|s| Pattern {
                            kind: PatternKind::Binding(s.to_string()),
                            span: sp(),
                        })
                        .collect(),
                },
                span: sp(),
            }
        }
        fn call(callee: Expr, args: Vec<Expr>) -> Expr {
            ex(ExprKind::Call {
                callee: Box::new(callee),
                args: args.into_iter().map(Arg::Positional).collect(),
            })
        }

        // enum Expr: Num(Float)  Add(Expr,Expr)  Mul(Expr,Expr)  Div(Expr,Expr)
        let expr_enum = EnumDecl {
            name: "Expr".to_string(),
            variants: vec![
                VariantDecl {
                    name: "Num".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float()])),
                    doc: None,
                    span: sp(),
                },
                VariantDecl {
                    name: "Add".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float(), ty_float()])),
                    doc: None,
                    span: sp(),
                },
                VariantDecl {
                    name: "Mul".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float(), ty_float()])),
                    doc: None,
                    span: sp(),
                },
                VariantDecl {
                    name: "Div".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float(), ty_float()])),
                    doc: None,
                    span: sp(),
                },
            ],
            doc: None,
            span: sp(),
        };

        // fn eval(e: Expr) returns Float:
        //     return match e:
        //         Num(n):    n
        //         Add(a, b): eval(a) + eval(b)
        //         Mul(a, b): eval(a) * eval(b)
        //         Div(a, b):
        //             divisor = eval(b)
        //             if divisor == 0.0: panic("division by zero")
        //             eval(a) / divisor
        let match_expr = ex(ExprKind::Match(MatchExpr {
            scrutinee: Box::new(name("e")),
            arms: vec![
                MatchArm {
                    pattern: variant_pat("Num", vec!["n"]),
                    guard: None,
                    body: block(vec![expr_stmt(name("n"))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Add", vec!["a", "b"]),
                    guard: None,
                    body: block(vec![expr_stmt(bin(
                        BinOp::Add,
                        call(name("eval"), vec![name("a")]),
                        call(name("eval"), vec![name("b")]),
                    ))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Mul", vec!["a", "b"]),
                    guard: None,
                    body: block(vec![expr_stmt(bin(
                        BinOp::Mul,
                        call(name("eval"), vec![name("a")]),
                        call(name("eval"), vec![name("b")]),
                    ))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Div", vec!["a", "b"]),
                    guard: None,
                    body: block(vec![
                        st(StmtKind::Binding(Binding {
                            name: "divisor".to_string(),
                            binder: Binder::Name("divisor".to_string()),
                            is_val: false,
                            ty: None,
                            value: call(name("eval"), vec![name("b")]),
                        })),
                        st(StmtKind::If(IfStmt {
                            arms: vec![(
                                bin(BinOp::Eq, name("divisor"), float(0.0)),
                                block(vec![expr_stmt(call(
                                    name("panic"),
                                    vec![ex(ExprKind::Str(StringLit {
                                        parts: vec![StrSeg::Text(
                                            "division by zero".to_string(),
                                        )],
                                    }))],
                                ))]),
                            )],
                            else_body: None,
                        })),
                        expr_stmt(bin(
                            BinOp::Div,
                            call(name("eval"), vec![name("a")]),
                            name("divisor"),
                        )),
                    ]),
                    span: sp(),
                },
            ],
        }));

        let eval_fn = FnDecl {
            name: "eval".to_string(),
            params: vec![Param::Named {
                name: "e".to_string(),
                ty: Type {
                    base: BaseType::Named { name: "Expr".to_string(), args: vec![] },
                    nullable: false,
                    span: sp(),
                },
                default: None,
            }],
            returns: Some(ty_float()),
            body: block(vec![st(StmtKind::Return(Some(match_expr)))]),
            doc: None,
            span: sp(),
        };

        // Build it and the program in a fresh interpreter.
        let program = Program {
            stmts: vec![st(StmtKind::Enum(expr_enum)), st(StmtKind::Fn(eval_fn))],
        };

        let mut interp = Interp { registry: Registry::default() };
        let root = Scope::new_root();
        seed_prelude(&root);
        interp.collect_decls(&program);
        for stmt in &program.stmts {
            interp.exec_stmt(stmt, &root).unwrap();
        }

        // program = Expr.Mul(Expr.Add(Expr.Num(1.0), Expr.Num(2.0)), Expr.Num(3.0))
        let vc = |v: &str, args: Vec<Expr>| {
            call(
                ex(ExprKind::Member { base: Box::new(name("Expr")), name: v.to_string(), safe: false }),
                args,
            )
        };
        let num = |f: f64| vc("Num", vec![float(f)]);
        let prog_expr = vc(
            "Mul",
            vec![vc("Add", vec![num(1.0), num(2.0)]), num(3.0)],
        );
        let call_eval = call(name("eval"), vec![prog_expr]);

        let result = interp.eval(&call_eval, &root).unwrap();
        match &result {
            Value::Float(f) => assert_eq!(*f, 9.0),
            v => panic!("expected Float(9.0), got {:?}", v),
        }
        // The critical formatting requirement from the spec.
        assert_eq!(show(&result), "9.0");
    }

    #[test]
    fn showcase_division_by_zero_panics() {
        // Reuse a minimal version: eval(Div(Num(1.0), Num(0.0))) must Err.
        // Build a tiny enum + eval fn covering just Num and Div.
        fn ty_float() -> Type {
            Type {
                base: BaseType::Named { name: "Float".to_string(), args: vec![] },
                nullable: false,
                span: sp(),
            }
        }
        fn call(callee: Expr, args: Vec<Expr>) -> Expr {
            ex(ExprKind::Call {
                callee: Box::new(callee),
                args: args.into_iter().map(Arg::Positional).collect(),
            })
        }
        fn variant_pat(name: &str, subs: Vec<&str>) -> Pattern {
            Pattern {
                kind: PatternKind::Variant {
                    enum_name: None,
                    name: name.to_string(),
                    subs: subs
                        .into_iter()
                        .map(|s| Pattern {
                            kind: PatternKind::Binding(s.to_string()),
                            span: sp(),
                        })
                        .collect(),
                },
                span: sp(),
            }
        }

        let expr_enum = EnumDecl {
            name: "E".to_string(),
            variants: vec![
                VariantDecl {
                    name: "Num".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float()])),
                    doc: None,
                    span: sp(),
                },
                VariantDecl {
                    name: "Div".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float(), ty_float()])),
                    doc: None,
                    span: sp(),
                },
            ],
            doc: None,
            span: sp(),
        };

        let match_expr = ex(ExprKind::Match(MatchExpr {
            scrutinee: Box::new(name("e")),
            arms: vec![
                MatchArm {
                    pattern: variant_pat("Num", vec!["n"]),
                    guard: None,
                    body: block(vec![expr_stmt(name("n"))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Div", vec!["a", "b"]),
                    guard: None,
                    body: block(vec![
                        st(StmtKind::Binding(Binding {
                            name: "divisor".to_string(),
                            binder: Binder::Name("divisor".to_string()),
                            is_val: false,
                            ty: None,
                            value: call(name("eval"), vec![name("b")]),
                        })),
                        st(StmtKind::If(IfStmt {
                            arms: vec![(
                                bin(BinOp::Eq, name("divisor"), float(0.0)),
                                block(vec![expr_stmt(call(
                                    name("panic"),
                                    vec![ex(ExprKind::Str(StringLit {
                                        parts: vec![StrSeg::Text("division by zero".to_string())],
                                    }))],
                                ))]),
                            )],
                            else_body: None,
                        })),
                        expr_stmt(bin(
                            BinOp::Div,
                            call(name("eval"), vec![name("a")]),
                            name("divisor"),
                        )),
                    ]),
                    span: sp(),
                },
            ],
        }));

        let eval_fn = FnDecl {
            name: "eval".to_string(),
            params: vec![Param::Named {
                name: "e".to_string(),
                ty: Type {
                    base: BaseType::Named { name: "E".to_string(), args: vec![] },
                    nullable: false,
                    span: sp(),
                },
                default: None,
            }],
            returns: Some(ty_float()),
            body: block(vec![st(StmtKind::Return(Some(match_expr)))]),
            doc: None,
            span: sp(),
        };

        let program = Program {
            stmts: vec![st(StmtKind::Enum(expr_enum)), st(StmtKind::Fn(eval_fn))],
        };
        let mut interp = Interp { registry: Registry::default() };
        let root = Scope::new_root();
        seed_prelude(&root);
        interp.collect_decls(&program);
        for stmt in &program.stmts {
            interp.exec_stmt(stmt, &root).unwrap();
        }

        let vc = |v: &str, args: Vec<Expr>| {
            call(
                ex(ExprKind::Member { base: Box::new(name("E")), name: v.to_string(), safe: false }),
                args,
            )
        };
        let num = |f: f64| vc("Num", vec![float(f)]);
        let div_zero = call(name("eval"), vec![vc("Div", vec![num(1.0), num(0.0)])]);
        let r = interp.eval(&div_zero, &root);
        assert!(r.is_err());
        assert!(r.unwrap_err().message.contains("division by zero"));
    }

    // =======================================================================
    // M2 Wave 1-A — built-in method dispatch (List / Map / Set / String)
    // =======================================================================

    /// Build a `Value::List` from raw `Int`s.
    fn int_list(ns: &[i64]) -> Value {
        list_value(ns.iter().map(|&n| Value::Int(BigInt::from(n))).collect())
    }

    /// Extract the `Int`s out of a `Value::List`, panicking on any other shape.
    fn list_ints(v: &Value) -> Vec<i64> {
        match v {
            Value::List(items) => items
                .borrow()
                .iter()
                .map(|x| match x {
                    Value::Int(n) => n.to_i64().unwrap(),
                    other => panic!("expected Int element, got {:?}", other),
                })
                .collect(),
            other => panic!("expected List, got {:?}", other),
        }
    }

    /// A passable callable `Value` built from a single-param lambda `p -> body`.
    fn lambda1(interp: &mut Interp, root: &Env, p: &str, body: Expr) -> Value {
        let lam = ex(ExprKind::Lambda(Lambda {
            params: vec![p.to_string()],
            body: Box::new(body),
        }));
        interp.eval(&lam, root).unwrap()
    }

    /// Call a built-in method on `recv` with already-evaluated argument values
    /// (wrapped as trivial positional arg expressions is unnecessary — we go
    /// straight through the per-type dispatchers).
    fn list_call(interp: &mut Interp, recv: &Value, name: &str, args: Vec<Value>) -> Value {
        let items = match recv {
            Value::List(items) => items.clone(),
            other => panic!("list_call on non-list {:?}", other),
        };
        interp.list_method(&items, name, args, sp()).unwrap()
    }

    #[test]
    fn list_map_filter_sum_pipeline() {
        let (mut interp, root) = fresh();
        let xs = int_list(&[1, 2, 3, 4, 5, 6]);
        // filter(n -> n % 2 == 0)
        let even = lambda1(
            &mut interp,
            &root,
            "n",
            bin(BinOp::Eq, bin(BinOp::Rem, name("n"), int(2)), int(0)),
        );
        let filtered = list_call(&mut interp, &xs, "filter", vec![even]);
        assert_eq!(list_ints(&filtered), vec![2, 4, 6]);
        // map(n -> n * n)
        let square = lambda1(&mut interp, &root, "n", bin(BinOp::Mul, name("n"), name("n")));
        let mapped = list_call(&mut interp, &filtered, "map", vec![square]);
        assert_eq!(list_ints(&mapped), vec![4, 16, 36]);
        // sum() -> 56
        match list_call(&mut interp, &mapped, "sum", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(56)),
            v => panic!("expected Int(56), got {:?}", v),
        }
    }

    #[test]
    fn list_fold_and_reduce() {
        let (mut interp, root) = fresh();
        let xs = int_list(&[1, 2, 3, 4]);
        let add = lambda1_2(&mut interp, &root, "a", "b", bin(BinOp::Add, name("a"), name("b")));
        match list_call(&mut interp, &xs, "fold", vec![Value::Int(BigInt::from(100)), add.clone()]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(110)),
            v => panic!("expected Int(110), got {:?}", v),
        }
        match list_call(&mut interp, &xs, "reduce", vec![add]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(10)),
            v => panic!("expected Int(10), got {:?}", v),
        }
        // reduce on empty is an error.
        let empty = int_list(&[]);
        let add2 = lambda1_2(&mut interp, &root, "a", "b", bin(BinOp::Add, name("a"), name("b")));
        let items = match &empty {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        assert!(interp.list_method(&items, "reduce", vec![add2], sp()).is_err());
    }

    /// A passable callable from a two-param lambda `(a, b) -> body`.
    fn lambda1_2(interp: &mut Interp, root: &Env, a: &str, b: &str, body: Expr) -> Value {
        let lam = ex(ExprKind::Lambda(Lambda {
            params: vec![a.to_string(), b.to_string()],
            body: Box::new(body),
        }));
        interp.eval(&lam, root).unwrap()
    }

    #[test]
    fn list_predicates_and_search() {
        let (mut interp, root) = fresh();
        let xs = int_list(&[1, 2, 3, 4, 5]);
        let gt3 = |interp: &mut Interp, root: &Env| {
            lambda1(interp, root, "n", bin(BinOp::Gt, name("n"), int(3)))
        };
        let p = gt3(&mut interp, &root);
        assert!(matches!(list_call(&mut interp, &xs, "any", vec![p]), Value::Bool(true)));
        let p = gt3(&mut interp, &root);
        assert!(matches!(list_call(&mut interp, &xs, "all", vec![p]), Value::Bool(false)));
        let p = gt3(&mut interp, &root);
        match list_call(&mut interp, &xs, "find", vec![p]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(4)),
            v => panic!("expected Int(4), got {:?}", v),
        }
        // find with no match -> Null
        let none = lambda1(&mut interp, &root, "n", bin(BinOp::Gt, name("n"), int(99)));
        assert!(matches!(list_call(&mut interp, &xs, "find", vec![none]), Value::Null));
        // contains
        assert!(matches!(
            list_call(&mut interp, &xs, "contains", vec![Value::Int(BigInt::from(3))]),
            Value::Bool(true)
        ));
        assert!(matches!(
            list_call(&mut interp, &xs, "contains", vec![Value::Int(BigInt::from(9))]),
            Value::Bool(false)
        ));
        // a non-Bool predicate is a runtime error (no truthiness).
        let bad = lambda1(&mut interp, &root, "n", name("n"));
        let items = match &xs {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        assert!(interp.list_method(&items, "filter", vec![bad], sp()).is_err());
    }

    #[test]
    fn list_size_first_last_min_max() {
        let (mut interp, _root) = fresh();
        let xs = int_list(&[3, 1, 4, 1, 5]);
        match list_call(&mut interp, &xs, "count", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("expected Int, got {:?}", v),
        }
        match list_call(&mut interp, &xs, "len", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("expected Int, got {:?}", v),
        }
        assert!(matches!(list_call(&mut interp, &xs, "is_empty", vec![]), Value::Bool(false)));
        match list_call(&mut interp, &xs, "first", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("got {:?}", v),
        }
        match list_call(&mut interp, &xs, "last", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("got {:?}", v),
        }
        match list_call(&mut interp, &xs, "min", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(1)),
            v => panic!("got {:?}", v),
        }
        match list_call(&mut interp, &xs, "max", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("got {:?}", v),
        }
        // first/last on empty -> Null
        let empty = int_list(&[]);
        assert!(matches!(list_call(&mut interp, &empty, "first", vec![]), Value::Null));
        assert!(matches!(list_call(&mut interp, &empty, "last", vec![]), Value::Null));
        assert!(matches!(list_call(&mut interp, &empty, "is_empty", vec![]), Value::Bool(true)));
    }

    #[test]
    fn list_take_skip_reverse_sorted_collect() {
        let (mut interp, _root) = fresh();
        let xs = int_list(&[1, 2, 3, 4, 5]);
        assert_eq!(
            list_ints(&list_call(&mut interp, &xs, "take", vec![Value::Int(BigInt::from(2))])),
            vec![1, 2]
        );
        assert_eq!(
            list_ints(&list_call(&mut interp, &xs, "skip", vec![Value::Int(BigInt::from(3))])),
            vec![4, 5]
        );
        // take/skip beyond length, and a negative count clamps to 0.
        assert_eq!(
            list_ints(&list_call(&mut interp, &xs, "take", vec![Value::Int(BigInt::from(99))])),
            vec![1, 2, 3, 4, 5]
        );
        assert!(
            list_ints(&list_call(&mut interp, &xs, "skip", vec![Value::Int(BigInt::from(-1))]))
                .len()
                == 5
        );
        assert_eq!(
            list_ints(&list_call(&mut interp, &xs, "reverse", vec![])),
            vec![5, 4, 3, 2, 1]
        );
        let unsorted = int_list(&[3, 1, 4, 1, 5, 9, 2, 6]);
        assert_eq!(
            list_ints(&list_call(&mut interp, &unsorted, "sorted", vec![])),
            vec![1, 1, 2, 3, 4, 5, 6, 9]
        );
        assert_eq!(list_ints(&list_call(&mut interp, &xs, "collect", vec![])), vec![1, 2, 3, 4, 5]);
        // sorted over incomparable (mixed Int/String) is an error.
        let mixed = list_value(vec![Value::Int(BigInt::from(1)), Value::Str("a".to_string())]);
        let items = match &mixed {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        assert!(interp.list_method(&items, "sorted", vec![], sp()).is_err());
    }

    #[test]
    fn list_enumerate_and_zip_make_tuples() {
        let (mut interp, _root) = fresh();
        let xs = list_value(vec![Value::Str("a".to_string()), Value::Str("b".to_string())]);
        let en = list_call(&mut interp, &xs, "enumerate", vec![]);
        match &en {
            Value::List(items) => {
                let items = items.borrow();
                assert_eq!(items.len(), 2);
                match &items[0] {
                    Value::Tuple(t) => {
                        assert!(matches!(&t[0], Value::Int(n) if *n == BigInt::from(0)));
                        assert!(matches!(&t[1], Value::Str(s) if s == "a"));
                    }
                    v => panic!("expected tuple, got {:?}", v),
                }
            }
            v => panic!("expected list, got {:?}", v),
        }
        // zip pairs up to the shorter length.
        let ns = int_list(&[10, 20, 30]);
        let zipped = list_call(&mut interp, &ns, "zip", vec![xs]);
        match &zipped {
            Value::List(items) => assert_eq!(items.borrow().len(), 2),
            v => panic!("expected list, got {:?}", v),
        }
    }

    #[test]
    fn list_append_and_pop_last_mutate() {
        let (mut interp, _root) = fresh();
        let xs = int_list(&[1, 2]);
        list_call(&mut interp, &xs, "append", vec![Value::Int(BigInt::from(3))]);
        assert_eq!(list_ints(&xs), vec![1, 2, 3]); // mutated in place
        match list_call(&mut interp, &xs, "pop_last", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("got {:?}", v),
        }
        assert_eq!(list_ints(&xs), vec![1, 2]);
        // pop_last on empty -> Null
        let empty = int_list(&[]);
        assert!(matches!(list_call(&mut interp, &empty, "pop_last", vec![]), Value::Null));
    }

    #[test]
    fn sum_int_float_and_mixed_error() {
        let (mut interp, _root) = fresh();
        // empty sum is Int(0)
        match list_call(&mut interp, &int_list(&[]), "sum", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(0)),
            v => panic!("got {:?}", v),
        }
        let floats = list_value(vec![Value::Float(1.5), Value::Float(2.0)]);
        match list_call(&mut interp, &floats, "sum", vec![]) {
            Value::Float(f) => assert_eq!(f, 3.5),
            v => panic!("got {:?}", v),
        }
        // mixing Int and Float is a runtime error (no coercion).
        let mixed = list_value(vec![Value::Int(BigInt::from(1)), Value::Float(2.0)]);
        let items = match &mixed {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        assert!(interp.list_method(&items, "sum", vec![], sp()).is_err());
    }

    // ---- Map --------------------------------------------------------------

    fn map_value(pairs: Vec<(Value, Value)>) -> Value {
        Value::Map(Rc::new(RefCell::new(pairs)))
    }

    fn map_call(interp: &mut Interp, recv: &Value, name: &str, args: Vec<Value>) -> Value {
        let pairs = match recv {
            Value::Map(p) => p.clone(),
            other => panic!("map_call on non-map {:?}", other),
        };
        interp.map_method(&pairs, name, args, sp()).unwrap()
    }

    #[test]
    fn map_get_insert_contains_len() {
        let (mut interp, _root) = fresh();
        let m = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
        ]);
        match map_call(&mut interp, &m, "get", vec![Value::Str("a".to_string())]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(1)),
            v => panic!("got {:?}", v),
        }
        // absent key -> Null
        assert!(matches!(
            map_call(&mut interp, &m, "get", vec![Value::Str("z".to_string())]),
            Value::Null
        ));
        // has / contains
        assert!(matches!(
            map_call(&mut interp, &m, "has", vec![Value::Str("b".to_string())]),
            Value::Bool(true)
        ));
        assert!(matches!(
            map_call(&mut interp, &m, "contains", vec![Value::Str("z".to_string())]),
            Value::Bool(false)
        ));
        // insert new key, then overwrite existing (in place; preserves order)
        map_call(&mut interp, &m, "insert", vec![Value::Str("c".to_string()), Value::Int(BigInt::from(3))]);
        map_call(&mut interp, &m, "insert", vec![Value::Str("a".to_string()), Value::Int(BigInt::from(9))]);
        match map_call(&mut interp, &m, "get", vec![Value::Str("a".to_string())]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(9)),
            v => panic!("got {:?}", v),
        }
        match map_call(&mut interp, &m, "len", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("got {:?}", v),
        }
    }

    #[test]
    fn map_keys_values_items() {
        let (mut interp, _root) = fresh();
        let m = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
        ]);
        // keys() preserves insertion order
        match map_call(&mut interp, &m, "keys", vec![]) {
            Value::List(items) => {
                let ks: Vec<String> = items
                    .borrow()
                    .iter()
                    .map(|k| match k {
                        Value::Str(s) => s.clone(),
                        v => panic!("got {:?}", v),
                    })
                    .collect();
                assert_eq!(ks, vec!["a", "b"]);
            }
            v => panic!("got {:?}", v),
        }
        assert_eq!(list_ints(&map_call(&mut interp, &m, "values", vec![])), vec![1, 2]);
        // items() yields 2-tuples
        match map_call(&mut interp, &m, "items", vec![]) {
            Value::List(items) => {
                let items = items.borrow();
                assert_eq!(items.len(), 2);
                match &items[0] {
                    Value::Tuple(t) => {
                        assert_eq!(t.len(), 2);
                        assert!(matches!(&t[0], Value::Str(s) if s == "a"));
                        assert!(matches!(&t[1], Value::Int(n) if *n == BigInt::from(1)));
                    }
                    v => panic!("got {:?}", v),
                }
            }
            v => panic!("got {:?}", v),
        }
    }

    // ---- Set --------------------------------------------------------------

    fn set_call(interp: &mut Interp, recv: &Value, name: &str, args: Vec<Value>) -> Value {
        let items = match recv {
            Value::Set(i) => i.clone(),
            other => panic!("set_call on non-set {:?}", other),
        };
        interp.set_method(&items, name, args, sp()).unwrap()
    }

    fn set_of(ns: &[i64]) -> Value {
        set_value(ns.iter().map(|&n| Value::Int(BigInt::from(n))).collect())
    }

    #[test]
    fn set_insert_dedup_contains_len() {
        let (mut interp, _root) = fresh();
        let s = set_of(&[1, 2]);
        set_call(&mut interp, &s, "insert", vec![Value::Int(BigInt::from(2))]); // dup, no-op
        set_call(&mut interp, &s, "insert", vec![Value::Int(BigInt::from(3))]);
        match set_call(&mut interp, &s, "len", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("got {:?}", v),
        }
        assert!(matches!(
            set_call(&mut interp, &s, "contains", vec![Value::Int(BigInt::from(3))]),
            Value::Bool(true)
        ));
        assert!(matches!(
            set_call(&mut interp, &s, "contains", vec![Value::Int(BigInt::from(9))]),
            Value::Bool(false)
        ));
    }

    #[test]
    fn set_union_and_intersect() {
        let (mut interp, _root) = fresh();
        let a = set_of(&[1, 2, 3]);
        let b = set_of(&[2, 3, 4]);
        let u = set_call(&mut interp, &a, "union", vec![b.clone()]);
        // union keeps insertion order of `a` then new elements of `b`
        match &u {
            Value::Set(items) => {
                let got: Vec<i64> = items
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Int(n) => n.to_i64().unwrap(),
                        x => panic!("got {:?}", x),
                    })
                    .collect();
                assert_eq!(got, vec![1, 2, 3, 4]);
            }
            v => panic!("got {:?}", v),
        }
        let i = set_call(&mut interp, &a, "intersect", vec![b]);
        match &i {
            Value::Set(items) => {
                let got: Vec<i64> = items
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Int(n) => n.to_i64().unwrap(),
                        x => panic!("got {:?}", x),
                    })
                    .collect();
                assert_eq!(got, vec![2, 3]);
            }
            v => panic!("got {:?}", v),
        }
    }

    // ---- String, equality, show, named-arg rejection ----------------------

    #[test]
    fn string_len() {
        // counts Unicode scalar values, not bytes (`é` is one char, two bytes).
        match Interp::str_method("héllo", "len", &[], sp()).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("expected Int(5), got {:?}", v),
        }
    }

    #[test]
    fn map_set_equality_is_order_insensitive() {
        let m1 = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
        ]);
        let m2 = map_value(vec![
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
        ]);
        assert!(values_equal(&m1, &m2));
        // differing value breaks equality
        let m3 = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(9))),
        ]);
        assert!(!values_equal(&m1, &m3));

        let s1 = set_of(&[1, 2, 3]);
        let s2 = set_of(&[3, 2, 1]);
        assert!(values_equal(&s1, &s2));
        assert!(!values_equal(&s1, &set_of(&[1, 2])));
    }

    #[test]
    fn show_map_and_set() {
        let m = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
        ]);
        assert_eq!(show(&m), "{a: 1, b: 2}");
        // empty map is `{}`
        assert_eq!(show(&map_value(vec![])), "{}");
        // set renders `{a, b}`; empty set renders `Set()`
        assert_eq!(show(&set_of(&[1, 2])), "{1, 2}");
        assert_eq!(show(&set_value(vec![])), "Set()");
    }

    #[test]
    fn builtin_method_rejects_named_args() {
        let (mut interp, root) = fresh();
        // xs.contains(x: 1) — named arg on a built-in method is rejected.
        let call_expr = ex(ExprKind::Call {
            callee: Box::new(ex(ExprKind::Member {
                base: Box::new(ex(ExprKind::List(vec![int(1), int(2)]))),
                name: "contains".to_string(),
                safe: false,
            })),
            args: vec![Arg::Named { name: "x".to_string(), value: int(1) }],
        });
        let r = interp.eval(&call_expr, &root);
        assert!(r.is_err());
        assert!(r.unwrap_err().message.contains("positional"));
    }

    #[test]
    fn unknown_builtin_method_errors() {
        let (mut interp, _root) = fresh();
        let xs = int_list(&[1, 2, 3]);
        let items = match &xs {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        let r = interp.list_method(&items, "no_such_method", vec![], sp());
        assert!(r.is_err());
        assert!(r.unwrap_err().message.contains("no method"));
    }
    // =====================================================================
    // M2 Wave 1-B — collections, comprehensions, tuples, default/named args.
    // Asserted on structure / scalars (not Map/Set `Show`, which W1-A owns).
    // =====================================================================

    /// Collect a `Value::List`'s elements, or panic.
    fn list_vals(v: Value) -> Vec<Value> {
        match v {
            Value::List(items) => items.borrow().clone(),
            other => panic!("expected List, got {:?}", other),
        }
    }

    fn as_int(v: &Value) -> i64 {
        match v {
            Value::Int(n) => n.to_i64().expect("fits i64"),
            other => panic!("expected Int, got {:?}", other),
        }
    }

    #[test]
    fn tuple_literal_and_eq() {
        // (1, 2) == (1, 2)
        match eval_src("val t = (1, 2)\nt == (1, 2)\n") {
            Value::Bool(b) => assert!(b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    #[test]
    fn set_literal_dedups() {
        // A set literal drops structural duplicates; collect its len via a list.
        match eval_src("val s = {1, 2, 2, 3, 1}\ns\n") {
            Value::Set(items) => assert_eq!(items.borrow().len(), 3),
            v => panic!("expected Set, got {:?}", v),
        }
    }

    #[test]
    fn map_literal_dedups_last_wins() {
        // A repeated key keeps first-seen position but the latest value.
        match eval_src("val m = {1: 10, 1: 20}\nm\n") {
            Value::Map(pairs) => {
                let p = pairs.borrow();
                assert_eq!(p.len(), 1);
                assert_eq!(as_int(&p[0].1), 20);
            }
            v => panic!("expected Map, got {:?}", v),
        }
    }

    #[test]
    fn list_comprehension_runs() {
        // [x * x for x in 1..=5 if x != 3] == [1, 4, 16, 25]
        let xs = list_vals(eval_src("[x * x for x in 1..=5 if x != 3]\n"));
        let got: Vec<i64> = xs.iter().map(as_int).collect();
        assert_eq!(got, vec![1, 4, 16, 25]);
    }

    #[test]
    fn set_comprehension_dedups() {
        match eval_src("{x % 3 for x in 0..9}\n") {
            Value::Set(items) => assert_eq!(items.borrow().len(), 3), // {0, 1, 2}
            v => panic!("expected Set, got {:?}", v),
        }
    }

    #[test]
    fn comprehension_over_tuple_list_destructures() {
        // Iterate a list of tuples, destructuring each into (a, b); sum a + b.
        let xs = list_vals(eval_src("[a + b for (a, b) in [(1, 2), (3, 4)]]\n"));
        let got: Vec<i64> = xs.iter().map(as_int).collect();
        assert_eq!(got, vec![3, 7]);
    }

    #[test]
    fn comprehension_filter_must_be_bool() {
        // A non-Bool filter is a runtime error (no truthiness).
        let toks = crate::lexer::lex("[x for x in 0..3 if x]\n").unwrap();
        let program = crate::parser::parse(&toks).unwrap();
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let last = &program.stmts[program.stmts.len() - 1];
        if let StmtKind::Expr(e) = &last.kind {
            assert!(interp.eval(e, &root).is_err());
        } else {
            panic!("expected expr statement");
        }
    }

    #[test]
    fn val_tuple_destructure_binds_names() {
        // val (a, b) = (7, 9) ; a + b == 16
        assert_eq!(as_int(&eval_src("val (a, b) = (7, 9)\na + b\n")), 16);
    }

    #[test]
    fn for_tuple_destructure_sums() {
        // for (a, b) in [(1,2),(3,4)]: total = total + a + b  -> 10
        let src = "total = 0\nfor (a, b) in [(1, 2), (3, 4)]:\n    total = total + a + b\ntotal\n";
        assert_eq!(as_int(&eval_src(src)), 10);
    }

    #[test]
    fn default_arg_used_when_omitted() {
        // fn add(a: Int, b: Int = 10) -> Int: a + b ; add(5) == 15
        let src = "fn add(a: Int, b: Int = 10) -> Int:\n    a + b\nadd(5)\n";
        assert_eq!(as_int(&eval_src(src)), 15);
    }

    #[test]
    fn default_arg_overridden_positionally() {
        let src = "fn add(a: Int, b: Int = 10) -> Int:\n    a + b\nadd(5, 100)\n";
        assert_eq!(as_int(&eval_src(src)), 105);
    }

    #[test]
    fn named_call_arg_binds_by_name() {
        // greeting passed by name; result is its concatenation.
        let src = "fn greet(name: String, greeting: String = \"Hi\") -> String:\n    greeting + name\ngreet(\"Ada\", greeting: \"Hello \")\n";
        match eval_src(src) {
            Value::Str(s) => assert_eq!(s, "Hello Ada"),
            v => panic!("expected String, got {:?}", v),
        }
    }

    #[test]
    fn lambda_passed_to_function_typed_param() {
        // A lambda flows into a function-typed param and is called.
        let src = "fn apply(f: (Int) -> Int, x: Int) -> Int:\n    f(x)\napply(n -> n * n, 6)\n";
        assert_eq!(as_int(&eval_src(src)), 36);
    }

    #[test]
    fn missing_required_arg_errors() {
        // fn f(a: Int): a ; f() -> arity error.
        let toks = crate::lexer::lex("fn f(a: Int) -> Int:\n    a\nf()\n").unwrap();
        let program = crate::parser::parse(&toks).unwrap();
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let mut err = None;
        for stmt in &program.stmts {
            if let Err(d) = interp.exec_stmt(stmt, &root) {
                err = Some(d);
                break;
            }
        }
        assert!(err.is_some(), "expected an arity error");
    }

    #[test]
    fn tuple_destructure_arity_mismatch_errors() {
        // val (a, b) = (1, 2, 3) -> runtime error.
        let toks = crate::lexer::lex("val (a, b) = (1, 2, 3)\n").unwrap();
        let program = crate::parser::parse(&toks).unwrap();
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let r = interp.exec_stmt(&program.stmts[0], &root);
        assert!(r.is_err());
    }

    // ----- M2 Wave 2-A: match guards / or-patterns / tuple+nested patterns ----

    #[test]
    fn match_guard_true_takes_arm() {
        // The guard holds, so the guarded arm fires (and sees the binding `x`).
        let src = "n = 7\nmatch n:\n    x if x > 5: x * 2\n    _: 0\n";
        assert_eq!(as_int(&eval_src(src)), 14);
    }

    #[test]
    fn match_guard_false_falls_through() {
        // The guard fails, so control falls to the next (catch-all) arm.
        let src = "n = 3\nmatch n:\n    x if x > 5: x * 2\n    _: 99\n";
        assert_eq!(as_int(&eval_src(src)), 99);
    }

    #[test]
    fn match_guard_non_bool_errors() {
        // A non-Bool guard is a runtime error (no truthiness).
        let toks = crate::lexer::lex("match 1:\n    x if x: 1\n    _: 0\n").unwrap();
        let program = crate::parser::parse(&toks).unwrap();
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        if let StmtKind::Expr(e) = &program.stmts[0].kind {
            assert!(interp.eval(e, &root).is_err());
        } else {
            panic!("expected expr statement");
        }
    }

    #[test]
    fn or_pattern_literal_alternatives() {
        // Any of 1/2/3 takes the arm; 4 falls through.
        let hit = "n = 2\nmatch n:\n    1 or 2 or 3: 1\n    _: 0\n";
        let miss = "n = 4\nmatch n:\n    1 or 2 or 3: 1\n    _: 0\n";
        assert_eq!(as_int(&eval_src(hit)), 1);
        assert_eq!(as_int(&eval_src(miss)), 0);
    }

    #[test]
    fn or_pattern_variant_alternatives() {
        // `.Red or .Green` matches either variant of the scrutinee's enum.
        let src = "enum Color:\n    Red\n    Green\n    Blue\nc = Color.Green\nmatch c:\n    .Red or .Green: 1\n    .Blue: 0\n";
        assert_eq!(as_int(&eval_src(src)), 1);
    }

    #[test]
    fn or_pattern_binds_from_matching_alternative() {
        // The binding from the alternative that matched is visible in the body.
        let src = "n = 5\nmatch n:\n    0: 0\n    x or y: x\n";
        assert_eq!(as_int(&eval_src(src)), 5);
    }

    #[test]
    fn tuple_pattern_destructures() {
        // A 2-tuple binds element-wise.
        let src = "p = (3, 4)\nmatch p:\n    (a, b): a + b\n";
        assert_eq!(as_int(&eval_src(src)), 7);
    }

    #[test]
    fn tuple_pattern_wrong_arity_skips() {
        // A 3-arity tuple pattern does not match a 2-tuple; fall through.
        let src = "p = (1, 2)\nmatch p:\n    (a, b, c): 0\n    _: 99\n";
        assert_eq!(as_int(&eval_src(src)), 99);
    }

    #[test]
    fn nested_variant_subpattern_binds_inner() {
        // A variant nested inside a variant binds the inner payload.
        let src = "enum Inner:\n    Pair(Int, Int)\nenum Outer:\n    Wrap(Inner)\n    None\nv = Outer.Wrap(Inner.Pair(2, 5))\nmatch v:\n    .Wrap(.Pair(a, b)): a + b\n    .None: 0\n";
        assert_eq!(as_int(&eval_src(src)), 7);
    }

    #[test]
    fn nested_literal_subpattern_filters() {
        // A literal sub-pattern only matches a specific payload value.
        let src = "enum Tag:\n    N(Int)\nv = Tag.N(2)\nmatch v:\n    .N(1): 10\n    .N(2): 20\n    .N(x): x\n";
        assert_eq!(as_int(&eval_src(src)), 20);
    }
    // ---- M2 Wave 2-B: `?.` safe-call and `.expect` -----------------------

    #[test]
    fn safe_member_on_null_yields_null() {
        // x?.field on a null receiver short-circuits to null.
        let src = "struct P:\n    name: String\nx: P? = null\nx?.name\n";
        assert!(matches!(eval_src(src), Value::Null));
    }

    #[test]
    fn safe_member_on_present_reads_field() {
        // x?.field on a present struct reads the field like `.`.
        let src = "struct P:\n    name: String\nx: P? = P(name: \"Ada\")\nx?.name\n";
        match eval_src(src) {
            Value::Str(s) => assert_eq!(s, "Ada"),
            v => panic!("expected String, got {:?}", v),
        }
    }

    #[test]
    fn safe_method_call_on_null_yields_null_without_evaluating_args() {
        // x?.m(panic(...)) on a null receiver must NOT evaluate the args — if it
        // did, the `panic` would surface as an error.
        let src =
            "struct P:\n    name: String\nx: P? = null\nx?.greet(panic(\"boom\"))\n";
        assert!(matches!(eval_src(src), Value::Null));
    }

    #[test]
    fn safe_call_chain_short_circuits() {
        // a?.b?.c yields null when an inner link is null.
        let src = "struct Inner:\n    n: Int\nstruct Outer:\n    inner: Inner?\nval o = Outer(inner: null)\no?.inner?.n\n";
        assert!(matches!(eval_src(src), Value::Null));
    }

    #[test]
    fn safe_call_chain_reaches_value_when_present() {
        let src = "struct Inner:\n    n: Int\nstruct Outer:\n    inner: Inner?\nval o = Outer(inner: Inner(n: 42))\no?.inner?.n\n";
        assert_eq!(as_int(&eval_src(src)), 42);
    }

    #[test]
    fn expect_present_returns_value() {
        // x.expect(msg) on a present value returns the value unchanged.
        let src = "val x: Int? = 7\nx.expect(\"required\")\n";
        assert_eq!(as_int(&eval_src(src)), 7);
    }

    #[test]
    fn expect_null_panics_with_message() {
        // x.expect(msg) on null is a runtime error carrying `msg`.
        let d = eval_src_err("val x: Int? = null\nx.expect(\"name was required\")\n");
        assert!(
            d.message.contains("name was required"),
            "expect panic should carry the message: {}",
            d.message
        );
    }
}
