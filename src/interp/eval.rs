//! Expression evaluation for the tree-walker.

use std::cell::RefCell;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::ast::{BinOp, Expr, ExprKind, StrSeg, UnOp};
use crate::error::Diagnostic;
use crate::token::Span;

use super::*;

impl<'a> Interp<'a> {
    // -----------------------------------------------------------------------
    // Expression evaluation
    // -----------------------------------------------------------------------

    pub(crate) fn eval(&mut self, expr: &Expr, env: &Env) -> EvalResult {
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
            // `try expr` (spec §9).
            ExprKind::Try(inner) => self.eval_try(inner, expr.span, env),
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
            // ----- collections / comprehensions -----
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
            // User types that opted into `derive Ord` compare structurally.
            (Value::Struct(_), Value::Struct(_)) | (Value::Enum(_), Value::Enum(_)) => {
                Some(compare_values(l, r, &self.registry, span)?)
            }
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
            // numeric story honest.
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
}
