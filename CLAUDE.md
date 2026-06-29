# CLAUDE.md

Guidance for AI coding sessions on Adder — the tree-walking
interpreter for a Python-readable, Rust-expressive language. The spec in
[`spec/`](spec/) is the source of truth; the surface grammar is split across
[`spec/03-mvp-grammar.md`](spec/03-mvp-grammar.md),
[`spec/05-m2-grammar.md`](spec/05-m2-grammar.md), and
[`spec/07-m3-grammar.md`](spec/07-m3-grammar.md) (read them together — each later
file adds to the earlier grammar). Don't duplicate the spec here — link to it.

## Pipeline

`lex -> parse -> check -> run`. Each stage owns one module directory — a `mod.rs`
and a `tests.rs`, plus topic submodules where the stage is large enough to
warrant them (`lexer` is just `mod.rs` + `tests.rs`); `lib.rs` has the canonical
ownership notes. The stage entry points are unchanged.

| Stage | Entry point | Module | Owns |
| --- | --- | --- | --- |
| lex   | `lexer::lex`     | `src/lexer/`    | source -> tokens; indentation (`Indent`/`Dedent`/`Newline`), string-interpolation re-lexing |
| parse | `parser::parse`  | `src/parser/`   | tokens -> `ast::Program`; parses interpolation sub-exprs (`stmt`/`control`/`item`/`expr`/`pattern`/`types`) |
| check | `checks::check`  | `src/checks/`   | the two static checks (below) and *only* those (`exhaustiveness`/`null_narrowing`) |
| run   | `interp::run`    | `src/interp/`   | the tree-walker + all runtime enforcement (below) (`value`/`env`/`show`/`builtins`) |

**Contracts** (shared; changing them ripples downstream):

- `src/token.rs` — lexer<->parser contract (`Token`, `Span`, `StrPart`). A `Token`
  also carries an optional `doc: Option<String>` — a `##` doc comment the lexer
  attaches to the next real token, which the parser reads into the AST's `doc` fields
  (run `cargo run -- --docs <file.adr>` to see them).
- `src/ast.rs` — parser<->checks<->interp contract (`Program` et al.).
- `src/error.rs` — `Diagnostic` + source renderer (caret underline), used by
  every stage and the CLI. `Phase` labels are `lex error` / `parse error` /
  `check error` / `runtime error`.

CLI driver is `src/main.rs`; it delegates to the in-process pipeline entry point
`adder::run_source(src, out)` in `lib.rs` (which writes program output to a
caller-supplied `Write`, so tests/embedders can capture it). Usage is
`adder <file.adr>`, exits non-zero on any diagnostic.

## Where rules are enforced

**Compile-time (`checks/`)** — exactly two analyses, run before execution:

1. **Match exhaustiveness** — a `match` over an enum must cover every variant (or
   `_`).
2. **Null-narrowing** — using a `T?` where a `T` is required is an error unless
   narrowed (`if x is not null:`) or defaulted (`.or_else(...)`).

**Runtime (`interp/`)** — everything else, including:

- `val`-immutability (reassigning a `val` is a runtime error),
- Bool-condition enforcement (`if`/`elif`/`while`/ternary; no truthiness),
- default `Show` rendering (for `print` / interpolation) and structural `==`
  (and `is` / `is not`) by walking runtime values,
- the prelude (`print`, `panic`) seeded as ordinary bindings,
- entry point: run top-level statements, then call `main()` if a zero-arg `main`
  exists.

Do not move runtime rules into `checks/` or vice versa — the split is
deliberate (the project is "typed-lite", not a full checker).

## Syntax cheat-sheet (do not regress to older forms)

Verify any new example against `examples/` and the grammar specs. Pipelines,
comprehensions, tuples, Map/Set, match guards, and `?.` are documented in
[`spec/04-m2-scope.md`](spec/04-m2-scope.md) and
[`spec/05-m2-grammar.md`](spec/05-m2-grammar.md); traits, `Result`/`try`,
`derive`, and generics in [`spec/06-m3-scope.md`](spec/06-m3-scope.md) and
[`spec/07-m3-grammar.md`](spec/07-m3-grammar.md).

- **Files** use the `.adr` extension.
- **Functions**: fully annotated — `fn f(a: Int) -> Int:`; omit the arrow for unit.
  Implicit final-expression return, or explicit `return`.
- **Bindings**: `val x = e` is immutable; bare `x = e` is mutable. Locals
  inferred; annotate when needed (`xs: List[Int] = []`).
