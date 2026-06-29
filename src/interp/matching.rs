//! `match` execution and pattern matching for the tree-walker.

use crate::ast::{MatchExpr, Pattern, PatternKind};
use crate::error::Diagnostic;
use crate::token::Span;

use super::*;

impl<'a> Interp<'a> {
    // -----------------------------------------------------------------------
    // Match
    // -----------------------------------------------------------------------

    /// Run a `match` and return the chosen arm body's [`Flow`] verbatim: a
    /// `return`/`break`/`continue` inside the arm propagates out unchanged, so a
    /// `match` in statement position behaves like the block it wraps. The
    /// expression form ([`eval_match`]) collapses that flow into a value.
    pub(crate) fn exec_match(&mut self, m: &MatchExpr, span: Span, env: &Env) -> FlowResult {
        let scrutinee = self.eval(&m.scrutinee, env)?;
        for arm in &m.arms {
            let arm_scope = Scope::child(env);
            if !self.try_match(&arm.pattern, &scrutinee, &arm_scope)? {
                continue;
            }
            // A guard (`pattern if cond:`) is evaluated in the arm scope, so it
            // sees the pattern's bindings. It must be `Bool` (no truthiness); a
            // false guard falls through to the next arm.
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
    pub(crate) fn eval_match(&mut self, m: &MatchExpr, span: Span, env: &Env) -> EvalResult {
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
                // Sub-patterns are full patterns (recursive); the flat
                // cases (`_`, name, `null`, literal) recurse here unchanged.
                for (sub, payload_v) in subs.iter().zip(inst.payload.iter()) {
                    if !self.try_match(sub, payload_v, scope)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            // An or-pattern matches if ANY alternative matches; the first
            // matching alternative's bindings stick. Alternatives are tried
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
            // element-wise. Nested patterns thus destructure deeply.
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
