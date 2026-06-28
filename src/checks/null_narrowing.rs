// ===========================================================================
// Check 2 — Null-narrowing
// ===========================================================================

use std::collections::HashMap;

use crate::ast::{
    BinOp, Binder, Binding, Expr, ExprKind, FnDecl, IfStmt, Param, Program, Stmt, StmtKind,
};
use crate::error::Diagnostic;

use super::{arg_expr, binder_names, walk_child_exprs};

pub(super) struct NullNarrowing<'a> {
    pub(super) diags: &'a mut Vec<Diagnostic>,
}

/// What we know about a name w.r.t. nullability in the current flow.
#[derive(Clone, Copy, PartialEq)]
enum NullState {
    /// Declared nullable (`T?`) and not (yet) narrowed in this flow.
    Nullable,
    /// Known non-null — either a non-nullable type, or narrowed.
    NonNull,
    /// We don't track it (conservative; never flagged).
    Unknown,
}

impl<'a> NullNarrowing<'a> {
    pub(super) fn run(&mut self, program: &Program) {
        let mut scope: HashMap<String, NullState> = HashMap::new();
        self.check_stmts(&program.stmts, &mut scope);
    }

    fn check_stmts(&mut self, stmts: &[Stmt], scope: &mut HashMap<String, NullState>) {
        for s in stmts {
            self.check_stmt(s, scope);
        }
    }

    fn check_stmt(&mut self, s: &Stmt, scope: &mut HashMap<String, NullState>) {
        match &s.kind {
            StmtKind::Binding(b) => {
                // The initializer is a use-context only loosely: a bare `x` on
                // the RHS doesn't "require T", so don't flag plain references.
                // But nested uses inside it (e.g. `x.f`) should be checked.
                self.check_expr_uses(&b.value, scope);
                match &b.binder {
                    Binder::Name(name) => {
                        let state = self.binding_null_state(b, scope);
                        scope.insert(name.clone(), state);
                    }
                    // Destructured names have no tracked nullability (a tuple
                    // binder carries no annotation); treat each as unknown.
                    Binder::Tuple(names) => {
                        for n in names {
                            scope.insert(n.clone(), NullState::Unknown);
                        }
                    }
                }
            }
            StmtKind::Assign(a) => {
                self.check_expr_uses(&a.value, scope);
                for seg in &a.target.path {
                    if let crate::ast::TargetSeg::Index(e) = seg {
                        self.check_expr_uses(e, scope);
                    }
                }
                // A plain `x = e` reassignment updates x's nullability from e.
                if a.target.path.is_empty() {
                    let st = self.expr_null_state(&a.value, scope);
                    scope.insert(a.target.base.clone(), st);
                }
            }
            StmtKind::Return(Some(e)) => self.check_expr_uses(e, scope),
            StmtKind::Return(None) => {}
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Expr(e) => self.check_expr_uses(e, scope),

            StmtKind::If(if_stmt) => self.check_if(if_stmt, scope),
            StmtKind::While(w) => {
                self.check_expr_uses(&w.cond, scope);
                let mut inner = scope.clone();
                // Apply the same positive narrowing inside the loop body.
                if let Some(name) = is_not_null_guard(&w.cond) {
                    inner.insert(name.to_string(), NullState::NonNull);
                }
                self.check_stmts(&w.body.stmts, &mut inner);
            }
            StmtKind::For(fr) => {
                self.check_expr_uses(&fr.iter, scope);
                let mut inner = scope.clone();
                for n in binder_names(&fr.binder) {
                    inner.insert(n.clone(), NullState::Unknown);
                }
                self.check_stmts(&fr.body.stmts, &mut inner);
            }

            StmtKind::Fn(decl) => self.check_fn(decl),
            StmtKind::Struct(_) => {}
            StmtKind::Impl(id) => {
                for m in &id.methods {
                    self.check_fn(m);
                }
            }
            StmtKind::Enum(_) => {}
            StmtKind::Trait(t) => {
                for m in &t.defaults {
                    self.check_fn(m);
                }
            }
        }
    }

