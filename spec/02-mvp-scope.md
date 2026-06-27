# Adder — MVP Scope (Milestone 1)

The goal of the MVP is to answer one question fast: **does Adder feel right to write?**
A 50-line program should already read and feel like Adder — Python-easy, with the
enum/match expressiveness that sets it apart — while exercising just enough of the
type system to prove the thesis isn't "dynamic Python with nicer syntax."

This doc defines the **smallest runnable Adder**. The full language lives in
[01-language-reference.md](01-language-reference.md); anything not listed under
"In scope" here is deferred to a later milestone *without redesign* — the cuts were
chosen so they layer on cleanly.

## Strategy: "typed-lite" tree-walker

A pure dynamic interpreter wouldn't test the thesis, and a full type checker with
inference is too much for a first cut. So the MVP is a **tree-walking interpreter
plus exactly two static checks**, run before execution:

1. **Match exhaustiveness** — a `match` over an enum must cover every variant (or use
   `_`). This is *the* feature that makes Adder feel like Rust rather than Python, and
   it's tractable over a closed set of variants without a full type system.
2. **Null-narrowing** — using a `T?` value where a `T` is required is a compile error
   unless it has been narrowed (`if x is not null:`) or defaulted (`.or_else(...)`).
   This delivers the "no surprise `null` blow-ups" promise.

Everything else is checked dynamically at runtime for now. Full bidirectional type
inference, and broader static type checking, are a later milestone.

> Rationale recorded in the MVP-scoping review: ship these two local analyses (both
> cheap, neither needs a global inference engine); defer the rest of the checker.

## In scope (Milestone 1)

**Lexing / parsing**
- Significant indentation, `:`-introduced blocks, inline single-statement blocks.
- `#` line comments, `##` doc comments.
- Newline-terminated statements; bracket-continuation across lines.

**Values & types** (runtime-tagged; annotations parsed and used by the two checks)
- `Int` (arbitrary precision), `Float`, `Bool`, `String`, `List[T]`, `()`.
- `T?` nullable types and the `null` literal.
- Structs and enums (with data-carrying variants).
- `Map`/`Set` are *nice-to-have* in M1 — include only if cheap; otherwise M2.

**Bindings**
- Mutable-by-default `=`; immutable `val` (reassignment is an error).
- Local type inference; explicit annotation allowed when inference can't decide
  (e.g. `xs: List[Int] = []`).

**Operators**
- `and` / `or` / `not`; `is` / `is not` and `==` / `!=` (value equality);
  `< <= > >=`; `+ - * / % **`.
- **Conditions must be `Bool`** (no truthiness coercion) — checked at runtime.

**Functions**
- `fn` with positional parameters; **fully annotated signatures** (param types +
  `returns` clause; omit `returns` for no result).
- Explicit `return` and implicit final-expression return.
- Lambdas `x -> expr` and `(a, b) -> expr`; closures capture by reference.

**Structs**
- Declaration, named + positional construction, field access, mutable fields,
  inherent methods (`fn` inside the struct or an `impl Type` block).

**Enums + match**
- Variants with positional/named data.
- `match` as an expression, with **exhaustiveness checking**.
- Patterns: variant destructuring with simple bindings, literals, `null`, and `_`.

**Nullability**
- `T?`, `null`, `is not null` **smart-narrowing**, `x.or_else(default)`.

**Control flow**
- `if` / `elif` / `else`, ternary `a if cond else b`, `while`, `for x in 0..n`,
  `break`, `continue`.

**Strings**
- `"{ }"` interpolation with `{{` / `}}` escaping.

**Output / display**
- Built-in `print`, and a default `Show`-style rendering produced by walking runtime
  values (so any struct/enum prints without the user writing anything).
- Structural `==` likewise produced by walking values.

**Entry point**
- Run top-level statements, then call `main()` if defined. No `__name__` ceremony.

## Deferred (Milestone 2+)

| Feature | Why it's safe to defer | Rough phase |
| --- | --- | --- |
| Traits / `impl ... for` / `Self` | Structs + inherent methods validate the feel; trait resolution is the biggest build cost | M2 (next) |
| Generics + trait bounds | Built-ins can be generic internally; user generics need traits + a checker | M2/M3 |
| `Result` + `try` | `panic` covers "test the feel"; `try`'s early-return desugaring is non-trivial | M2 |
| Full type checker + inference | The two MVP checks carry the thesis; full inference is large | M3 |
| Lazy iterator pipelines (`map`/`filter`/`fold`) | `for` loops cover iteration; laziness is real interpreter work | M2 |
| Comprehensions | Pure sugar over `for` | M2 |
| Default / named arguments | Positional is fine for small programs | M2 |
| The module system (imports/packages) | Single-file programs validate feel | M2 |
| Auto-derived `Ord` / `.sort()` | Opt-in ordering isn't needed to feel like Adder | M2 |
| `?.` safe-call, `.expect` | Narrowing + `or_else` cover the common cases; these are secondary sugar | M2 |
| Match guards, or-patterns, nested destructuring | Flat variant matching is enough to start | M2 |
| `private` visibility, per-field `val` | Additive; nothing depends on them | later |
| Macros, concurrency, native/bytecode backend | Out of scope by design | v2+ |

## MVP showcase program

The §13 evaluator, rewritten to the MVP subset — note it swaps `Result`/`try` for
`panic` (deferred error model) and otherwise reads identically:

```adder
## A tiny expression evaluator (MVP subset).

enum Expr:
    Num(Float)
    Add(Expr, Expr)
    Mul(Expr, Expr)
    Div(Expr, Expr)

fn eval(e: Expr) returns Float:
    return match e:
        Num(n):    n
        Add(a, b): eval(a) + eval(b)
        Mul(a, b): eval(a) * eval(b)
        Div(a, b):
            divisor = eval(b)
            if divisor == 0.0:
                panic("division by zero")
            eval(a) / divisor

fn main():
    # (1 + 2) * 3
    program = Mul(Add(Num(1.0), Num(2.0)), Num(3.0))
    print("= {eval(program)}")     # = 9.0
```

This program exercises: indentation syntax, enums with data, an exhaustive `match`
expression, recursion, `fn` signatures with `returns`, `val`, `panic`, and string
interpolation — the whole MVP feel in ~20 lines, with both static checks active
(exhaustiveness over `Expr`, and no nulls to mishandle here).

## Definition of done for Milestone 1

- The showcase program above runs and prints `= 9.0`.
- Removing the `Div` arm from the `match` is a **compile-time** exhaustiveness error.
- Using a `T?` value as a `T` without narrowing is a **compile-time** error; the
  same code guarded by `is not null` compiles and runs.
- A `val` reassignment is rejected; a non-`Bool` condition is rejected.
