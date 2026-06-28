//! Stage 3 — **static checks** (compile-time): the two marquee analyses.
//!
//! Owned by the *checks agent*. Runs over the [`Program`] *before* execution
//! and owns exactly the two MVP static analyses (see `02-mvp-scope.md`):
//!
//! 1. **Match exhaustiveness** — a `match` over an enum must cover every
//!    variant (or use `_`). Removing an arm is a compile-time error.
//! 2. **Null-narrowing** — using a `T?` value where a `T` is required is a
//!    compile-time error unless it has been narrowed (`if x is not null:`) or
//!    defaulted (`.or_else(...)`).
//!
//! These are the **only** checks this stage owns. In particular, `val`
//! reassignment rejection and non-`Bool` condition rejection are **runtime**
//! checks owned by [`crate::interp`] (per spec) — do not duplicate them here.
//!
//! ## Design notes
//!
//! Neither analysis runs full type inference (out of scope for M1). Both are
//! deliberately **conservative**: they only fire when the relevant fact (the
//! scrutinee is a known enum / a name is still nullable at a use site) can be
//! *proven* from local annotations and obvious constructions. Preferring zero
//! false positives over completeness is the explicit M1 tradeoff.
//!
//! Contract: `Ok(())` if the program passes, else **all** collected
//! [`Diagnostic`]s.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Arg, BinOp, Binding, EnumDecl, Expr, ExprKind, FnDecl, IfStmt, ImplDecl, Param,
    PatternKind, Payload, Program, Stmt, StmtKind, Type, BaseType, MatchExpr,
};
use crate::error::Diagnostic;
use crate::token::Span;

/// Run the compile-time checks (exhaustiveness + null-narrowing) over `program`.
pub fn check(program: &Program) -> Result<(), Vec<Diagnostic>> {
    let mut diags = Vec::new();

    // Shared, program-wide enum information, gathered once.
    let enums = EnumInfo::gather(program);

    // Check 1 — match exhaustiveness.
    let mut ex = Exhaustiveness { enums: &enums, diags: &mut diags };
    ex.run(program);

    // Check 2 — null-narrowing.
    let mut nn = NullNarrowing { enums: &enums, diags: &mut diags };
    nn.run(program);

    if diags.is_empty() {
        Ok(())
    } else {
        Err(diags)
    }
}

// ===========================================================================
// Shared: enum information gathered across the whole program
// ===========================================================================

/// Program-wide enum metadata used by both analyses.
struct EnumInfo {
    /// enum name → ordered list of (variant name, arity).
    ///
    /// `arity` is the number of payload fields (`0` for niladic). Variant order
    /// preserves declaration order so missing-variant messages read naturally.
    enums: HashMap<String, Vec<(String, usize)>>,
    /// variant name → enum name. Only populated for variants whose name is
    /// **unambiguous** across all enums; an entry is removed if a second enum
    /// reuses the same variant name (we can't pick one, so we stay silent).
    variant_to_enum: HashMap<String, String>,
}

impl EnumInfo {
    fn gather(program: &Program) -> Self {
        let mut enums: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        // Track names seen more than once so we can drop ambiguous mappings.
        let mut variant_owner: HashMap<String, Option<String>> = HashMap::new();

        let record = |decl: &EnumDecl,
                          enums: &mut HashMap<String, Vec<(String, usize)>>,
                          variant_owner: &mut HashMap<String, Option<String>>| {
            let mut variants = Vec::new();
            for v in &decl.variants {
                let arity = match &v.payload {
                    None => 0,
                    Some(Payload::Positional(tys)) => tys.len(),
                    Some(Payload::Named(fields)) => fields.len(),
                };
                variants.push((v.name.clone(), arity));

                // Ambiguity tracking for variant → enum resolution.
                match variant_owner.get(&v.name) {
                    None => {
                        variant_owner.insert(v.name.clone(), Some(decl.name.clone()));
                    }
                    Some(Some(existing)) if existing == &decl.name => { /* same enum, fine */ }
                    Some(_) => {
                        // Already owned by a different enum (or already ambiguous).
                        variant_owner.insert(v.name.clone(), None);
                    }
                }
            }
            // Last decl wins if an enum name is (oddly) re-declared; harmless.
            enums.insert(decl.name.clone(), variants);
        };

        // Walk the whole program so enums nested in fn/impl/struct bodies count.
        walk_enum_decls(&program.stmts, &mut |decl| {
            record(decl, &mut enums, &mut variant_owner);
        });

        let variant_to_enum = variant_owner
            .into_iter()
            .filter_map(|(k, v)| v.map(|e| (k, e)))
            .collect();

        EnumInfo { enums, variant_to_enum }
    }

