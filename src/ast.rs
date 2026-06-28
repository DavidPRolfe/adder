//! The **parser ↔ checks ↔ interpreter contract**: the full Milestone-1 AST.
//!
//! This is the most load-bearing file in the crate. The parser
//! ([`crate::parser::parse`]) produces a [`Program`]; the static checks
//! ([`crate::checks::check`]) and the interpreter ([`crate::interp::run`]) both
//! consume it. It is intended to be **complete** for M1 — downstream agents
//! should not need to modify it.
//!
//! It mirrors grammar §2–§7. Where the grammar leaves something *semantic*
//! (e.g. whether a call is a function call or a struct construction, whether a
//! named `arg` is valid, or which trailing expression is a block's "value"),
//! the AST records the surface shape only and a doc comment notes that the
//! distinction is **resolved later** by checks/interp.
//!
//! Spans are attached to the nodes errors point at: statements, expressions,
//! patterns, parameters, and declaration headers.

use num_bigint::BigInt;

use crate::token::Span;

// ===========================================================================
// Program
// ===========================================================================

/// A whole source file: a sequence of top-level statements (grammar §2).
///
/// The runtime executes these in order, then calls a zero-arg `main()` if one
/// was declared (entry-point rule; see [`crate::interp`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

// ===========================================================================
// Statements (§4)
// ===========================================================================

/// A statement node: a [`StmtKind`] plus its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

/// Every statement form — simple (§4.1) and compound (§4.2–§4.6).
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    // ----- Simple statements (§4.1) -----
    /// A binding: `val x = e`, `x: T = e`, or `x = e` (first occurrence).
    /// See [`Binding`] for the immutable/mutable/typed distinction.
    Binding(Binding),

    /// Assignment to an existing l-value: `target = e` (grammar §4.1).
    /// Reassigning a `val` is a **runtime** error owned by [`crate::interp`].
    Assign(Assign),

    /// `return [expr]` (grammar §4.1).
    Return(Option<Expr>),

    /// `break`.
    Break,

    /// `continue`.
    Continue,

    /// An expression used as a statement (e.g. a call, `print(...)`).
    Expr(Expr),

    // ----- Compound statements (§4.2) -----
    /// `if` / `elif` / `else` (grammar §4.2).
    If(IfStmt),

    /// `while cond: suite` (grammar §4.2).
    While(WhileStmt),

    /// `for NAME in iter: suite` (grammar §4.2). Single `NAME` binder only.
    For(ForStmt),

    /// A function declaration (grammar §4.3).
    Fn(FnDecl),

    /// A struct declaration (grammar §4.4).
    Struct(StructDecl),

    /// An enum declaration (grammar §4.5).
    Enum(EnumDecl),

    /// An inherent `impl Type:` block (grammar §4.6).
    Impl(ImplDecl),
}

/// A binding statement (grammar §4.1). The three surface forms collapse into
/// one node distinguished by `is_val` and the optional `ty` annotation:
///
/// - `val x = e`        → `is_val = true`,  `ty = None`
/// - `val x: T = e`     → `is_val = true`,  `ty = Some(T)`
/// - `x: T = e`         → `is_val = false`, `ty = Some(T)`  (typed mutable)
/// - `x = e`            → `is_val = false`, `ty = None`      (inferred mutable)
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    /// The bound name.
    pub name: String,
    /// `true` for an immutable `val` binding; `false` for mutable.
    pub is_val: bool,
    /// Optional explicit type annotation.
    pub ty: Option<Type>,
    /// The initializer expression.
    pub value: Expr,
}

/// An assignment to an existing l-value (grammar §4.1).
#[derive(Debug, Clone, PartialEq)]
pub struct Assign {
    /// The l-value being written.
    pub target: Target,
    /// The new value.
    pub value: Expr,
}

/// An assignment l-value: `NAME { "." NAME | "[" expr "]" }` (grammar §4.1).
///
/// `base` is the root name; `path` is the chain of field/index accesses applied
/// to it. An empty `path` means a plain name reassignment.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub base: String,
    pub path: Vec<TargetSeg>,
    pub span: Span,
}

