//! Runtime data model for the interpreter: the [`Value`] enum and its
//! supporting types (`StructInstance`, `EnumInstance`, `Closure`, `Builtin`,
//! `Scope`, `Binding`, the `Env` alias).
//!
//! Pure data; no `Interp` methods live here. See [`super`] for the tree-walker.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use num_bigint::BigInt;

use crate::ast::{FnDecl, Lambda};

/// A runtime value (grammar §5 / scope "Values & types").
///
/// `Clone` is cheap for the heap-backed variants because they wrap `Rc`. The
/// reference-counted/interior-mutable shapes (`List`, `Struct`, `Closure`,
/// `Env`) give closures capture-by-reference and structs mutable fields.
///
/// `PartialEq` is intentionally *not* derived: the language's structural `==`
/// (and `is`/`is not`) is implemented separately by the `values_equal` free
/// function — e.g. `Closure`/`Builtin` equality is not meaningful and `Float`
/// follows IEEE rules. Compare values through `values_equal`, never a derived impl.
#[derive(Debug, Clone)]
pub enum Value {
    /// Arbitrary-precision integer.
    Int(BigInt),
    /// 64-bit float.
    Float(f64),
    /// Boolean.
    Bool(bool),
    /// String (already-rendered text; not interpolation parts).
    Str(String),
    /// A list, mutable and shared by reference.
    List(Rc<RefCell<Vec<Value>>>),
    /// A map (M2), insertion-ordered for stable `Show`. Stored as a `Vec` of
    /// key/value pairs rather than a hash map because keys are arbitrary
    /// (structurally-hashed) runtime values and insertion order must be
    /// preserved; mutable and shared by reference.
    /// Produced and consumed by map literals / comprehensions / built-in
    /// methods.
    Map(Rc<RefCell<Vec<(Value, Value)>>>),
    /// A set (M2), insertion-ordered, deduplicated by structural equality.
    /// Stored as a `Vec` for the same reasons as [`Value::Map`]; mutable and
    /// shared by reference.
    /// Produced and consumed by set literals / comprehensions / built-in
    /// methods.
    Set(Rc<RefCell<Vec<Value>>>),
    /// A tuple (M2): a fixed, immutable sequence of values. Shared by reference
    /// (the contents never mutate, so no `RefCell`).
    /// Produced and consumed by tuple literals / patterns.
    Tuple(Rc<Vec<Value>>),
    /// The unit value `()`.
    Unit,
    /// The `null` value.
    Null,
    /// A struct instance: its type name plus its (mutable) named fields.
    Struct(Rc<RefCell<StructInstance>>),
    /// An enum instance: its enum/variant plus payload values.
    Enum(Rc<EnumInstance>),
    /// A user closure capturing its defining environment by reference.
    Closure(Rc<Closure>),
    /// A built-in (prelude) function such as `print` / `panic`.
    Builtin(Builtin),
}

/// A struct instance's runtime data.
#[derive(Debug, Clone)]
pub struct StructInstance {
    /// The struct type's name.
    pub type_name: String,
    /// Field values keyed by field name (mutable).
    pub fields: HashMap<String, Value>,
    /// Field order, for stable `Show` rendering.
    pub field_order: Vec<String>,
}

/// An enum instance's runtime data.
#[derive(Debug, Clone)]
pub struct EnumInstance {
    /// The enum type's name.
    pub enum_name: String,
    /// The active variant's name.
    pub variant: String,
    /// Payload values. For named payloads, order matches the declaration; the
    /// interpreter agent may also track field names if needed for `Show`.
    pub payload: Vec<Value>,
    /// For named payloads, the field names (parallel to `payload`). Empty for
    /// positional or niladic variants.
    pub payload_names: Vec<String>,
}

/// A user-defined closure.
///
/// Holds the function/lambda body and the environment captured **by
/// reference** at creation time (closures see later mutations to captured
/// bindings).
#[derive(Debug)]
pub struct Closure {
    /// The closure's source. A named `fn` or an anonymous lambda.
    pub kind: ClosureKind,
    /// The captured defining environment (shared by reference).
    pub env: Env,
}

/// What a [`Closure`] wraps.
#[derive(Debug, Clone)]
pub enum ClosureKind {
    /// A declared function (carries params, returns, body).
    Function(Rc<FnDecl>),
    /// An anonymous lambda.
    Lambda(Rc<Lambda>),
}

/// A built-in prelude function, identified by tag. The interpreter dispatches
/// on the tag; new built-ins can be added without grammar changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// `print(args, …)` — writes the `Show` rendering of each argument.
    Print,
    /// `panic(msg)` — aborts with `msg`; never returns normally.
    Panic,
    /// `Set()` — constructs an empty set (the `{}` literal is an empty *map*,
    /// so the empty set needs its own spelling; see spec §3).
    Set,
    /// `Ok(value)` — constructs the prelude `Result.Ok` variant (M3; spec §9).
    Ok,
    /// `Err(value)` — constructs the prelude `Result.Err` variant (M3; spec §9).
    Err,
}

/// A lexical environment / scope, shared and mutable by reference.
///
/// `Rc<RefCell<Scope>>` so closures capture by reference and nested scopes can
/// chain to their parent. Walk `parent` to resolve a name; the innermost scope
/// holding it wins.
pub type Env = Rc<RefCell<Scope>>;

/// One scope frame: its bindings and an optional parent.
#[derive(Debug, Default)]
pub struct Scope {
    /// Name → (value, is_immutable). The `is_val` flag backs the runtime
    /// `val`-reassignment check.
    pub vars: HashMap<String, Binding>,
    /// Enclosing scope, if any.
    pub parent: Option<Env>,
}

/// A single binding slot in a [`Scope`].
#[derive(Debug, Clone)]
pub struct Binding {
    pub value: Value,
    /// `true` if bound with `val` (reassignment is a runtime error).
    pub is_val: bool,
}

impl Scope {
    /// A fresh empty root scope wrapped as an [`Env`].
    pub fn new_root() -> Env {
        Rc::new(RefCell::new(Scope::default()))
    }

    /// A fresh child scope whose parent is `parent`.
    pub fn child(parent: &Env) -> Env {
        Rc::new(RefCell::new(Scope {
            vars: HashMap::new(),
            parent: Some(Rc::clone(parent)),
        }))
    }
}
