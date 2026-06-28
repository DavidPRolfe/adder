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

use std::collections::HashMap;

use crate::ast::{
    Arg, BaseType, Binder, EnumDecl, Expr, ExprKind, IfStmt, ImplDecl, Payload, Program, Stmt,
    StmtKind, Type,
};
use crate::error::Diagnostic;

mod exhaustiveness;
mod null_narrowing;
#[cfg(test)]
mod tests;

use exhaustiveness::Exhaustiveness;
use null_narrowing::NullNarrowing;

/// Run the compile-time checks (exhaustiveness + null-narrowing) over `program`.
pub fn check(program: &Program) -> Result<(), Vec<Diagnostic>> {
    let mut diags = Vec::new();

    // Shared, program-wide enum information, gathered once.
    let enums = EnumInfo::gather(program);

    // Check 1 — match exhaustiveness.
    let mut ex = Exhaustiveness { enums: &enums, diags: &mut diags };
    ex.run(program);

    // Check 2 — null-narrowing.
    let mut nn = NullNarrowing { diags: &mut diags };
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
pub(crate) struct EnumInfo {
    /// enum name → ordered list of (variant name, arity).
    ///
    /// `arity` is the number of payload fields (`0` for niladic). Variant order
    /// preserves declaration order so missing-variant messages read naturally.
    pub(crate) enums: HashMap<String, Vec<(String, usize)>>,
    /// variant name → enum name. Only populated for variants whose name is
    /// **unambiguous** across all enums; an entry is removed if a second enum
    /// reuses the same variant name (we can't pick one, so we stay silent).
    pub(crate) variant_to_enum: HashMap<String, String>,
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
    pub(crate) fn enum_name_of_type(&self, ty: &Type) -> Option<&str> {
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
// Shared free helpers (used by both analyses)
// ===========================================================================

/// The name(s) a [`Binder`] introduces — one for a single name, several for a
/// tuple destructure. Used to seed loop-binder scoping in both static passes.
pub(crate) fn binder_names(binder: &Binder) -> Vec<&String> {
    match binder {
        Binder::Name(n) => vec![n],
        Binder::Tuple(names) => names.iter().collect(),
    }
}

/// The expression carried by a call [`Arg`], discarding the (irrelevant-here)
/// name of a named argument. Both static passes recurse into the same sub-expr
/// regardless of whether the argument was positional or named.
pub(crate) fn arg_expr(arg: &Arg) -> &Expr {
    match arg {
        Arg::Positional(e) => e,
        Arg::Named { value, .. } => value,
    }
}

/// Apply `f` to each *direct* sub-expression of `e`, in evaluation/source order.
///
/// This is a purely **structural**, semantics-free walk: it imposes no per-pass
/// logic (no exhaustiveness, no null-narrowing) — it only enumerates a node's
/// immediate child expressions so the two passes can share their recursion for
/// the cases where they merely descend. Nodes that carry no sub-expressions
/// (leaves: literals, `Name`, `self`) yield nothing.
///
/// Note: it does **not** descend into `Match` arm bodies (those are statements,
/// which each pass scopes differently), so callers handle `Match` themselves.
pub(crate) fn walk_child_exprs(e: &Expr, f: &mut impl FnMut(&Expr)) {
    match &e.kind {
        ExprKind::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        ExprKind::Unary { operand, .. } => f(operand),
        ExprKind::Ternary { then, cond, otherwise } => {
            f(then);
            f(cond);
            f(otherwise);
        }
        ExprKind::Call { callee, args } => {
            f(callee);
            for a in args {
                f(arg_expr(a));
            }
        }
        ExprKind::Index { base, index } => {
            f(base);
            f(index);
        }
        ExprKind::Member { base, .. } => f(base),
        ExprKind::List(items) => {
            for it in items {
                f(it);
            }
        }
        ExprKind::Lambda(l) => f(&l.body),
        ExprKind::Str(s) => {
            for part in &s.parts {
                if let crate::ast::StrSeg::Expr(inner) = part {
                    f(inner);
                }
            }
        }
        ExprKind::Map(pairs) => {
            for (k, v) in pairs {
                f(k);
                f(v);
            }
        }
        ExprKind::Set(items) | ExprKind::Tuple(items) => {
            for it in items {
                f(it);
            }
        }
        ExprKind::Comprehension(c) => {
            use crate::ast::ComprehensionOutput::*;
            match &c.output {
                List(out) | Set(out) => f(out),
                Map { key, value } => {
                    f(key);
                    f(value);
                }
            }
            f(&c.iter);
            if let Some(cond) = &c.cond {
                f(cond);
            }
        }
        // `Match` is intentionally not descended here (see doc comment), and the
        // remaining leaves carry no sub-expressions.
        ExprKind::Match(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Null
        | ExprKind::Name(_)
        | ExprKind::SelfExpr => {}
    }
}