/// One segment of a [`Target`] path.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetSeg {
    /// `.field`
    Field(String),
    /// `[index_expr]`
    Index(Expr),
}

/// `if cond: suite { elif cond: suite } [ else: suite ]` (grammar §4.2).
///
/// Conditions must evaluate to `Bool` — a **runtime** check owned by
/// [`crate::interp`] (no truthiness).
#[derive(Debug, Clone, PartialEq)]
pub struct IfStmt {
    /// The leading `if` and any `elif`s, each a (condition, body) pair.
    pub arms: Vec<(Expr, Block)>,
    /// The optional `else` body.
    pub else_body: Option<Block>,
}

/// `while cond: suite` (grammar §4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct WhileStmt {
    pub cond: Expr,
    pub body: Block,
}

/// `for NAME in iter: suite` (grammar §4.2). M1 expects `iter` to be a range or
/// a list (enforced at runtime).
#[derive(Debug, Clone, PartialEq)]
pub struct ForStmt {
    pub var: String,
    pub iter: Expr,
    pub body: Block,
}

/// A block / suite (grammar §3): an ordered list of statements.
///
/// Grammatically a `suite` is one inline simple statement or an indented block;
/// both reduce to a `Vec<Stmt>` here. Whether the final statement is the
/// block's *value* (for fn bodies and match arms) is a **semantic** rule
/// resolved by [`crate::interp`], not encoded structurally.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

// ===========================================================================
// Declarations (§4.3–§4.6)
// ===========================================================================

/// A function declaration (grammar §4.3). Also used for methods inside
/// `struct`/`impl` bodies.
#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    /// Result type from a `-> T` clause; `None` means unit `()`. (The field name
    /// `returns` is kept for stability; the surface syntax is `->` as of M2 —
    /// the `returns` keyword was dropped.)
    pub returns: Option<Type>,
    pub body: Block,
    /// Doc comment (`##`) captured immediately above, if any.
    pub doc: Option<String>,
    pub span: Span,
}

/// A function/method parameter (grammar §4.3).
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    /// The `self` receiver — only valid as the first param of a method
    /// (validity resolved by checks/interp). Untyped.
    SelfRecv,
    /// A fully annotated positional parameter `NAME: type`, with an optional
    /// **default value** `NAME: type = expr` (M2 Wave 1). The parser produces
    /// `default: None` until default args are wired up in Wave 1.
    Named {
        name: String,
        ty: Type,
        default: Option<Expr>,
    },
}

/// A struct declaration (grammar §4.4): a set of fields. Methods are defined
/// separately in an `impl` block (§4.6) — a `fn` in a struct body is a parse
/// error, so there is exactly one place methods live.
#[derive(Debug, Clone, PartialEq)]
pub struct StructDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
    pub doc: Option<String>,
    pub span: Span,
}

/// A struct field declaration `NAME: type` (grammar §4.4). Mutable by default.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: Type,
    pub doc: Option<String>,
    pub span: Span,
}

/// An enum declaration (grammar §4.5).
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub name: String,
    pub variants: Vec<VariantDecl>,
    pub doc: Option<String>,
    pub span: Span,
}

/// An enum variant declaration `NAME [ "(" payload ")" ]` (grammar §4.5).
#[derive(Debug, Clone, PartialEq)]
pub struct VariantDecl {
    pub name: String,
    /// The variant's payload shape. `None` for a niladic variant (`Empty`).
    pub payload: Option<Payload>,
    pub doc: Option<String>,
    pub span: Span,
}

/// A variant payload (grammar §4.5): all positional, or all named. The grammar
/// permits a variant to be one or the other (not mixed).
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    /// Positional payload, e.g. `Add(Expr, Expr)`.
    Positional(Vec<Type>),
    /// Named payload, e.g. `Circle(radius: Float)`.
    Named(Vec<(String, Type)>),
}

