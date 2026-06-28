//! Built-in (prelude) functions and the built-in **method table** for non-user
//! receiver types — `List`, `Str`, `Map`, `Set`, `Tuple`, and range-lists
//! (M2 Wave 1-A). Houses the eager iterator pipeline (`map`/`filter`/`fold`/…),
//! the `Map`/`Set` methods, and their argument/coercion/numeric helpers.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive};

use super::*;

impl<'a> Interp<'a> {
    pub(crate) fn call_builtin(&mut self, b: Builtin, args: Vec<Value>, span: Span) -> EvalResult {
        match b {
            Builtin::Print => {
                let rendered: Vec<String> = args.iter().map(show).collect();
                writeln!(self.out, "{}", rendered.join(" ")).map_err(|e| {
                    Diagnostic::runtime(format!("failed to write output: {}", e), span)
                })?;
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
            // M3: the prelude `Result` constructors (spec §9). Each takes exactly
            // one positional payload and builds the corresponding variant.
            Builtin::Ok => self.construct_result_variant("Ok", args, span),
            Builtin::Err => self.construct_result_variant("Err", args, span),
        }
    }

    /// Build a `Result.Ok(v)` / `Result.Err(v)` instance from a single argument
    /// (M3; spec §9). A wrong argument count is a runtime error.
    fn construct_result_variant(
        &self,
        variant: &str,
        args: Vec<Value>,
        span: Span,
    ) -> EvalResult {
        if args.len() != 1 {
            return Err(Diagnostic::runtime(
                format!("`{}` takes exactly one argument", variant),
                span,
            ));
        }
        Ok(Value::Enum(Rc::new(EnumInstance {
            enum_name: "Result".to_string(),
            variant: variant.to_string(),
            payload: args,
            payload_names: Vec::new(),
        })))
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
    pub(crate) fn call_builtin_method(
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
    pub(crate) fn list_method(
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
                extreme(&items.borrow(), &self.registry, Ordering::Less, span)
            }
            "max" => {
                arg0(name, &args, span)?;
                extreme(&items.borrow(), &self.registry, Ordering::Greater, span)
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
                sort_values(&mut out, &self.registry, span)?;
                Ok(list_value(out))
            }
            // In-place sort (M3; spec §7.1) — mutates the receiver, returns unit.
            "sort" => {
                arg0(name, &args, span)?;
                let mut out = items.borrow().clone();
                sort_values(&mut out, &self.registry, span)?;
                *items.borrow_mut() = out;
                Ok(Value::Unit)
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
    pub(crate) fn map_method(
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
    pub(crate) fn set_method(
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
    pub(crate) fn str_method(s: &str, name: &str, args: &[Value], span: Span) -> EvalResult {
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
}

// ===========================================================================
// Free helpers for the built-in machinery
// ===========================================================================

/// Insert a key/value into an insertion-ordered map vector, deduplicating by
/// structural key equality (a re-inserted key overwrites its value in place,
/// preserving first-seen order). Mirrors the `Map` literal / comprehension rule.
pub(crate) fn map_insert(entries: &mut Vec<(Value, Value)>, key: Value, value: Value) {
    if let Some(slot) = entries.iter_mut().find(|(k, _)| values_equal(k, &key)) {
        slot.1 = value;
    } else {
        entries.push((key, value));
    }
}

/// Insert an element into an insertion-ordered set vector, deduplicating by
/// structural equality (a duplicate is dropped, keeping the first occurrence).
pub(crate) fn set_insert(elems: &mut Vec<Value>, value: Value) {
    if !elems.iter().any(|e| values_equal(e, &value)) {
        elems.push(value);
    }
}

// ---- Built-in-method argument arity (M2 Wave 1-A) ------------------------

/// Require a built-in method to receive **no** arguments.
pub(crate) fn arg0(name: &str, args: &[Value], span: Span) -> Result<(), Diagnostic> {
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
pub(crate) fn arg1(name: &str, args: Vec<Value>, span: Span) -> Result<Value, Diagnostic> {
    if args.len() != 1 {
        return Err(Diagnostic::runtime(
            format!("`{}` takes exactly one argument, got {}", name, args.len()),
            span,
        ));
    }
    Ok(args.into_iter().next().unwrap())
}

/// Require **exactly two** arguments, returning them in order.
pub(crate) fn arg2(name: &str, args: Vec<Value>, span: Span) -> Result<(Value, Value), Diagnostic> {
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
pub(crate) fn list_value(items: Vec<Value>) -> Value {
    Value::List(Rc::new(RefCell::new(items)))
}

/// Wrap a `Vec<Value>` as a fresh, reference-shared `Value::Set` (caller must
/// have already deduplicated).
pub(crate) fn set_value(items: Vec<Value>) -> Value {
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
fn extreme(items: &[Value], reg: &Registry, want: Ordering, span: Span) -> EvalResult {
    let mut iter = items.iter();
    let label = if want == Ordering::Less { "min" } else { "max" };
    let mut best = iter
        .next()
        .ok_or_else(|| {
            Diagnostic::runtime(format!("`{}` on an empty list has no result", label), span)
        })?
        .clone();
    for v in iter {
        if compare_values(v, &best, reg, span)? == want {
            best = v.clone();
        }
    }
    Ok(best)
}

/// Sort a slice of values ascending by structural ordering, surfacing the first
/// incomparable pair as a runtime error.
fn sort_values(items: &mut [Value], reg: &Registry, span: Span) -> Result<(), Diagnostic> {
    // `sort_by` can't carry a `Result`, so capture the first error out-of-band.
    let mut err: Option<Diagnostic> = None;
    items.sort_by(|a, b| match compare_values(a, b, reg, span) {
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
