# Adder — Language Specification

Adder is a strongly-typed, garbage-collected programming language designed as **a
modern successor to Python**: it keeps Python's approachability and readability while
adding the better semantics and a more expressive type system you need when a
codebase scales up. It borrows the *expressive* parts of Rust's type system
(algebraic data types, exhaustive pattern matching, traits, generics, iterator
pipelines) and leaves behind the parts that fight the programmer (lifetimes,
borrowing, ownership).

> The name nods to its inspirations: a smaller, friendlier cousin of the **Python**,
> with some of the **Rust** in its scales.

## Design goal in one sentence

> You write types at the edges (function signatures) — not on every line — and they
> are always there underneath, inferred and checked, keeping the program consistent.

## Documents

| Doc | Purpose | Status |
| --- | --- | --- |
| [00-design-principles.md](00-design-principles.md) | Vision, non-goals, and the locked-in foundational decisions | Draft |
| [01-language-reference.md](01-language-reference.md) | Concrete syntax & semantics (the full working language design) | Draft |
| [02-mvp-scope.md](02-mvp-scope.md) | What ships in the first runnable MVP, and what's deferred | Draft |
| [03-mvp-grammar.md](03-mvp-grammar.md) | The formal M1 surface grammar (authority for M1 syntax) | Draft |
| [04-m2-scope.md](04-m2-scope.md) | What ships in Milestone 2 (data-flow core + sugar), and what's deferred | Draft |

## Locked-in decisions (from initial design session)

| Area | Decision |
| --- | --- |
| Memory management | Tracing garbage collector |
| Surface syntax | Indentation / significant whitespace (Python-like) |
| Error handling | Hybrid — `Result` + `try` for recoverable, `panic` for bugs |
| Optionality | Kotlin-style nullable types (`T?`, `?.`, `.or_else()`), **not** `Option<T>` wrapping |
| Execution model | Tree-walking interpreter first; faster backend later |
| Abstraction | Traits (with default methods), **no inheritance** |
| Mutability | Mutable by default |
| Type annotations | Required on **function signatures** (params + `->` result type); inferred for locals |
| Null fallback | `x.or_else(default)` (method) — keeps `else` for conditionals only |
| Function result | `->` arrow (`fn f(...) -> Int:`); the **same** `->` types lambdas and function types; omitted for unit |
| Concurrency | Deferred to v2 |
| Audience / domain | General-purpose; a Python successor. Optimized for **readability/usability for fluent programmers**, not absolute beginners |
| Style mandate | **Prefer English keywords to symbols** (`and`/`or`/`not`, `is not null`, `try`) — a tie-breaker, not absolute: universal notation like `->` stays |
| Integers | Arbitrary precision (no overflow) |
| First MVP | **Typed-lite** tree-walker — two static checks: match exhaustiveness + null-narrowing (see [02-mvp-scope.md](02-mvp-scope.md)) |

## Open questions / assumptions to confirm

- Whether to ship default + named arguments in v1 (leaning **defer**).
- Exact `derive` spelling, and whether `Show` splits into debug vs display forms.
- Word-based ranges (`0 to 10`) vs `0..10`.
- Project manifest/root name (module residual).
- Macros / metaprogramming are explicitly **out of scope for v1**.

*Resolved across sessions: domain = general-purpose Python successor for a
beginner-friendly audience; integers = arbitrary precision; English-over-symbols is
a standing style mandate; `const` dropped (`val` is the only immutable binding);
module system — Python-style imports, everything importable, directories are pure
namespaces with no package-entry file, `private` deferred; auto-derived
`Eq`/`Hash`/`Show` with opt-in `Ord`; signatures fully annotated (params + `->`
result type); function results use the `->` arrow with the `returns` keyword dropped
(M2-scoping decision); audience reframed to fluent programmers over absolute beginners;
null-fallback is `x.or_else()`; first MVP is typed-lite (exhaustiveness +
null-narrowing only).*

Everything marked **(proposed)** in the reference doc is a starting point open to revision.