    /// If `ty` names a known enum, return that enum's name (borrowed from the
    /// gathered enum table so the lifetime ties to `self`, not `ty`).
    fn enum_name_of_type(&self, ty: &Type) -> Option<&str> {
        if let BaseType::Named { name, .. } = &ty.base {
            if let Some((key, _)) = self.enums.get_key_value(name) {
                return Some(key.as_str());
            }
        }
        None
    }
}

/// Visit every [`EnumDecl`] reachable in a statement list (recursing into fn,
/// impl, struct, and control-flow bodies).
fn walk_enum_decls(stmts: &[Stmt], f: &mut impl FnMut(&EnumDecl)) {
    for s in stmts {
        match &s.kind {
            StmtKind::Enum(d) => {
                f(d);
            }
            StmtKind::Fn(decl) => walk_enum_decls(&decl.body.stmts, f),
            StmtKind::Impl(ImplDecl { methods, .. }) => {
                for m in methods {
                    walk_enum_decls(&m.body.stmts, f);
                }
            }
            StmtKind::If(IfStmt { arms, else_body }) => {
                for (_, body) in arms {
                    walk_enum_decls(&body.stmts, f);
                }
                if let Some(b) = else_body {
                    walk_enum_decls(&b.stmts, f);
                }
            }
            StmtKind::While(w) => walk_enum_decls(&w.body.stmts, f),
            StmtKind::For(fr) => walk_enum_decls(&fr.body.stmts, f),
            _ => {}
        }
    }
}

// ===========================================================================
// Check 1 — Match exhaustiveness
// ===========================================================================

struct Exhaustiveness<'a> {
    enums: &'a EnumInfo,
    diags: &'a mut Vec<Diagnostic>,
}

/// A lightweight, locally-resolved type for a name. We only care about whether
/// a name is a known enum; everything else is `Unknown` (conservative).
#[derive(Clone)]
enum ResolvedTy {
    /// A value of the named enum.
    Enum(String),
    /// Anything we couldn't (or didn't try to) resolve.
    Unknown,
}

impl<'a> Exhaustiveness<'a> {
    fn run(&mut self, program: &Program) {
        // Top-level scope. Top-level bindings can introduce enum-typed names.
        let mut scope: HashMap<String, ResolvedTy> = HashMap::new();
        self.check_stmts(&program.stmts, &mut scope);
    }

    /// Walk statements, threading a `name → ResolvedTy` environment built from
    /// annotations and obvious enum constructions.
    fn check_stmts(&mut self, stmts: &[Stmt], scope: &mut HashMap<String, ResolvedTy>) {
        for s in stmts {
            self.check_stmt(s, scope);
        }
    }

