// ===========================================================================
// Check 1 — Match exhaustiveness
// ===========================================================================

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Binder, Binding, Expr, ExprKind, FnDecl, IfStmt, MatchExpr, Param, Pattern, PatternKind,
    Program, Stmt, StmtKind,
};
use crate::error::Diagnostic;
use crate::token::Span;

use super::{binder_names, walk_child_exprs, EnumInfo};

pub(super) struct Exhaustiveness<'a> {
    pub(super) enums: &'a EnumInfo,
    pub(super) diags: &'a mut Vec<Diagnostic>,
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
    pub(super) fn run(&mut self, program: &Program) {
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
                match &b.binder {
                    // A single-name binding may resolve to an enum type.
                    Binder::Name(name) => {
                        let ty = self.resolve_binding_ty(b, scope);
                        scope.insert(name.clone(), ty);
                    }
                    // A tuple destructure binds several names of unknown type.
                    Binder::Tuple(names) => {
                        for n in names {
                            scope.insert(n.clone(), ResolvedTy::Unknown);
                        }
                    }
                }
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
                // The loop binder's name(s) are of unknown type to this pass.
                for n in binder_names(&fr.binder) {
                    inner.insert(n.clone(), ResolvedTy::Unknown);
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
            // Trait default methods have bodies that may contain matches.
            StmtKind::Trait(t) => {
                for m in &t.defaults {
                    self.check_fn(m);
                }
            }
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
    ///
    /// Only `match` needs per-node handling (the exhaustiveness check plus
    /// arm-body scoping); every other node is pure structural recursion, so it
    /// is delegated to the shared [`walk_child_exprs`]. The comprehension binder
    /// is scoped to the comprehension but is of unknown type to this lightweight
    /// pass, so we do not add it to `scope`.
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
            // Every other node — Binary/Unary/Ternary/Call/Index/Member/List/
            // Lambda/Str/Map/Set/Tuple/Comprehension — just recurses into its
            // children (and the leaves recurse into nothing).
            _ => walk_child_exprs(e, &mut |child| self.check_expr(child, scope)),
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
        // Clone the variant table so `self` is free for `&mut self` diagnostics
        // while we walk the arms (the borrow checker treats a method's `self` as
        // a whole, so we cannot hold a `&self.enums` borrow across `self.diags`).
        let variants: Vec<(String, usize)> = match self.enums.enums.get(&enum_name) {
            Some(v) => v.clone(),
            None => return,
        };

        let mut covered: HashSet<&str> = HashSet::new();
        let mut has_catch_all = false;

        for arm in &m.arms {
            // A *guarded* arm (`pattern if cond:`) may not fire, so it does not
            // contribute to exhaustiveness — skip its coverage entirely. So
            // a guard over the last uncovered variant still leaves the match
            // non-exhaustive; adding `_` (or an unguarded arm) fixes it.
            if arm.guard.is_some() {
                continue;
            }
            self.cover_pattern(
                &arm.pattern,
                &enum_name,
                &variants,
                &mut covered,
                &mut has_catch_all,
            );
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

    /// Record the enum-variant coverage of one (sub-)pattern against `variants`,
    /// emitting any best-effort arity / unknown-variant / wrong-enum diagnostics.
    ///
    /// Coverage stays **enum-variant-based**: a variant pattern covers
    /// its variant regardless of how its sub-patterns are shaped — no deep or
    /// cross-product reasoning. `_` and a bare binding cover everything
    /// (`has_catch_all`). An **or-pattern** contributes the coverage of *all* its
    /// alternatives (so `.A or .B` covers both). Tuple / null / literal patterns
    /// never cover an enum variant. The `'a` lifetime ties inserted names to the
    /// pattern tree (`m`), so `covered` can borrow them.
    fn cover_pattern<'p>(
        &mut self,
        pat: &'p Pattern,
        enum_name: &str,
        variants: &[(String, usize)],
        covered: &mut HashSet<&'p str>,
        has_catch_all: &mut bool,
    ) {
        match &pat.kind {
            PatternKind::Wildcard => *has_catch_all = true,
            PatternKind::Binding(_) => *has_catch_all = true, // bare NAME matches anything
            PatternKind::Variant { enum_name: qual, name, subs } => {
                // A qualified pattern must name the scrutinee's enum.
                if let Some(en) = qual {
                    if en != enum_name {
                        self.diags.push(Diagnostic::check(
                            format!(
                                "pattern matches enum `{}`, but the value is `{}`",
                                en, enum_name
                            ),
                            pat.span,
                        ));
                        return;
                    }
                }
                // Arity / unknown-variant diagnostics (cheap, best-effort). The
                // sub-patterns may themselves be nested patterns; they do
                // not affect exhaustiveness, so we do not recurse into them here.
                if let Some((_, arity)) = variants.iter().find(|(vn, _)| vn == name) {
                    if *arity != subs.len() {
                        self.diags.push(Diagnostic::check(
                            format!(
                                "variant `{}::{}` expects {} field(s), but the pattern binds {}",
                                enum_name, name, arity, subs.len()
                            ),
                            pat.span,
                        ));
                    }
                    covered.insert(name.as_str());
                } else {
                    self.diags.push(Diagnostic::check(
                        format!("unknown variant `{}` for enum `{}`", name, enum_name),
                        pat.span,
                    ));
                }
            }
            // An or-pattern covers the union of its alternatives' coverage, so
            // each alternative is walked recursively (`.A or .B` covers both; an
            // alternative that is a wildcard makes the whole arm a catch-all).
            PatternKind::Or(alts) => {
                for alt in alts {
                    self.cover_pattern(alt, enum_name, variants, covered, has_catch_all);
                }
            }
            // null / literal / tuple patterns never cover an enum variant.
            PatternKind::Null | PatternKind::Literal(_) | PatternKind::Tuple(_) => {}
        }
    }
}