/// An inherent `impl Type:` block of methods (grammar §4.6).
#[derive(Debug, Clone, PartialEq)]
pub struct ImplDecl {
    /// The type the methods are attached to (a `base_type` name in M1).
    pub type_name: String,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

// ===========================================================================
// Expressions (§5)
// ===========================================================================

/// An expression node: an [`ExprKind`] plus its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// Every expression form (grammar §5).
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    // ----- Literals (§5.5) -----
    /// Arbitrary-precision integer literal.
    Int(BigInt),
    /// Float literal.
    Float(f64),
    /// Boolean literal.
    Bool(bool),
    /// The `null` literal.
    Null,
    /// A string literal with interpolation parts (grammar §1.5). See
    /// [`StringLit`].
    Str(StringLit),

    // ----- Primaries (§5.5) -----
    /// A bare name reference.
    Name(String),
    /// The `self` receiver.
    SelfExpr,
    /// A list literal `[ e, … ]`.
    List(Vec<Expr>),

    // ----- Compound expressions (§5) -----
    /// A lambda `params -> body` (grammar §5). The body is a single expression.
    Lambda(Lambda),

    /// A ternary `then if cond else otherwise` (grammar §5).
    Ternary {
        then: Box<Expr>,
        cond: Box<Expr>,
        otherwise: Box<Expr>,
    },

    /// A binary operation (grammar §5.3–§5.4, plus `and`/`or` and ranges).
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },

    /// A unary operation: `not e` or `-e` (grammar §5.4).
    Unary {
        op: UnOp,
        operand: Box<Expr>,
    },

    /// A call / construction `callee(args)` (grammar §5.5). Whether this is a
    /// function call or a struct/enum construction (and whether named args are
    /// valid) is **resolved later** by checks/interp.
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },

    /// An index `base[index]` (grammar §5.5).
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },

    /// A member access `base.name` (grammar §5.5), or the **safe-call**
    /// `base?.name` (M2 Wave 2) when `safe` is `true`. A safe access yields
    /// `null` if `base` is `null` instead of erroring. All M1 constructions set
    /// `safe: false`.
    Member {
        base: Box<Expr>,
        name: String,
        /// `true` for `?.` (safe access); `false` for plain `.`.
        safe: bool,
    },

    /// A `match` expression (grammar §5.7). `match` is an expression and may
    /// appear wherever a primary is allowed.
    Match(MatchExpr),

    // ----- Collections & comprehensions (M2; spec §3, §11) -----
    /// A map literal `{ k: v, … }` (M2). Insertion-ordered key/value pairs.
    /// `{}` is an empty **map** (an empty set is spelled `Set()`).
    Map(Vec<(Expr, Expr)>),

    /// A set literal `{ x, … }` (M2). At least one element (the empty case is a
    /// `Map`, so `{}` never parses as a set).
    Set(Vec<Expr>),

    /// A tuple literal `(a, b, …)` (M2). Always has at least two elements — a
    /// single parenthesized expression is grouping, not a 1-tuple.
    Tuple(Vec<Expr>),

    /// A comprehension `[out for binder in iter (if cond)?]` and its map/set
    /// forms (M2). See [`Comprehension`].
    Comprehension(Comprehension),
}

/// A string literal as a sequence of segments (grammar §1.5).
///
/// The lexer produces a `STRING` token whose value is a list of
/// [`crate::token::StrPart`]s; the parser turns each interpolation's nested
/// token stream into an [`Expr`], yielding this `StringLit`. The interpreter
/// renders each segment in order (text verbatim, expressions via `Show`).
#[derive(Debug, Clone, PartialEq)]
pub struct StringLit {
    pub parts: Vec<StrSeg>,
}

/// One segment of a [`StringLit`].
#[derive(Debug, Clone, PartialEq)]
pub enum StrSeg {
    /// Literal text (escapes already resolved by the lexer).
    Text(String),
    /// An interpolated expression `{ expr }`, already parsed.
    Expr(Box<Expr>),
}

/// A lambda expression (grammar §5).
///
/// `x -> e` yields a one-param lambda; `(a, b) -> e` yields a multi-param one.
/// Lambda parameters are **untyped** names (inference handles them). Closures
/// capture by reference (see [`crate::interp`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Lambda {
    pub params: Vec<String>,
    pub body: Box<Expr>,
}

