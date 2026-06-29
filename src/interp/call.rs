//! Calls, construction, and method dispatch for the tree-walker.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{Arg, Expr, ExprKind, Param, Payload};
use crate::error::Diagnostic;
use crate::token::Span;

use super::*;

impl<'a> Interp<'a> {
    // -----------------------------------------------------------------------
    // Calls / construction / methods
    // -----------------------------------------------------------------------

    pub(crate) fn eval_call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> EvalResult {
        // A member callee `base.name(args)` — qualified enum construction, the
        // `?.`/`.or_else` null sugar, or an ordinary method call.
        if let ExprKind::Member { base, name, safe } = &callee.kind {
            return self.eval_member_call(base, name, *safe, args, span, env);
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
        // A named `fn` supports named call args and default values;
        // bind directly from the raw `Arg`s so names/defaults are honoured.
        if let Value::Closure(c) = &callee_v {
            if let ClosureKind::Function(f) = &c.kind {
                let f = Rc::clone(f);
                let c = Rc::clone(c);
                let call_scope = Scope::child(&c.env);
                self.bind_call(&f.params, args, &call_scope, &f.name, span, env)?;
                let body = self.exec_stmts(&f.body.stmts, &call_scope);
                return self.finish_body(body, "`break`/`continue` outside a loop", span);
            }
        }
        // Lambdas / builtins take positional args only (no names/defaults).
        let arg_vals = self.eval_positional_args(args, span, env)?;
        self.apply(callee_v, arg_vals, span)
    }

    /// A call whose callee is a member expression `base.name(args)`: qualified
    /// enum-variant construction (`Enum.Variant(...)`), the built-in null sugar
    /// (`?.` safe-call short-circuit and `.or_else(default)`), or an ordinary
    /// method call `recv.name(args)`.
    fn eval_member_call(
        &mut self,
        base: &Expr,
        name: &str,
        safe: bool,
        args: &[Arg],
        span: Span,
        env: &Env,
    ) -> EvalResult {
        // Qualified enum-variant construction: `Enum.Variant(args)`. Only when
        // the base names a known enum that isn't shadowed by a value. `?.` never
        // qualifies an enum (its receiver is a value), so this is limited to the
        // plain-`.` case.
        if let ExprKind::Name(enum_name) = &base.kind {
            if !safe
                && env_get(env, enum_name).is_none()
                && self.registry.enums.contains_key(enum_name)
            {
                return self.construct_variant(enum_name, name, args, span, env);
            }
        }

        // Otherwise it is a method call `recv.method(args)`.
        let recv = self.eval(base, env)?;

        // `?.method(...)` safe-call: a `null` receiver short-circuits the whole
        // call to `null` — the args are never evaluated and the method never
        // runs (so a chain `a?.b()?.c()` propagates `null`).
        if safe && matches!(recv, Value::Null) {
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

        self.call_method(recv, name, args, span, env)
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
    pub(crate) fn apply(&mut self, callee: Value, args: Vec<Value>, span: Span) -> EvalResult {
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
    pub(crate) fn call_closure(
        &mut self,
        closure: &Closure,
        args: Vec<Value>,
        span: Span,
    ) -> EvalResult {
        let call_scope = Scope::child(&closure.env);
        match &closure.kind {
            ClosureKind::Function(f) => {
                self.bind_params(&f.params, &args, &call_scope, &f.name, span)?;
                let body = self.exec_stmts(&f.body.stmts, &call_scope);
                self.finish_body(body, "`break`/`continue` outside a loop", span)
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
                // A lambda body is a single expression. A `try` inside it
                // unwinds to here, the lambda's own boundary — via the same
                // single interception `finish_body` uses.
                match self.eval(&l.body, &call_scope) {
                    Err(d) => self.intercept_propagation(d),
                    ok => ok,
                }
            }
        }
    }

    /// Bind already-evaluated **positional** args to params (the `self` receiver,
    /// if any, is pre-bound by the caller). Trailing params that have a default
    /// value may be omitted; their defaults are evaluated in `scope`.
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
    /// **named arguments** and **default values**. Positional args
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
    /// the built-in method table.
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
        // values honoured).
        let method_scope = Scope::child(&root_of(env));
        env_define(&method_scope, "self", recv, false);
        self.bind_call(&method.params, args, &method_scope, &method.name, span, env)?;

        let body = self.exec_stmts(&method.body.stmts, &method_scope);
        self.finish_body(body, "`break`/`continue` outside a loop", span)
    }

    /// Evaluate `try expr` (spec §9). `expr` must produce a `Result`: `Ok(v)`
    /// yields `v`; `Err(..)` is stashed in `self.propagating` and unwound via a
    /// sentinel error to the nearest call boundary ([`Self::finish_body`]), which
    /// returns it as the enclosing function's value.
    pub(crate) fn eval_try(&mut self, inner: &Expr, span: Span, env: &Env) -> EvalResult {
        let v = self.eval(inner, env)?;
        match &v {
            Value::Enum(e) if e.enum_name == "Result" => match e.variant.as_str() {
                "Ok" => Ok(e.payload.first().cloned().unwrap_or(Value::Unit)),
                "Err" => {
                    self.propagating = Some(v.clone());
                    Err(try_unwind_sentinel(span))
                }
                _ => Err(Diagnostic::runtime(
                    "`try` expects a `Result` (`Ok`/`Err`)".to_string(),
                    span,
                )),
            },
            _ => Err(Diagnostic::runtime(
                "`try` can only be applied to a `Result` value".to_string(),
                span,
            )),
        }
    }

    /// Collapse a function/method/lambda body result into the call's value,
    /// honoring an in-flight `try` propagation: a body that errored while
    /// `self.propagating` is set is a `try` unwind, so the propagated `Err`
    /// becomes this call's return value rather than a runtime error.
    fn finish_body(&mut self, body: FlowResult, message: &str, span: Span) -> EvalResult {
        match body {
            Ok(flow) => finish_call(flow, message, span),
            Err(d) => self.intercept_propagation(d),
        }
    }

    /// The single place a `try` unwind is turned back into a value at a call
    /// boundary: if a `try` stashed an `Err` in `self.propagating`, consume it
    /// as this call's value; otherwise `d` is a genuine runtime error. Every
    /// call boundary — function/method bodies via [`Self::finish_body`] and the
    /// lambda path in [`Self::call_closure`] — routes its `Err` through here, so
    /// the "intercept the propagation sentinel" invariant lives in exactly one
    /// spot.
    fn intercept_propagation(&mut self, d: Diagnostic) -> EvalResult {
        match self.propagating.take() {
            Some(v) => Ok(v),
            None => Err(d),
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

    pub(crate) fn construct_variant(
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
}