    fn check_fn(&mut self, decl: &FnDecl) {
        let mut scope: HashMap<String, NullState> = HashMap::new();
        for p in &decl.params {
            if let Param::Named { name, ty, .. } = p {
                let st = if ty.nullable { NullState::Nullable } else { NullState::NonNull };
                scope.insert(name.clone(), st);
            }
        }
        self.check_stmts(&decl.body.stmts, &mut scope);
    }

    /// `if x is not null:` narrows `x` to non-null inside the then-branch.
    /// We handle the positive then-branch narrowing only (per M1 scope).
    fn check_if(&mut self, if_stmt: &IfStmt, scope: &mut HashMap<String, NullState>) {
        for (cond, body) in &if_stmt.arms {
            // Each arm's condition is evaluated in the *outer* scope.
            self.check_expr_uses(cond, scope);

            let mut inner = scope.clone();
            if let Some(name) = is_not_null_guard(cond) {
                inner.insert(name.to_string(), NullState::NonNull);
            }
            self.check_stmts(&body.stmts, &mut inner);

            // For an `elif`, earlier conditions were false on this path, but we
            // don't do negative narrowing in M1 — just keep the outer scope.
        }
        if let Some(else_body) = &if_stmt.else_body {
            let mut inner = scope.clone();
            self.check_stmts(&else_body.stmts, &mut inner);
        }
    }

    /// Determine a binding's resulting null-state: an explicit nullable
    /// annotation, `= null`, or otherwise inferred from the initializer.
    fn binding_null_state(&self, b: &Binding, scope: &HashMap<String, NullState>) -> NullState {
        if let Some(ty) = &b.ty {
            return if ty.nullable { NullState::Nullable } else { NullState::NonNull };
        }
        self.expr_null_state(&b.value, scope)
    }

    /// A conservative null-state for an expression value.
    fn expr_null_state(&self, e: &Expr, scope: &HashMap<String, NullState>) -> NullState {
        match &e.kind {
            ExprKind::Null => NullState::Nullable,
            ExprKind::Name(n) => scope.get(n).copied().unwrap_or(NullState::Unknown),
            // `x.or_else(default)` and `x.expect(msg)` both yield a non-null
            // value (the latter `panic`s rather than producing `null`).
            ExprKind::Call { callee, .. } if is_null_handling_call(callee) => NullState::NonNull,
            // Most other literals/constructions are non-null, but we only need
            // to be sure for flagging; treat unknown to stay conservative about
            // *propagation* (we never flag based on Unknown).
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::List(_) => NullState::NonNull,
            _ => NullState::Unknown,
        }
    }

