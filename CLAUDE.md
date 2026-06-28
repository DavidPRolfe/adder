# CLAUDE.md

Guidance for AI coding sessions on Adder — the tree-walking
interpreter for a Python-readable, Rust-expressive language. The spec in
[`spec/`](spec/) is the source of truth; [`spec/03-mvp-grammar.md`](spec/03-mvp-grammar.md)
is the authority for surface syntax. Don't duplicate the spec here — link to it.

## Pipeline

`lex -> parse -> check -> run`. Each stage owns one file; `lib.rs` has the
canonical ownership notes.

| Stage | Entry point | File | Owns |
| --- | --- | --- | --- |
| lex   | `lexer::lex`     | `src/lexer.rs`  | source -> tokens; indentation (`Indent`/`Dedent`/`Newline`), string-interpolation re-lexing |
| parse | `parser::parse`  | `src/parser.rs` | tokens -> `ast::Program`; parses interpolation sub-exprs |
| check | `checks::check`  | `src/checks.rs` | the two static checks (below) and *only* those |
| run   | `interp::run`    | `src/interp.rs` | the tree-walker + all runtime enforcement (below) |

**Contracts** (shared; changing them ripples downstream):

- `src/token.rs` — lexer<->parser contract (`Token`, `Span`, `StrPart`). A `Token`
  also carries an optional `doc: Option<String>` — a `##` doc comment the lexer
  attaches to the next real token, which the parser reads into the AST's `doc` fields
  (run `cargo run -- --docs <file.adr>` to see them).
- `src/ast.rs` — parser<->checks<->interp contract (`Program` et al.).
- `src/error.rs` — `Diagnostic` + source renderer (caret underline), used by
  every stage and the CLI. `Phase` labels are `lex error` / `parse error` /
  `check error` / `runtime error`.

CLI driver is `src/main.rs` (`run_pipeline`); usage is `adder <file.adr>`, exits
non-zero on any diagnostic.

## Where rules are enforced

**Compile-time (`checks.rs`)** — exactly two analyses, run before execution:

1. **Match exhaustiveness** — a `match` over an enum must cover every variant (or
   `_`).
2. **Null-narrowing** — using a `T?` where a `T` is required is an error unless
   narrowed (`if x is not null:`) or defaulted (`.or_else(...)`).

**Runtime (`interp.rs`)** — everything else, including:

- `val`-immutability (reassigning a `val` is a runtime error),
- Bool-condition enforcement (`if`/`elif`/`while`/ternary; no truthiness),
- default `Show` rendering (for `print` / interpolation) and structural `==`
  (and `is` / `is not`) by walking runtime values,
- the prelude (`print`, `panic`) seeded as ordinary bindings,
- entry point: run top-level statements, then call `main()` if a zero-arg `main`
  exists.

Do not move runtime rules into `checks.rs` or vice versa — the split is
deliberate (the project is "typed-lite", not a full checker).

## Syntax cheat-sheet (do not regress to older forms)

Verify any new example against `examples/` and `spec/03-mvp-grammar.md`. The M2
surface syntax (pipelines, comprehensions, tuples, Map/Set, match guards, `?.`)
is documented in [`spec/04-m2-scope.md`](spec/04-m2-scope.md) and
[`spec/05-m2-grammar.md`](spec/05-m2-grammar.md).

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

## Deferred — NOT yet implemented (M3+)

These are not yet built — don't assume they exist:

- Traits / `impl Trait for Type` / `Self` / `derive`
- Generics + trait bounds (user-declared `[T]`)
- `Result` / `try` / error propagation (the project uses `panic`)
- Modules / imports
- Full type checker + inference (only the two static checks exist)