    fn check_stmt(&mut self, s: &Stmt, scope: &mut HashMap<String, ResolvedTy>) {
        match &s.kind {
            StmtKind::Binding(b) => {
                // Visit the initializer first (it may contain matches).
                self.check_expr(&b.value, scope);
                let ty = self.resolve_binding_ty(b, scope);
                scope.insert(b.name.clone(), ty);
            }
            StmtKind::Assign(a) => {
                self.check_expr(&a.value, scope);
                for seg in &a.target.path {
                    if let crate::ast::TargetSeg::Index(e) = seg {
                        self.check_expr(e, scope);
                    }
                }
            }
            StmtKind::Return(Some(e)) => self.check_expr(e, scope),
            StmtKind::Return(None) => {}
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Expr(e) => self.check_expr(e, scope),

            StmtKind::If(IfStmt { arms, else_body }) => {
                for (cond, body) in arms {
                    self.check_expr(cond, scope);
                    let mut inner = scope.clone();
                    self.check_stmts(&body.stmts, &mut inner);
                }
                if let Some(b) = else_body {
                    let mut inner = scope.clone();
                    self.check_stmts(&b.stmts, &mut inner);
                }
            }
            StmtKind::While(w) => {
                self.check_expr(&w.cond, scope);
                let mut inner = scope.clone();
                self.check_stmts(&w.body.stmts, &mut inner);
            }
            StmtKind::For(fr) => {
                self.check_expr(&fr.iter, scope);
                let mut inner = scope.clone();
                // The loop var's type is unknown to this lightweight pass.
                inner.insert(fr.var.clone(), ResolvedTy::Unknown);
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
        }
    }

    /// Each function gets a fresh scope seeded with its annotated params.
    fn check_fn(&mut self, decl: &FnDecl) {
        let mut scope: HashMap<String, ResolvedTy> = HashMap::new();
        for p in &decl.params {
            if let Param::Named { name, ty, .. } = p {
                let rt = match self.enums.enum_name_of_type(ty) {
                    // A *nullable* enum param isn't a plain enum value, so don't
                    // treat it as exhaustively-matchable directly.
                    Some(e) if !ty.nullable => ResolvedTy::Enum(e.to_string()),
                    _ => ResolvedTy::Unknown,
                };
                scope.insert(name.clone(), rt);
            }
        }
        self.check_stmts(&decl.body.stmts, &mut scope);
    }

    /// Resolve a binding's type for the enum environment, from (in order):
    /// an explicit enum annotation, or an initializer that is an obvious enum
    /// construction.
    fn resolve_binding_ty(
        &self,
        b: &Binding,
        scope: &HashMap<String, ResolvedTy>,
    ) -> ResolvedTy {
        if let Some(ty) = &b.ty {
            if let Some(e) = self.enums.enum_name_of_type(ty) {
                if !ty.nullable {
                    return ResolvedTy::Enum(e.to_string());
                }
            }
            // Annotated as something non-enum (or nullable): trust it, unknown.
            return ResolvedTy::Unknown;
        }
        self.resolve_expr_ty(&b.value, scope)
    }

    /// Best-effort enum resolution of an expression's type. Only recognizes the
    /// cases the spec calls out: a name in scope, or a call constructing a
    /// known enum variant.
    fn resolve_expr_ty(&self, e: &Expr, scope: &HashMap<String, ResolvedTy>) -> ResolvedTy {
        match &e.kind {
            ExprKind::Name(n) => scope.get(n).cloned().unwrap_or(ResolvedTy::Unknown),
            ExprKind::Call { callee, .. } => {
                if let ExprKind::Name(callee_name) = &callee.kind {
                    if let Some(enum_name) = self.enums.variant_to_enum.get(callee_name) {
                        return ResolvedTy::Enum(enum_name.clone());
                    }
                }
                ResolvedTy::Unknown
            }
            _ => ResolvedTy::Unknown,
        }
    }

    /// Walk an expression, checking any `match` it contains and recursing.
    fn check_expr(&mut self, e: &Expr, scope: &HashMap<String, ResolvedTy>) {
        match &e.kind {
            ExprKind::Match(m) => {
                self.check_match(m, e.span, scope);
                // Recurse into scrutinee and arm bodies too.
                self.check_expr(&m.scrutinee, scope);
                for arm in &m.arms {
                    let mut inner = scope.clone();
                    self.check_stmts(&arm.body.stmts, &mut inner);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.check_expr(lhs, scope);
                self.check_expr(rhs, scope);
            }
            ExprKind::Unary { operand, .. } => self.check_expr(operand, scope),
            ExprKind::Ternary { then, cond, otherwise } => {
                self.check_expr(then, scope);
                self.check_expr(cond, scope);
                self.check_expr(otherwise, scope);
            }
            ExprKind::Call { callee, args } => {
                self.check_expr(callee, scope);
                for a in args {
                    match a {
                        Arg::Positional(e) => self.check_expr(e, scope),
                        Arg::Named { value, .. } => self.check_expr(value, scope),
                    }
                }
            }
            ExprKind::Index { base, index } => {
                self.check_expr(base, scope);
                self.check_expr(index, scope);
            }
            ExprKind::Member { base, .. } => self.check_expr(base, scope),
            ExprKind::List(items) => {
                for it in items {
                    self.check_expr(it, scope);
                }
            }
            ExprKind::Lambda(l) => self.check_expr(&l.body, scope),
            ExprKind::Str(s) => {
                for part in &s.parts {
                    if let crate::ast::StrSeg::Expr(inner) = part {
                        self.check_expr(inner, scope);
                    }
                }
            }
            // M2 collections / comprehensions: recurse so nested `match`es are
            // still checked. These never resolve to an enum type themselves.
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.check_expr(k, scope);
                    self.check_expr(v, scope);
                }
            }
            ExprKind::Set(items) | ExprKind::Tuple(items) => {
                for it in items {
                    self.check_expr(it, scope);
                }
            }
            ExprKind::Comprehension(c) => {
                self.check_comprehension_exprs(c, scope, &mut |this, e, sc| {
                    this.check_expr(e, sc)
                });
            }
            // Leaves.
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::Name(_)
            | ExprKind::SelfExpr => {}
        }
    }

