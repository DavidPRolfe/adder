//! Statement execution and l-value assignment for the tree-walker.

use num_traits::{Signed, ToPrimitive};

use crate::ast::{
    Block, Expr, ExprKind, ForStmt, IfStmt, Stmt, StmtKind, Target, TargetSeg, WhileStmt,
};
use crate::error::Diagnostic;
use crate::token::Span;

use super::*;

impl<'a> Interp<'a> {
    // -----------------------------------------------------------------------
    // Statement execution
    // -----------------------------------------------------------------------

    /// Execute a statement in `env`, returning a `Flow` signal. For an
    /// expression statement, `Flow::Normal` carries the expression's value
    /// (this is how a block / fn body / match arm produces an implicit value).
    pub(crate) fn exec_stmt(&mut self, stmt: &Stmt, env: &Env) -> FlowResult {
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
            StmtKind::Struct(_) | StmtKind::Enum(_) | StmtKind::Impl(_) | StmtKind::Trait(_) => {
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
    pub(crate) fn exec_stmts(&mut self, stmts: &[Stmt], env: &Env) -> FlowResult {
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

    /// Materialize a `for`-iterable into a vector of element values. Supports
    /// ranges (already evaluated to lists) and lists.
    pub(crate) fn iterable_items(&self, v: &Value, span: Span) -> Result<Vec<Value>, Diagnostic> {
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
    pub(crate) fn eval_bool_cond(&mut self, cond: &Expr, env: &Env) -> Result<bool, Diagnostic> {
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
    pub(crate) fn list_index(&self, idx: &Value, len: usize, span: Span) -> Result<usize, Diagnostic> {
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
}
