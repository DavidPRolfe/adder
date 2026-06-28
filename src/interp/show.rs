//! Default `Show` rendering and structural value equality / ordering.
//!
//! - [`show`] / [`format_float`] — the walk-the-value display used by `print`
//!   and string interpolation.
//! - [`values_equal`] / [`lit_matches`] — structural `==` (and `is`/`is not`)
//!   and literal-pattern matching.
//! - [`compare_values`] — structural ordering for `sorted`/`min`/`max`.

use std::cmp::Ordering;

use super::*;

/// Structural ordering over comparable scalars (`Int`, `Float`, `String`,
/// `Bool`) and, for M3, user struct/enum types that opted in with `derive Ord`
/// (spec §7.1). Comparison is only defined within a single type; comparing
/// across types — or comparing a non-scalar that did **not** derive `Ord` — is a
/// runtime error (used by `sorted`/`sort`/`min`/`max` and the comparison
/// operators).
///
/// Ordering of a derived type is **lexicographic by declaration order**: a
/// struct compares field-by-field in declaration order; an enum compares by
/// variant declaration order first, then payload-by-payload within a variant.
pub(crate) fn compare_values(
    a: &Value,
    b: &Value,
    reg: &Registry,
    span: Span,
) -> Result<Ordering, Diagnostic> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).ok_or_else(|| {
            Diagnostic::runtime("cannot order NaN Float values".to_string(), span)
        }),
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        // M3: `derive Ord` struct/enum types — lexicographic by declaration order.
        (Value::Struct(x), Value::Struct(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            if x.type_name != y.type_name {
                return Err(incomparable(a, b, span));
            }
            if !reg.ord_types.contains(&x.type_name) {
                return Err(no_ord(&x.type_name, span));
            }
            for f in &x.field_order {
                let (xv, yv) = (&x.fields[f], &y.fields[f]);
                match compare_values(xv, yv, reg, span)? {
                    Ordering::Equal => continue,
                    ord => return Ok(ord),
                }
            }
            Ok(Ordering::Equal)
        }
        (Value::Enum(x), Value::Enum(y)) => {
            if x.enum_name != y.enum_name {
                return Err(incomparable(a, b, span));
            }
            if !reg.ord_types.contains(&x.enum_name) {
                return Err(no_ord(&x.enum_name, span));
            }
            let xi = variant_index(reg, &x.enum_name, &x.variant);
            let yi = variant_index(reg, &y.enum_name, &y.variant);
            if xi != yi {
                return Ok(xi.cmp(&yi));
            }
            for (xv, yv) in x.payload.iter().zip(y.payload.iter()) {
                match compare_values(xv, yv, reg, span)? {
                    Ordering::Equal => continue,
                    ord => return Ok(ord),
                }
            }
            Ok(Ordering::Equal)
        }
        (x, y) => Err(incomparable(x, y, span)),
    }
}

/// Declaration index of a variant within its enum (for `derive Ord` ordering).
/// Unknown variants sort last (defensive; the checker rejects bad variants).
fn variant_index(reg: &Registry, enum_name: &str, variant: &str) -> usize {
    reg.enums
        .get(enum_name)
        .and_then(|e| e.variants.iter().position(|v| v.name == variant))
        .unwrap_or(usize::MAX)
}

fn incomparable(a: &Value, b: &Value, span: Span) -> Diagnostic {
    Diagnostic::runtime(
        format!("cannot order {} against {}", type_name(a), type_name(b)),
        span,
    )
}

fn no_ord(type_name: &str, span: Span) -> Diagnostic {
    Diagnostic::runtime(
        format!("type `{type_name}` is not orderable; add `derive Ord` to compare or sort it"),
        span,
    )
}

/// Does a literal pattern match a runtime value (by value equality)?
pub(crate) fn lit_matches(lit: &LitPattern, val: &Value) -> bool {
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
pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
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
        // Tuples: element-wise structural equality (correct and final).
        (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| values_equal(p, q))
        }
        // Maps are order-insensitive: equal iff every key in `x` is present in
        // `y` with a structurally-equal value (and the sizes match). Keys are
        // matched structurally via linear scan — no hashing, consistent with the
        // `Vec`-backed store.
        (Value::Map(x), Value::Map(y)) => {
            let xs = x.borrow();
            let ys = y.borrow();
            xs.len() == ys.len()
                && xs.iter().all(|(kx, vx)| {
                    ys.iter()
                        .find(|(ky, _)| values_equal(kx, ky))
                        .map_or(false, |(_, vy)| values_equal(vx, vy))
                })
        }
        // Sets are order-insensitive: equal iff same size and every element of
        // `x` appears in `y` (each side is already deduplicated, so this is a
        // mutual-containment check).
        (Value::Set(x), Value::Set(y)) => {
            let xs = x.borrow();
            let ys = y.borrow();
            xs.len() == ys.len()
                && xs.iter().all(|p| ys.iter().any(|q| values_equal(p, q)))
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
pub(crate) fn show(v: &Value) -> String {
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
        // Tuples render as `(a, b, …)` (final).
        Value::Tuple(items) => {
            let inner: Vec<String> = items.iter().map(show).collect();
            format!("({})", inner.join(", "))
        }
        // A `Map` renders `{k: v, k2: v2}`; an empty map is `{}` — matching the
        // surface literal (the empty `{}` is a `Map`).
        Value::Map(pairs) => {
            let inner: Vec<String> = pairs
                .borrow()
                .iter()
                .map(|(k, v)| format!("{}: {}", show(k), show(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        // A `Set` renders `{a, b}`; the *empty* set renders as `Set()` rather
        // than `{}` (which is the empty `Map`), matching the surface literal.
        Value::Set(items) => {
            let items = items.borrow();
            if items.is_empty() {
                "Set()".to_string()
            } else {
                let inner: Vec<String> = items.iter().map(show).collect();
                format!("{{{}}}", inner.join(", "))
            }
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
pub(crate) fn format_float(f: f64) -> String {
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
