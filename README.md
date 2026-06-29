# Adder

Adder is **a modern successor to Python**: it keeps Python's approachability and
readability — significant indentation, low punctuation, string interpolation —
while borrowing the *expressive* parts of Rust's type system (algebraic data
types, exhaustive pattern matching, nullable types) and leaving behind the parts
that fight the programmer (lifetimes, borrowing, ownership). You write types at
the edges — on function signatures — and they are checked underneath. This repo
is the interpreter: a tree-walking interpreter written in Rust. The full
language vision lives in [`spec/`](spec/).

> The name nods to its inspirations: a smaller, friendlier cousin of the
> **Python**, with some of the **Rust** in its scales.

## A taste

[`examples/eval.adr`](examples/eval.adr) — a tiny expression evaluator:

```adder
## A tiny expression evaluator (MVP subset).

enum Expr:
    Num(Float)
    Add(Expr, Expr)
    Mul(Expr, Expr)
    Div(Expr, Expr)

fn eval(e: Expr) -> Float:
    return match e:
        .Num(n):    n
        .Add(a, b): eval(a) + eval(b)
        .Mul(a, b): eval(a) * eval(b)
        .Div(a, b):
            divisor = eval(b)
            if divisor == 0.0:
                panic("division by zero")
            eval(a) / divisor

fn main():
    # (1 + 2) * 3
    program = Expr.Mul(Expr.Add(Expr.Num(1.0), Expr.Num(2.0)), Expr.Num(3.0))
    print("= {eval(program)}")     # = 9.0
```

## Build, run, test

Adder source files use the `.adr` extension.

```sh
cargo build                       # build the interpreter
cargo run -- examples/eval.adr    # run a program  ->  = 9.0
cargo run -- --docs examples/eval.adr  # print declarations with their ## doc comments
cargo test                        # run the test suite (unit + acceptance)
```

`cargo run -- <file.adr>` runs the whole pipeline (lex -> parse -> check -> run),
executing top-level statements and then calling `main()` if a zero-argument
`main` is defined. Errors from any stage are reported with a caret pointing at
the offending source.

More programs to try: [`examples/ledger.adr`](examples/ledger.adr) (a small bank
ledger), [`examples/shapes.adr`](examples/shapes.adr) (structs and methods),
[`examples/shapes_enum.adr`](examples/shapes_enum.adr) (namespaced enum
variants), and [`examples/narrowed.adr`](examples/narrowed.adr) (null narrowing).
The [`examples/errors/`](examples/errors/) directory holds programs that are
*supposed* to be rejected — useful for seeing the diagnostics.

## Language tour

This covers the core surface syntax. The grammar is specified across
[`spec/03-mvp-grammar.md`](spec/03-mvp-grammar.md),
[`spec/05-m2-grammar.md`](spec/05-m2-grammar.md), and
[`spec/07-m3-grammar.md`](spec/07-m3-grammar.md) (each adds to the last); the
matching scope docs (`02`/`04`/`06`) say what's in vs. deferred. Beyond the
basics below, the language also has pipelines, comprehensions, tuples, Map/Set,
match guards, `?.`, traits, `Result`/`try`, and `derive`.

**Functions** have fully annotated signatures (parameter types plus an `->` result
clause; omit the `->` for no result). The body returns its final expression
implicitly, or via an explicit `return`.

```adder
fn double(n: Int) -> Int:
    n * 2                       # implicit final-expression return

fn greet(name: String):        # no -> clause = unit result
    print("hello {name}")
```

**Bindings** are mutable by default with bare `=`; `val` makes a binding
immutable (reassigning it is an error). Locals are inferred; annotate when
inference can't decide (`xs: List[Int] = []`).

```adder
count = 0          # mutable
count = count + 1
val pi = 3.14159   # immutable
```

**Enums** carry data and namespace their variants under the enum. Construct with
`Enum.Variant(...)` (a niladic variant is just `Enum.Variant`). `match` is an
expression and **must be exhaustive**. In match arms, the leading-dot form
`.Variant(...)` infers the enum from the scrutinee (or write `Enum.Variant(...)`
explicitly); a bare `NAME` is a binding, not a variant.