    /// Recurse into a comprehension's sub-expressions (output, iterable, filter)
    /// with `visit`. The binder is scoped to the comprehension but is of unknown
    /// type to this lightweight pass, so we do not add it to `scope`.
    /// TODO(W1-B): once comprehensions evaluate, revisit scoping if needed.
    fn check_comprehension_exprs(
        &mut self,
        c: &crate::ast::Comprehension,
        scope: &HashMap<String, ResolvedTy>,
        visit: &mut dyn FnMut(&mut Self, &Expr, &HashMap<String, ResolvedTy>),
    ) {
        use crate::ast::ComprehensionOutput::*;
        match &c.output {
            List(e) | Set(e) => visit(self, e, scope),
            Map { key, value } => {
                visit(self, key, scope);
                visit(self, value, scope);
            }
        }
        visit(self, &c.iter, scope);
        if let Some(cond) = &c.cond {
            visit(self, cond, scope);
        }
    }

    /// The core exhaustiveness check for one `match`.
    fn check_match(
        &mut self,
        m: &MatchExpr,
        match_span: Span,
        scope: &HashMap<String, ResolvedTy>,
    ) {
        // Only proceed if we can prove the scrutinee is a known enum.
        let enum_name = match self.resolve_expr_ty(&m.scrutinee, scope) {
            ResolvedTy::Enum(e) => e,
            ResolvedTy::Unknown => return, // conservative: don't flag.
        };
        let variants = match self.enums.enums.get(&enum_name) {
            Some(v) => v,
            None => return,
        };

        let mut covered: HashSet<&str> = HashSet::new();
        let mut has_catch_all = false;

        for arm in &m.arms {
            // A *guarded* arm (`pattern if cond:`) may not fire, so it does not
            // contribute to exhaustiveness — skip its coverage entirely (M2). The
            // parser produces `guard: None` until Wave 2, so today this is a
            // no-op; it is here so guarded arms behave correctly once wired up.
            // TODO(W2-A): revisit once guards are parsed.
            if arm.guard.is_some() {
                continue;
            }
            match &arm.pattern.kind {
                PatternKind::Wildcard => has_catch_all = true,
                PatternKind::Binding(_) => has_catch_all = true, // bare NAME matches anything
                PatternKind::Variant { enum_name: qual, name, subs } => {
                    // A qualified pattern must name the scrutinee's enum.
                    if let Some(en) = qual {
                        if en != &enum_name {
                            self.diags.push(Diagnostic::check(
                                format!(
                                    "pattern matches enum `{}`, but the value is `{}`",
                                    en, enum_name
                                ),
                                arm.pattern.span,
                            ));
                            continue;
                        }
                    }
                    // Arity / unknown-variant diagnostics (cheap, best-effort).
                    if let Some((_, arity)) = variants.iter().find(|(vn, _)| vn == name) {
                        if *arity != subs.len() {
                            self.diags.push(Diagnostic::check(
                                format!(
                                    "variant `{}::{}` expects {} field(s), but the pattern binds {}",
                                    enum_name, name, arity, subs.len()
                                ),
                                arm.pattern.span,
                            ));
                        }
                        covered.insert(name.as_str());
                    } else {
                        self.diags.push(Diagnostic::check(
                            format!("unknown variant `{}` for enum `{}`", name, enum_name),
                            arm.pattern.span,
                        ));
                    }
                }
                // null and literal patterns never cover an enum variant.
                PatternKind::Null | PatternKind::Literal(_) => {}
                // Or-patterns and tuple patterns are M2 Wave 2. For now they
                // contribute no coverage (conservative — they never *over*-claim
                // exhaustiveness). The parser does not yet produce them.
                // TODO(W2-A): an or-pattern should count each alternative's
                // variant toward coverage; a tuple pattern never covers an enum.
                PatternKind::Or(_) | PatternKind::Tuple(_) => {}
            }
        }

        if has_catch_all {
            return;
        }

        let missing: Vec<&str> = variants
            .iter()
            .map(|(n, _)| n.as_str())
            .filter(|n| !covered.contains(n))
            .collect();

        if !missing.is_empty() {
            let list = missing
                .iter()
                .map(|n| format!("`{}`", n))
                .collect::<Vec<_>>()
                .join(", ");
            self.diags.push(Diagnostic::check(
                format!(
                    "non-exhaustive match over enum `{}`: missing variant(s) {} (add the arm(s) or a `_` catch-all)",
                    enum_name, list
                ),
                match_span,
            ));
        }
    }
}

// ===========================================================================
// Check 2 — Null-narrowing
// ===========================================================================