- **Enums**: variants are namespaced under the enum. Construct `Color.Red` /
  `Shape.Circle(radius: 2.0)`. Match with leading-dot `.Circle(r):` (enum
  inferred from scrutinee) or explicit `Shape.Circle(r):`; a bare `NAME:` is a
  binding, not a variant. Matches must be exhaustive.
- **Structs**: fields in the struct body; methods **only** in `impl Type:`
  blocks with a `self` receiver; mutate via `self.field = e`. A `fn` in a struct
  body is a parse error.
- **Nullability**: `T?`, `null`; narrow with `if x is not null:` or default with
  `x.or_else(default)`.
- **No truthiness** — conditions must be `Bool`. **No implicit Int/Float
  coercion.** `Int` is arbitrary precision. `**` is right-associative.
- **Strings**: interpolate `"{expr}"`; escape braces `{{` / `}}`.
- `print` / `panic` are built-in functions (shadowable), not keywords.
- **Lists** `[..]` with indexing; `for x in ...` over ranges (`0..n`, `0..=n`)
  or lists. Ternary is `value if cond else other`.

## Adding an example + acceptance test

1. Write the program at `examples/<name>.adr` (or `examples/errors/<name>.adr`
   for a program that should be rejected). Match an existing example's style;
   `##` doc comments are conventional at the top.
2. Add a test in `tests/acceptance.rs`. Tests spawn the compiled `adder` binary
   on a fixture (via the `run_fixture` helper) and assert on stdout / stderr /
   exit status — they exercise the whole pipeline as a user would. For a
   rejection test, assert `!o.status.success()`, that stderr contains the right
   phase label (e.g. `"check error"`), and that nothing ran (`stdout` empty).
3. Run the suite:

   ```sh
   cargo test
   cargo run -- examples/<name>.adr   # sanity-check it by hand
   ```

The M1 **definition of done** lives in these acceptance tests and in
[`spec/02-mvp-scope.md`](spec/02-mvp-scope.md): the showcase prints `= 9.0`,
removing a match arm is a compile-time error, an unnarrowed `T?` is a
compile-time error, and `val` reassignment / non-Bool conditions are rejected.
The M2 **definition of done** lives in `tests/m2_showcase.rs` /
`examples/m2_showcase.adr` and [`spec/04-m2-scope.md`](spec/04-m2-scope.md).
The M3 **definition of done** lives in `tests/m3_showcase.rs` / `tests/m3_features.rs` /
`examples/m3_showcase.adr` and [`spec/06-m3-scope.md`](spec/06-m3-scope.md).

## Runtime-checked features (typed-lite — don't over-assume)

These features ([`spec/06-m3-scope.md`](spec/06-m3-scope.md), grammar
[`spec/07-m3-grammar.md`](spec/07-m3-grammar.md)) are all *runtime*-checked — the
two static checks are unchanged except that exhaustiveness also sees the prelude
`Result` enum:

- **Traits** — `trait` (required sigs + default methods), `impl Trait for Type`, `Self`
  (as a type). Dispatch is runtime: a trait impl's methods and inherited defaults are
  folded into the same method table as inherent methods (`interp::collect_decls`), so a
  trait-typed parameter (`fn f(x: Drawable)`) is duck-dispatched. A missing required
  method is a **runtime** error, not a static one.
- **`Result` + `try`** — `Result[T, E]` (`Ok`/`Err`) is a prelude enum seeded by both the
  checker and the interpreter (`ast::result_enum_decl`); `Ok`/`Err` are prelude
  constructors and may be matched **unqualified** (`Ok(v):`/`Err(e):`) — the one exception
  to the qualified-variant rule. `try expr` early-returns the `Err` via a propagation
  side-channel (`Interp::propagating` + `finish_body`), not an `EvalError` refactor.
- **`derive Ord`** — opt-in structural ordering (lexicographic by declaration order),
  gated by `Registry::ord_types`; enables `<`/`<=`/`>`/`>=` and `.sort()`/`sorted`/`min`/
  `max` on user types. `Eq`/`Hash`/`Show` remain automatic.
- **Generics** — `[T: Bound and Bound2]` on `fn`/`struct`/`enum`/`impl`/`trait` are
  **parsed and erased**, never checked (like function types — parsed, not checked).

## Deferred — NOT yet implemented (M4+)

These are not yet built — don't assume they exist:

- **Full type checker + inference** (only the two static checks exist) — the M4 posture
  change; also home to **generic bound checking** and **trait conformance** as static checks
- Lazy iterator pipelines (pipelines currently evaluate eagerly)
- Modules / imports
- Associated types/constants on traits; `Char`; REPL; word-ranges; `private`