```adder
enum Shape:
    Circle(radius: Float)
    Rect(w: Float, h: Float)
    Unit                        # niladic variant

fn area(s: Shape) -> Float:
    return match s:
        .Circle(r):  3.14159 * r * r
        .Rect(w, h): w * h
        .Unit:       1.0

area(Shape.Circle(radius: 2.0))
```

**Structs** declare fields in the body; methods live **only** in an `impl Type:`
block, take a `self` receiver, and mutate through `self.field = e`. Construction
is a bare call, positional or named.

```adder
struct Rectangle:
    width: Float
    height: Float

impl Rectangle:
    fn area(self) -> Float:
        return self.width * self.height

    fn grow(self, factor: Float):           # mutates self in place
        self.width = self.width * factor
        self.height = self.height * factor

r = Rectangle(3.0, 4.0)                      # positional
unit = Rectangle(width: 1.0, height: 1.0)    # named
```

**Nullability** is part of the type: `T?` and the `null` literal. Use a `T?`
where a `T` is required and you get a compile-time error unless you narrow it
(`if x is not null:`) or default it (`x.or_else(default)`).

```adder
fn add_one(x: Int?) -> Int:
    if x is not null:
        return x + 1            # x is narrowed to Int here
    return 0
```

**Other facts to know:**

- No truthiness — conditions in `if` / `elif` / `while` / ternary must be `Bool`.
- No implicit `Int`/`Float` coercion; `Int` is arbitrary precision (no overflow).
- Operators use English where it helps: `and` / `or` / `not`, `is` / `is not`
  (value equality). `**` is right-associative.
- Strings interpolate with `"{expr}"`; escape literal braces with `{{` and `}}`.
- `print` and `panic` are ordinary built-in functions (not keywords).
- Lists are `[..]` with indexing; `for x in ...` iterates ranges (`0..n`,
  `0..=n`) or lists.
- Ternary reads `value if cond else other`.

## Project status

A **typed-lite tree-walker**. Rather than a full type checker, it runs exactly
two static checks before execution:

1. **Match exhaustiveness** — a `match` over an enum must cover every variant (or
   use `_`).
2. **Null-narrowing** — using a `T?` value where a `T` is required is a
   compile-time error unless it has been narrowed or defaulted.

Everything else (e.g. `val`-immutability, the Bool-condition rule, and trait
method dispatch) is enforced at runtime. The implemented surface includes enums,
structs and methods, traits, nullability, collections (List/Map/Set), pipelines,
comprehensions, `Result`/`try`, and opt-in `derive Ord`. A full type checker with
inference, generic-bound checking, and trait conformance is the next milestone.

This is a pre-1.0 interpreter and the language will change. The `spec/` scope docs
([`02`](spec/02-mvp-scope.md) / [`04`](spec/04-m2-scope.md) /
[`06`](spec/06-m3-scope.md)) give the full in-scope / deferred breakdown;
modules/imports and lazy iterators remain deferred.

## Project layout

```
spec/        Language spec: design principles (00), language reference (01),
             and per-milestone scope + grammar docs (02–07)
src/         The interpreter; each pipeline stage owns a module:
  token.rs     lexer<->parser contract (tokens, spans, string parts)
  ast.rs       parser<->checks<->interp contract (the syntax tree)
  error.rs     diagnostics shared by every stage
  lexer/       lex:   source -> tokens (+ indentation, string interpolation)
  parser/      parse: tokens -> AST
  checks/      check: exhaustiveness + null-narrowing
  interp/      run:   the tree-walker + runtime enforcement
  lib.rs       pipeline overview and ownership notes
  main.rs      the `adder` CLI
examples/    Runnable `.adr` programs (errors/ holds ones meant to be rejected;
             features/ covers individual language features)
tests/       acceptance.rs (definition-of-done), features.rs (per-feature
             coverage), program_ledger.rs (the ledger example), the feature
             suites (collections/nullable/patterns/pipelines/sets_maps), and the
             showcase tests (m2_showcase.rs, m3_showcase.rs, m3_features.rs)
```