struct NullNarrowing<'a> {
    #[allow(dead_code)]
    enums: &'a EnumInfo,
    diags: &'a mut Vec<Diagnostic>,
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
    fn run(&mut self, program: &Program) {
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
                let state = self.binding_null_state(b, scope);
                scope.insert(b.name.clone(), state);
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
                inner.insert(fr.var.clone(), NullState::Unknown);
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
        for (i, (cond, body)) in if_stmt.arms.iter().enumerate() {
            // Each arm's condition is evaluated in the *outer* scope.
            self.check_expr_uses(cond, scope);

            let mut inner = scope.clone();
            if let Some(name) = is_not_null_guard(cond) {
                inner.insert(name.to_string(), NullState::NonNull);
            }
            self.check_stmts(&body.stmts, &mut inner);

            // For an `elif`, earlier conditions were false on this path, but we
            // don't do negative narrowing in M1 — just keep the outer scope.
            let _ = i;
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
            // `x.or_else(default)` always yields a non-null value.
            ExprKind::Call { callee, .. } if is_or_else_call(callee) => NullState::NonNull,
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
            // Member access / method call: `x.f`. The `or_else` member itself is
            // a *valid* use of a nullable receiver, so special-case it.
            // (`?.` safe access — `safe: true` — also handles a nullable
            // receiver; M2 Wave 2 extends this check to recognise it. TODO(W2-B).)
            ExprKind::Member { base, name, .. } => {
                if name == "or_else" {
                    // `x.or_else` — accessing or_else on a nullable x is allowed.
                    // Still recurse into base's own sub-structure, but do NOT
                    // treat a bare nullable name here as a required-T use.
                    if !matches!(base.kind, ExprKind::Name(_)) {
                        self.check_expr_uses(base, scope);
                    }
                } else {
                    self.require_non_null(base, scope, "member access");
                    self.check_expr_uses(base, scope);
                }
            }
            ExprKind::Call { callee, args } => {
                // Detect `x.or_else(d)` — the receiver `x` may be nullable here.
                if let ExprKind::Member { base, name, .. } = &callee.kind {
                    if name == "or_else" {
                        // Receiver use is fine even if nullable; recurse into a
                        // non-name base only.
                        if !matches!(base.kind, ExprKind::Name(_)) {
                            self.check_expr_uses(base, scope);
                        }
                        for a in args {
                            match a {
                                Arg::Positional(e) => self.check_expr_uses(e, scope),
                                Arg::Named { value, .. } => self.check_expr_uses(value, scope),
                            }
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
                    match a {
                        Arg::Positional(e) => self.check_expr_uses(e, scope),
                        Arg::Named { value, .. } => self.check_expr_uses(value, scope),
                    }
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
            ExprKind::Ternary { then, cond, otherwise } => {
                self.check_expr_uses(then, scope);
                self.check_expr_uses(cond, scope);
                self.check_expr_uses(otherwise, scope);
            }
            ExprKind::Match(m) => {
                self.check_expr_uses(&m.scrutinee, scope);
                for arm in &m.arms {
                    let mut inner = scope.clone();
                    self.check_stmts(&arm.body.stmts, &mut inner);
                }
            }
            ExprKind::List(items) => {
                for it in items {
                    self.check_expr_uses(it, scope);
                }
            }
            ExprKind::Lambda(l) => self.check_expr_uses(&l.body, scope),
            ExprKind::Str(s) => {
                for part in &s.parts {
                    if let crate::ast::StrSeg::Expr(inner) = part {
                        self.check_expr_uses(inner, scope);
                    }
                }
            }
            // M2 collections / comprehensions: recurse into sub-expressions.
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.check_expr_uses(k, scope);
                    self.check_expr_uses(v, scope);
                }
            }
            ExprKind::Set(items) | ExprKind::Tuple(items) => {
                for it in items {
                    self.check_expr_uses(it, scope);
                }
            }
            ExprKind::Comprehension(c) => {
                use crate::ast::ComprehensionOutput::*;
                match &c.output {
                    List(e) | Set(e) => self.check_expr_uses(e, scope),
                    Map { key, value } => {
                        self.check_expr_uses(key, scope);
                        self.check_expr_uses(value, scope);
                    }
                }
                self.check_expr_uses(&c.iter, scope);
                if let Some(cond) = &c.cond {
                    self.check_expr_uses(cond, scope);
                }
            }
            // Leaves — a bare name reference on its own does not "require T".
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::Name(_)
            | ExprKind::SelfExpr => {}
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

/// True if `callee` is a member access `<something>.or_else`.
fn is_or_else_call(callee: &Expr) -> bool {
    matches!(&callee.kind, ExprKind::Member { name, .. } if name == "or_else")
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;
    use crate::token::Span;
    use num_bigint::BigInt;

    // ---- tiny AST constructors (parser is built in parallel; do NOT use it) --

    fn sp() -> Span {
        Span::dummy()
    }

    fn ty_named(name: &str, nullable: bool) -> Type {
        Type {
            base: BaseType::Named { name: name.to_string(), args: vec![] },
            nullable,
            span: sp(),
        }
    }

    fn expr(kind: ExprKind) -> Expr {
        Expr { kind, span: sp() }
    }

    fn name(n: &str) -> Expr {
        expr(ExprKind::Name(n.to_string()))
    }

    fn call(callee: Expr, args: Vec<Expr>) -> Expr {
        expr(ExprKind::Call {
            callee: Box::new(callee),
            args: args.into_iter().map(Arg::Positional).collect(),
        })
    }

    fn member(base: Expr, n: &str) -> Expr {
        expr(ExprKind::Member { base: Box::new(base), name: n.to_string(), safe: false })
    }

    fn stmt(kind: StmtKind) -> Stmt {
        Stmt { kind, span: sp() }
    }

    fn block(stmts: Vec<Stmt>) -> Block {
        Block { stmts, span: sp() }
    }

    fn enum_decl(name: &str, variants: &[(&str, usize)]) -> Stmt {
        let variants = variants
            .iter()
            .map(|(n, arity)| VariantDecl {
                name: n.to_string(),
                payload: if *arity == 0 {
                    None
                } else {
                    Some(Payload::Positional(vec![ty_named("Int", false); *arity]))
                },
                doc: None,
                span: sp(),
            })
            .collect();
        stmt(StmtKind::Enum(EnumDecl { name: name.to_string(), variants, doc: None, span: sp() }))
    }

    fn variant_arm(name: &str, binds: &[&str], body: Expr) -> MatchArm {
        MatchArm {
            pattern: Pattern {
                kind: PatternKind::Variant {
                    enum_name: None,
                    name: name.to_string(),
                    subs: binds
                        .iter()
                        .map(|b| Pattern {
                            kind: PatternKind::Binding(b.to_string()),
                            span: sp(),
                        })
                        .collect(),
                },
                span: sp(),
            },
            guard: None,
            body: block(vec![stmt(StmtKind::Expr(body))]),
            span: sp(),
        }
    }

    fn wildcard_arm(body: Expr) -> MatchArm {
        MatchArm {
            pattern: Pattern { kind: PatternKind::Wildcard, span: sp() },
            guard: None,
            body: block(vec![stmt(StmtKind::Expr(body))]),
            span: sp(),
        }
    }

    fn match_expr(scrutinee: Expr, arms: Vec<MatchArm>) -> Expr {
        expr(ExprKind::Match(MatchExpr { scrutinee: Box::new(scrutinee), arms }))
    }

    /// `fn <name>(<param>: <ty>) returns <ret?>: <body>`
    fn fn_one_param(
        fname: &str,
        pname: &str,
        pty: Type,
        returns: Option<Type>,
        body: Vec<Stmt>,
    ) -> Stmt {
        stmt(StmtKind::Fn(FnDecl {
            name: fname.to_string(),
            params: vec![Param::Named { name: pname.to_string(), ty: pty, default: None }],
            returns,
            body: block(body),
            doc: None,
            span: sp(),
        }))
    }

    fn program(stmts: Vec<Stmt>) -> Program {
        Program { stmts }
    }

    fn check_errs(p: &Program) -> Vec<String> {
        match check(p) {
            Ok(()) => vec![],
            Err(ds) => ds.into_iter().map(|d| d.message).collect(),
        }
    }

    // ----------------------- Exhaustiveness tests --------------------------

    /// Model the DoD showcase: enum Expr { Num, Add, Mul, Div } + fn eval.
    /// Full match → Ok.
    #[test]
    fn exhaustive_full_match_ok() {
        let expr_enum =
            enum_decl("Expr", &[("Num", 1), ("Add", 2), ("Mul", 2), ("Div", 2)]);
        let m = match_expr(
            name("e"),
            vec![
                variant_arm("Num", &["n"], name("n")),
                variant_arm("Add", &["a", "b"], name("a")),
                variant_arm("Mul", &["a", "b"], name("a")),
                variant_arm("Div", &["a", "b"], name("a")),
            ],
        );
        let eval = fn_one_param(
            "eval",
            "e",
            ty_named("Expr", false),
            Some(ty_named("Float", false)),
            vec![stmt(StmtKind::Return(Some(m)))],
        );
        let p = program(vec![expr_enum, eval]);
        assert_eq!(check_errs(&p), Vec::<String>::new());
    }

    /// Removing the `Div` arm is a compile-time exhaustiveness error naming Div.
    #[test]
    fn exhaustive_missing_div_errors() {
        let expr_enum =
            enum_decl("Expr", &[("Num", 1), ("Add", 2), ("Mul", 2), ("Div", 2)]);
        let m = match_expr(
            name("e"),
            vec![
                variant_arm("Num", &["n"], name("n")),
                variant_arm("Add", &["a", "b"], name("a")),
                variant_arm("Mul", &["a", "b"], name("a")),
                // Div arm removed.
            ],
        );
        let eval = fn_one_param(
            "eval",
            "e",
            ty_named("Expr", false),
            Some(ty_named("Float", false)),
            vec![stmt(StmtKind::Return(Some(m)))],
        );
        let p = program(vec![expr_enum, eval]);
        let errs = check_errs(&p);
        assert_eq!(errs.len(), 1, "expected exactly one error, got {:?}", errs);
        assert!(errs[0].contains("Div"), "error should name Div: {}", errs[0]);
        assert!(errs[0].contains("Expr"), "error should name the enum: {}", errs[0]);
    }

    /// A `_` catch-all covers the missing variant → Ok.
    #[test]
    fn exhaustive_wildcard_covers_missing_ok() {
        let expr_enum =
            enum_decl("Expr", &[("Num", 1), ("Add", 2), ("Mul", 2), ("Div", 2)]);
        let m = match_expr(
            name("e"),
            vec![
                variant_arm("Num", &["n"], name("n")),
                wildcard_arm(name("n")),
            ],
        );
        let eval = fn_one_param(
            "eval",
            "e",
            ty_named("Expr", false),
            Some(ty_named("Float", false)),
            vec![stmt(StmtKind::Return(Some(m)))],
        );
        let p = program(vec![expr_enum, eval]);
        assert_eq!(check_errs(&p), Vec::<String>::new());
    }

    /// A bare `NAME` binding pattern is also a catch-all → Ok.
    #[test]
    fn exhaustive_binding_pattern_covers_ok() {
        let expr_enum = enum_decl("Color", &[("Red", 0), ("Green", 0), ("Blue", 0)]);
        let other_arm = MatchArm {
            pattern: Pattern { kind: PatternKind::Binding("other".to_string()), span: sp() },
            guard: None,
            body: block(vec![stmt(StmtKind::Expr(name("other")))]),
            span: sp(),
        };
        let m = match_expr(
            name("c"),
            vec![variant_arm("Red", &[], name("c")), other_arm],
        );
        let f = fn_one_param(
            "f",
            "c",
            ty_named("Color", false),
            None,
            vec![stmt(StmtKind::Expr(m))],
        );
        let p = program(vec![expr_enum, f]);
        assert_eq!(check_errs(&p), Vec::<String>::new());
    }

    /// A match over a non-enum / unresolvable scrutinee must NOT error.
    #[test]
    fn exhaustive_unresolvable_scrutinee_no_error() {
        // `x` is annotated Int (not an enum). A match over it must not be
        // flagged even though arms are literal patterns only.
        let m = match_expr(
            name("x"),
            vec![MatchArm {
                pattern: Pattern {
                    kind: PatternKind::Literal(LitPattern::Int(BigInt::from(0))),
                    span: sp(),
                },
                guard: None,
                body: block(vec![stmt(StmtKind::Expr(name("x")))]),
                span: sp(),
            }],
        );
        let f = fn_one_param(
            "f",
            "x",
            ty_named("Int", false),
            None,
            vec![stmt(StmtKind::Expr(m))],
        );
        let p = program(vec![f]);
        assert_eq!(check_errs(&p), Vec::<String>::new());
    }

    /// A match whose scrutinee is a construction call resolves to that enum.
    #[test]
    fn exhaustive_construction_scrutinee_resolves() {
        let color = enum_decl("Color", &[("Red", 0), ("Green", 0), ("Blue", 0)]);
        // match Red(): Red(): ... — missing Green and Blue → error.
        let m = match_expr(
            call(name("Red"), vec![]),
            vec![variant_arm("Red", &[], name("x"))],
        );
        let main = stmt(StmtKind::Fn(FnDecl {
            name: "main".to_string(),
            params: vec![],
            returns: None,
            body: block(vec![stmt(StmtKind::Expr(m))]),
            doc: None,
            span: sp(),
        }));
        let p = program(vec![color, main]);
        let errs = check_errs(&p);
        assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
        assert!(errs[0].contains("Green") && errs[0].contains("Blue"), "{}", errs[0]);
    }

    /// Variant pattern arity mismatch is flagged.
    #[test]
    fn exhaustive_arity_mismatch_flagged() {
        let expr_enum = enum_decl("Pair", &[("Both", 2)]);
        // Both(x) — arity 1 vs declared 2.
        let m = match_expr(
            name("e"),
            vec![variant_arm("Both", &["x"], name("x"))],
        );
        let f = fn_one_param(
            "f",
            "e",
            ty_named("Pair", false),
            None,
            vec![stmt(StmtKind::Expr(m))],
        );
        let p = program(vec![expr_enum, f]);
        let errs = check_errs(&p);
        assert!(errs.iter().any(|e| e.contains("field")), "expected arity error: {:?}", errs);
    }

    // ----------------------- Null-narrowing tests --------------------------

    /// `fn f(x: T?)` doing `x.field` directly → error.
    #[test]
    fn null_direct_member_errors() {
        let body = vec![stmt(StmtKind::Expr(member(name("x"), "field")))];
        let f = fn_one_param("f", "x", ty_named("Thing", true), None, body);
        let p = program(vec![f]);
        let errs = check_errs(&p);
        assert_eq!(errs.len(), 1, "expected one null error, got {:?}", errs);
        assert!(errs[0].contains("`x`"), "{}", errs[0]);
    }

    /// The same wrapped in `if x is not null:` then `x.field` → Ok.
    #[test]
    fn null_narrowed_member_ok() {
        let guard = expr(ExprKind::Binary {
            op: BinOp::IsNot,
            lhs: Box::new(name("x")),
            rhs: Box::new(expr(ExprKind::Null)),
        });
        let then_body = block(vec![stmt(StmtKind::Expr(member(name("x"), "field")))]);
        let if_stmt = stmt(StmtKind::If(IfStmt {
            arms: vec![(guard, then_body)],
            else_body: None,
        }));
        let f = fn_one_param("f", "x", ty_named("Thing", true), None, vec![if_stmt]);
        let p = program(vec![f]);
        assert_eq!(check_errs(&p), Vec::<String>::new());
    }

    /// `x.or_else(d)` usage of a nullable x → Ok (it's a valid use).
    #[test]
    fn null_or_else_ok() {
        // x.or_else(0).field — the or_else result is non-null, so .field is fine.
        let or_else = call(member(name("x"), "or_else"), vec![expr(ExprKind::Int(BigInt::from(0)))]);
        let use_expr = member(or_else, "field");
        let body = vec![stmt(StmtKind::Expr(use_expr))];
        let f = fn_one_param("f", "x", ty_named("Thing", true), None, body);
        let p = program(vec![f]);
        assert_eq!(check_errs(&p), Vec::<String>::new());
    }

    /// `x.or_else(d)` on its own (no further use) is also fine.
    #[test]
    fn null_or_else_bare_ok() {
        let or_else =
            call(member(name("x"), "or_else"), vec![expr(ExprKind::Int(BigInt::from(0)))]);
        let body = vec![stmt(StmtKind::Expr(or_else))];
        let f = fn_one_param("f", "x", ty_named("Thing", true), None, body);
        let p = program(vec![f]);
        assert_eq!(check_errs(&p), Vec::<String>::new());
    }

    /// A non-nullable param used as a member is fine.
    #[test]
    fn null_nonnullable_member_ok() {
        let body = vec![stmt(StmtKind::Expr(member(name("x"), "field")))];
        let f = fn_one_param("f", "x", ty_named("Thing", false), None, body);
        let p = program(vec![f]);
        assert_eq!(check_errs(&p), Vec::<String>::new());
    }

    /// Nullable used in arithmetic → error.
    #[test]
    fn null_arithmetic_errors() {
        let add = expr(ExprKind::Binary {
            op: BinOp::Add,
            lhs: Box::new(name("x")),
            rhs: Box::new(expr(ExprKind::Int(BigInt::from(1)))),
        });
        let body = vec![stmt(StmtKind::Expr(add))];
        let f = fn_one_param("f", "x", ty_named("Int", true), None, body);
        let p = program(vec![f]);
        let errs = check_errs(&p);
        assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    }

    /// Comparing a nullable to null with `is not` is allowed (no error).
    #[test]
    fn null_compare_to_null_ok() {
        let cmp = expr(ExprKind::Binary {
            op: BinOp::IsNot,
            lhs: Box::new(name("x")),
            rhs: Box::new(expr(ExprKind::Null)),
        });
        let body = vec![stmt(StmtKind::Expr(cmp))];
        let f = fn_one_param("f", "x", ty_named("Int", true), None, body);
        let p = program(vec![f]);
        assert_eq!(check_errs(&p), Vec::<String>::new());
    }
}
