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
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::ast::{
    Arg, BinOp, Block, EnumDecl, Expr, ExprKind, FnDecl, ForStmt, IfStmt, Lambda, LitPattern,
    MatchExpr, Param, Pattern, PatternKind, Payload, Program, Stmt, StmtKind, StrSeg, StructDecl,
    SubPattern, Target, TargetSeg, UnOp, WhileStmt,
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
                if !b.is_val && b.ty.is_none() {
                    // Bare `x = e` (grammar §4.1): reassign if the name already
                    // resolves in an accessible scope, otherwise introduce it
                    // here. A bare-name reassignment is parsed as a `Binding`,
                    // not an `Assign`, so the `val`-immutability and
                    // cross-scope mutation rules must be enforced on this path.
                    match env_assign(env, &b.name, v.clone()) {
                        Ok(true) => {}
                        Ok(false) => env_define(env, &b.name, v, false),
                        Err(()) => {
                            return Err(Diagnostic::runtime(
                                format!("cannot reassign `val` binding `{}`", b.name),
                                stmt.span,
                            ));
                        }
                    }
                } else {
                    // `val x = e` or typed `x: T = e`: a declaration in this scope.
                    env_define(env, &b.name, v, b.is_val);
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
            // Each iteration gets a fresh scope holding the loop variable.
            let scope = Scope::child(env);
            env_define(&scope, &s.var, item, false);
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
            ExprKind::Member { base, name } => self.eval_member(base, name, expr.span, env),
            ExprKind::Match(m) => self.eval_match(m, expr.span, env),
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
        if let ExprKind::Member { base, name } = &callee.kind {
            // Qualified enum-variant construction: `Enum.Variant(args)`. Only
            // when the base names a known enum that isn't shadowed by a value.
            if let ExprKind::Name(enum_name) = &base.kind {
                if env_get(env, enum_name).is_none()
                    && self.registry.enums.contains_key(enum_name)
                {
                    return self.construct_variant(enum_name, name, args, span, env);
                }
            }

            // Otherwise it is a method call `recv.method(args)`.
            let recv = self.eval(base, env)?;

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

    /// Bind positional params (handling a leading `self` if `self_val` already
    /// placed in scope). Here params do not include a pre-bound self.
    fn bind_params(
        &self,
        params: &[Param],
        args: &[Value],
        scope: &Env,
        fn_name: &str,
        span: Span,
    ) -> Result<(), Diagnostic> {
        // Count non-self params.
        let named: Vec<&Param> = params
            .iter()
            .filter(|p| !matches!(p, Param::SelfRecv))
            .collect();
        if named.len() != args.len() {
            return Err(Diagnostic::runtime(
                format!(
                    "`{}` expects {} argument(s), got {}",
                    fn_name,
                    named.len(),
                    args.len()
                ),
                span,
            ));
        }
        for (p, v) in named.iter().zip(args.iter()) {
            if let Param::Named { name, .. } = p {
                env_define(scope, name, v.clone(), false);
            }
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
        }
    }

    /// Resolve and call a method `recv.name(args)`.
    fn call_method(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> EvalResult {
        let type_name_str = match &recv {
            Value::Struct(s) => s.borrow().type_name.clone(),
            Value::Enum(e) => e.enum_name.clone(),
            other => {
                return Err(Diagnostic::runtime(
                    format!("cannot call method `.{}` on {}", name, type_name(other)),
                    span,
                ));
            }
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

        let arg_vals = self.eval_positional_args(args, span, env)?;

        // Method scope is a child of the *global* root env (captured via the
        // env chain). Methods resolve other top-level names through the call
        // env's root; we use the call-site env's chain root so globals (other
        // fns) are visible. Bind `self` first.
        let method_scope = Scope::child(&root_of(env));
        env_define(&method_scope, "self", recv, false);
        self.bind_params(&method.params, &arg_vals, &method_scope, &method.name, span)?;

        match self.exec_stmts(&method.body.stmts, &method_scope)? {
            Flow::Normal(v) => Ok(v),
            Flow::Return(v) => Ok(v),
            Flow::Break | Flow::Continue => Err(Diagnostic::runtime(
                "`break`/`continue` outside a loop".to_string(),
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
            if self.try_match(&arm.pattern, &scrutinee, &arm_scope)? {
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
                for (sub, payload_v) in subs.iter().zip(inst.payload.iter()) {
                    if !self.sub_matches(sub, payload_v, scope)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    fn sub_matches(
        &mut self,
        sub: &SubPattern,
        val: &Value,
        scope: &Env,
    ) -> Result<bool, Diagnostic> {
        match sub {
            SubPattern::Wildcard => Ok(true),
            SubPattern::Null => Ok(matches!(val, Value::Null)),
            SubPattern::Literal(lit) => Ok(lit_matches(lit, val)),
            SubPattern::Binding(name) => {
                env_define(scope, name, val.clone(), false);
                Ok(true)
            }
        }
    }
}

// ===========================================================================
// Free helpers (no &self)
// ===========================================================================

/// The root (outermost) env of a chain — used so methods see top-level names.
fn root_of(env: &Env) -> Env {
    let parent = env.borrow().parent.clone();
    match parent {
        Some(p) => root_of(&p),
        None => Rc::clone(env),
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
            is_val: false,
            ty: None,
            value: int(0),
        }));
        let for_stmt = st(StmtKind::For(ForStmt {
            var: "x".to_string(),
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
            is_val: false,
            ty: None,
            value: int(0),
        }));
        let for_stmt = st(StmtKind::For(ForStmt {
            var: "x".to_string(),
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
                    body: block(vec![expr_stmt(int(0))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: Pattern {
                        kind: PatternKind::Variant {
                            enum_name: None,
                            name: "B".to_string(),
                            subs: vec![SubPattern::Binding("n".to_string())],
                        },
                        span: sp(),
                    },
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
                    body: block(vec![expr_stmt(int(100))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: Pattern { kind: PatternKind::Wildcard, span: sp() },
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
                        .map(|s| SubPattern::Binding(s.to_string()))
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
                    body: block(vec![expr_stmt(name("n"))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Add", vec!["a", "b"]),
                    body: block(vec![expr_stmt(bin(
                        BinOp::Add,
                        call(name("eval"), vec![name("a")]),
                        call(name("eval"), vec![name("b")]),
                    ))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Mul", vec!["a", "b"]),
                    body: block(vec![expr_stmt(bin(
                        BinOp::Mul,
                        call(name("eval"), vec![name("a")]),
                        call(name("eval"), vec![name("b")]),
                    ))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Div", vec!["a", "b"]),
                    body: block(vec![
                        st(StmtKind::Binding(Binding {
                            name: "divisor".to_string(),
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
                ex(ExprKind::Member { base: Box::new(name("Expr")), name: v.to_string() }),
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
                        .map(|s| SubPattern::Binding(s.to_string()))
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
                    body: block(vec![expr_stmt(name("n"))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Div", vec!["a", "b"]),
                    body: block(vec![
                        st(StmtKind::Binding(Binding {
                            name: "divisor".to_string(),
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
                ex(ExprKind::Member { base: Box::new(name("E")), name: v.to_string() }),
                args,
            )
        };
        let num = |f: f64| vc("Num", vec![float(f)]);
        let div_zero = call(name("eval"), vec![vc("Div", vec![num(1.0), num(0.0)])]);
        let r = interp.eval(&div_zero, &root);
        assert!(r.is_err());
        assert!(r.unwrap_err().message.contains("division by zero"));
    }
}