    /// Walk an expression looking for **uses that require a non-null `T`** of a
    /// still-nullable name, flagging each. Also recurses so nested matches /
    /// calls are covered.
    fn check_expr_uses(&mut self, e: &Expr, scope: &HashMap<String, NullState>) {
        match &e.kind {
            // Member access / method call: `x.f`. Three forms *handle* a
            // nullable receiver and so do NOT require narrowing: `?.` safe access
            // (`safe: true`), `.or_else`, and `.expect` — see [`handles_null`].
            ExprKind::Member { base, name, safe } => {
                if *safe || handles_null(name) {
                    // The receiver may be nullable here. Still recurse into a
                    // non-name base, but do NOT treat a bare nullable name as a
                    // required-T use.
                    if !matches!(base.kind, ExprKind::Name(_)) {
                        self.check_expr_uses(base, scope);
                    }
                } else {
                    self.require_non_null(base, scope, "member access");
                    self.check_expr_uses(base, scope);
                }
            }
            ExprKind::Call { callee, args } => {
                // A method call whose callee *handles* null — `x.or_else(d)`,
                // `x.expect(m)`, or any `x?.m(…)` safe-call — may take a nullable
                // receiver `x` without narrowing.
                if let ExprKind::Member { base, name, safe } = &callee.kind {
                    if *safe || handles_null(name) {
                        // Receiver use is fine even if nullable; recurse into a
                        // non-name base only.
                        if !matches!(base.kind, ExprKind::Name(_)) {
                            self.check_expr_uses(base, scope);
                        }
                        for a in args {
                            self.check_expr_uses(arg_expr(a), scope);
                        }
                        return;
                    }
                    // Any other method call `x.foo(...)` requires non-null x.
                    self.require_non_null(base, scope, "method call");
                    self.check_expr_uses(base, scope);
                } else {
                    self.check_expr_uses(callee, scope);
                }
                for a in args {
                    self.check_expr_uses(arg_expr(a), scope);
                }
            }
            ExprKind::Index { base, index } => {
                self.require_non_null(base, scope, "indexing");
                self.check_expr_uses(base, scope);
                self.check_expr_uses(index, scope);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                // Equality / `is` / `is not` against operands does NOT require
                // non-null (you may compare a nullable value to null). All other
                // binary ops (arithmetic, ordering, ranges) require non-null.
                let requires = !matches!(
                    op,
                    BinOp::Eq | BinOp::NotEq | BinOp::Is | BinOp::IsNot | BinOp::And | BinOp::Or
                );
                if requires {
                    self.require_non_null(lhs, scope, "operator operand");
                    self.require_non_null(rhs, scope, "operator operand");
                }
                self.check_expr_uses(lhs, scope);
                self.check_expr_uses(rhs, scope);
            }
            ExprKind::Unary { op, operand } => {
                if matches!(op, crate::ast::UnOp::Neg) {
                    self.require_non_null(operand, scope, "operator operand");
                }
                self.check_expr_uses(operand, scope);
            }
            ExprKind::Match(m) => {
                self.check_expr_uses(&m.scrutinee, scope);
                for arm in &m.arms {
                    let mut inner = scope.clone();
                    self.check_stmts(&arm.body.stmts, &mut inner);
                }
            }
            // Nodes that impose no required-non-null context on their children —
            // Ternary/List/Lambda/Str/Map/Set/Tuple/Comprehension — just recurse
            // into every sub-expression (and the leaves recurse into nothing). A
            // comprehension binder is untracked (its element type is unknown), so
            // it stays absent from `scope` and is never flagged. A bare `Name`
            // reference on its own does not "require T".
            _ => walk_child_exprs(e, &mut |child| self.check_expr_uses(child, scope)),
        }
    }

    /// If `e` is a bare name proven to still be nullable, flag it as an invalid
    /// use where a non-null `T` is required.
    fn require_non_null(&mut self, e: &Expr, scope: &HashMap<String, NullState>, ctx: &str) {
        if let ExprKind::Name(n) = &e.kind {
            if let Some(NullState::Nullable) = scope.get(n) {
                self.diags.push(Diagnostic::check(
                    format!(
                        "nullable value `{}` used in {} where a non-null value is required; narrow with `if {} is not null:` or default with `{}.or_else(...)`",
                        n, ctx, n, n
                    ),
                    e.span,
                ));
            }
        }
    }
}

/// Recognize the narrowing guard `x is not null` (scrutinee exactly a name).
/// Returns the narrowed name. Handles both `x is not null` and the mirror
/// `null is not x` shape.
fn is_not_null_guard(cond: &Expr) -> Option<&str> {
    if let ExprKind::Binary { op: BinOp::IsNot, lhs, rhs } = &cond.kind {
        match (&lhs.kind, &rhs.kind) {
            (ExprKind::Name(n), ExprKind::Null) => return Some(n),
            (ExprKind::Null, ExprKind::Name(n)) => return Some(n),
            _ => {}
        }
    }
    None
}

/// The two methods that consume a nullable receiver and produce a non-null
/// result: `.or_else(default)` (substitutes the default) and `.expect(msg)`
/// (asserts non-null, `panic`king otherwise). Both are valid ways to handle a
/// `T?` and both yield a non-null `T`.
fn handles_null(name: &str) -> bool {
    name == "or_else" || name == "expect"
}

/// True if `callee` is a member access for a null-handling method
/// (`<something>.or_else` / `<something>.expect`) — see [`handles_null`].
fn is_null_handling_call(callee: &Expr) -> bool {
    matches!(&callee.kind, ExprKind::Member { name, .. } if handles_null(name))
}