/// A call/construction argument (grammar §5.5).
///
/// Both forms parse everywhere; the named form is only *valid* for
/// struct/enum construction — that validity is **resolved later** by
/// checks/interp.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    /// A positional argument.
    Positional(Expr),
    /// A named argument `name: expr`.
    Named { name: String, value: Expr },
}

/// A comprehension (M2; spec §11): sugar over a single `for` loop that builds a
/// list, set, or map. The general shape is
/// `OUTPUT for BINDER in ITER [ if COND ]`, wrapped in `[ ]` (list/`{ }` for
/// set/map). The binder is scoped to the comprehension.
///
/// The collection kind is carried by [`Comprehension::output`]:
/// - `[expr for …]`          → [`ComprehensionOutput::List`]
/// - `{expr for …}`          → [`ComprehensionOutput::Set`]
/// - `{key: value for …}`    → [`ComprehensionOutput::Map`]
#[derive(Debug, Clone, PartialEq)]
pub struct Comprehension {
    /// What each iteration produces (and thus the collection kind).
    pub output: ComprehensionOutput,
    /// The loop binder, scoped to the comprehension.
    pub binder: ComprehensionBinder,
    /// The iterable being walked.
    pub iter: Box<Expr>,
    /// An optional `if COND` filter; only matching elements contribute.
    pub cond: Option<Box<Expr>>,
}

/// The per-element output of a [`Comprehension`], which also selects the built
/// collection kind.
#[derive(Debug, Clone, PartialEq)]
pub enum ComprehensionOutput {
    /// `[expr for …]` — a `List` of `expr`.
    List(Box<Expr>),
    /// `{expr for …}` — a `Set` of `expr`.
    Set(Box<Expr>),
    /// `{key: value for …}` — a `Map` from `key` to `value`.
    Map { key: Box<Expr>, value: Box<Expr> },
}

/// The binder of a [`Comprehension`]: either a single name or a tuple of names
/// destructured from each element (e.g. `for (k, v) in map.items()`).
#[derive(Debug, Clone, PartialEq)]
pub enum ComprehensionBinder {
    /// `for x in …` — binds the whole element to `x`.
    Name(String),
    /// `for (a, b, …) in …` — destructures a tuple element into names.
    Tuple(Vec<String>),
}

/// Binary operators (grammar §5.3–§5.4).
///
/// `is` / `is not` mean value (in)equality, equivalent to `==` / `!=`
/// (grammar §5.3) — they are kept as distinct variants so error messages and
/// any future identity semantics can tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // logical
    Or,
    And,
    // comparison
    Eq,      // ==
    NotEq,   // !=
    Lt,      // <
    LtEq,    // <=
    Gt,      // >
    GtEq,    // >=
    Is,      // is        (value equality)
    IsNot,   // is not    (value inequality)
    // ranges
    Range,        // ..
    RangeIncl,    // ..=
    // arithmetic
    Add,     // +
    Sub,     // -
    Mul,     // *
    Div,     // /
    Rem,     // %
    Pow,     // **  (right-associative)
}

/// Unary operators (grammar §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `not e`
    Not,
    /// `-e`
    Neg,
}

// ===========================================================================
// Match & patterns (§5.7–§5.8)
// ===========================================================================

/// A `match` expression (grammar §5.7).
#[derive(Debug, Clone, PartialEq)]
pub struct MatchExpr {
    /// The scrutinee being matched.
    pub scrutinee: Box<Expr>,
    /// The arms, in source order. Exhaustiveness over an enum is the
    /// compile-time check owned by [`crate::checks`].
    pub arms: Vec<MatchArm>,
}

/// One `pattern [if guard]: arm_body` arm of a match (grammar §5.7).
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    /// An optional `if COND` **match guard** (M2 Wave 2). A guarded arm does not
    /// count toward exhaustiveness ([`crate::checks`]). The parser produces
    /// `None` until guards are wired up in Wave 2.
    pub guard: Option<Expr>,
    /// The arm body. Its *value* is its final expression (a semantic rule
    /// resolved by [`crate::interp`]).
    pub body: Block,
    pub span: Span,
}

/// A pattern node: a [`PatternKind`] plus its span (grammar §5.8).
///
/// As of M2, patterns are **recursive**: a variant's sub-patterns (and tuple
/// elements) are themselves full [`Pattern`]s, so nested destructuring like
/// `.Some(.Pair(a, b))` is representable. The previously-flat M1 cases (`_`, a
/// name, `null`, a literal) are the base [`PatternKind`] variants.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

/// A pattern (grammar §5.8). Recursive as of M2.
#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    /// `_` — wildcard, matches anything without binding.
    Wildcard,
    /// `null` — matches the null value.
    Null,
    /// A literal pattern: `INT | FLOAT | BOOL | STRING`.
    Literal(LitPattern),
    /// A bare `NAME` — binds the whole scrutinee.
    Binding(String),
    /// A variant destructure, possibly **nested**. Variants are **qualified**:
    /// either the leading-dot form `.Variant(sub, …)` (enum inferred from the
    /// scrutinee) or the explicit `Enum.Variant(sub, …)`. A niladic variant
    /// drops the parentheses (`.Empty` / `Enum.Empty`). A *bare* `NAME(…)` is
    /// no longer a variant pattern (a bare `NAME` is a [`PatternKind::Binding`]).
    ///
    /// As of M2, `subs` are full [`Pattern`]s, so a sub-pattern may itself be a
    /// variant/tuple/or pattern (nested destructuring).
    Variant {
        /// `Some("Color")` for the qualified form `Color.Red`; `None` for the
        /// leading-dot form `.Red`, resolved against the scrutinee's enum.
        enum_name: Option<String>,
        name: String,
        subs: Vec<Pattern>,
    },
    /// An **or-pattern** `p1 or p2 or …` (M2 Wave 2): matches if any alternative
    /// matches. Alternatives bind the same names (enforced later). The parser
    /// produces this only once or-patterns are wired up in Wave 2.
    Or(Vec<Pattern>),
    /// A **tuple pattern** `(p1, p2, …)` (M2 Wave 2): destructures a tuple value
    /// element-wise. Always two or more elements.
    Tuple(Vec<Pattern>),
}

/// A literal usable in a pattern (grammar §5.8).
///
/// Note: a string literal pattern is a plain decoded `String` — patterns never
/// contain interpolations (an interpolated string is not a constant), so this
/// does not reuse [`StringLit`].
#[derive(Debug, Clone, PartialEq)]
pub enum LitPattern {
    Int(BigInt),
    Float(f64),
    Bool(bool),
    Str(String),
}

// ===========================================================================
// Types (§6)
// ===========================================================================

/// A type annotation (grammar §6): a base type with an optional `?` nullable
/// suffix.
#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub base: BaseType,
    /// `true` if a trailing `?` made it nullable (`String?`, `List[Int]?`).
    pub nullable: bool,
    pub span: Span,
}

/// The base of a [`Type`] (grammar §6).
#[derive(Debug, Clone, PartialEq)]
pub enum BaseType {
    /// A (possibly generic-applied) named type, e.g. `Int`, `List[Int]`,
    /// `Map[K, V]`. `args` is empty for a plain name.
    Named { name: String, args: Vec<Type> },
    /// The unit type `()`.
    Unit,
    /// A **function type** `(T1, …, Tn) -> R` (M2; spec §6). Zero params is
    /// `() -> R`. Parsed and used for documentation/parameter signatures, but
    /// not statically checked in M2 (typed-lite — see `spec/04-m2-scope.md`).
    Fn {
        /// The parameter types (possibly empty).
        params: Vec<Type>,
        /// The result type. Always present (`-> R`); unit results are `()`.
        ret: Box<Type>,
    },
    /// A **tuple type** `(A, B, …)` (M2; spec §6). Always has at least two
    /// components — `(T)` is grouping, not a 1-tuple.
    Tuple(Vec<Type>),
}
