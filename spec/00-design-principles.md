# Adder — Design Principles

This document captures *why* Adder is the way it is. When a concrete syntax or
semantic question comes up later, it should be answered by appeal to these
principles. The detailed mechanics live in
[01-language-reference.md](01-language-reference.md).

## 1. Vision

Adder is **a modern successor to Python**: the same approachability and readability,
but with the better semantics and a more expressive type system you need when a
codebase scales up. It targets the problems Python hits in the large — silent type
errors, refactors that break at runtime, `None` blowing up three layers down, APIs
you can't trust — while keeping the qualities that make Python pleasant to write and
easy to learn.

It borrows the *expressive* parts of Rust's type system — algebraic data types,
exhaustive pattern matching, traits, generics, iterator pipelines — because those
are exactly the tools that let you model a growing domain precisely and make illegal
states unrepresentable. It leaves behind the parts that fight the programmer:
lifetimes, the borrow checker, ownership.

It should *read* like Python: clean, indentation-structured, low on punctuation, and
understandable even to people relatively new to programming. It should *behave* like
a strongly-typed language: nothing is `Any` by accident, mismatches are caught
before the program runs, and refactors are safe.

The unifying idea: **types are pervasive but concentrated at the edges.** You write
them where they earn their keep — on **function signatures** (parameter types and the
`->` result type), where they double as documentation and produce better error
messages. *Inside* a function, almost nothing is annotated: locals, literals, and
lambdas are inferred. So "you rarely write types" is true line-by-line, but honest:
the annotations live at signatures, not on every binding. (Dropping signature
annotations on private helpers via stronger inference is planned future ergonomics,
not a v1 promise.)

### Readability over terseness — prefer English to symbols

Adder targets **programmers fluent in mainstream languages**, not absolute beginners;
"easy" means *readability and usability*, not a teaching language. So where there's a
genuine choice between a word and a sigil, **the word wins**:

- Logical operators are `and` / `or` / `not`, never `&&` / `||` / `!`.
- Null checks read as `x is not null`, not `x != null`.
- Error propagation is `try expr`, not `expr?`.
- Anonymous functions are `x -> x + 1`, not `|x| x + 1`.

This is a tie-breaker, not an absolute: universally-understood math and comparison
symbols (`+ - * / < > == <=`) stay, because spelling them out (`plus`, `is greater
than`) would hurt readability, not help it. The same exception covers `->` — the
function/mapping arrow, read aloud as "maps to" — which every working programmer
already knows from math and from other languages; a word here (`x gives x + 1`) would
not read better. The test is always *"would a fluent programmer read this line
correctly and understand it at a glance?"*

## 2. The three pillars

1. **Expressive like Rust.** Algebraic data types + exhaustive pattern matching,
   traits for shared behavior, generics with trait bounds, first-class closures and
   lazy iterator pipelines. These are the features that make Rust feel powerful, and
   they survive intact.

2. **Easy like Python.** Significant indentation, no semicolons or braces, minimal
   sigils, string interpolation, comprehensions, a REPL, mutable-by-default
   variables, and a fast tree-walking interpreter so the edit→run loop is instant.

3. **Safe by inference, not by ceremony.** A strong static type system runs
   underneath, but you rarely spell it out. Safety comes from the *type checker*,
   not from forcing the programmer to prove memory facts (the GC handles memory;
   there are no lifetimes).

## 3. What we deliberately drop from Rust

- **Lifetimes and the borrow checker.** Memory is managed by a tracing GC. This is
  the single biggest simplification and the whole reason the language exists.
- **`Option<T>` as the absence type.** Replaced by Kotlin-style nullable types
  (`T?`) with safe-call and elvis operators. `Option` may still exist as a library
  type, but day-to-day absence uses `?`.
- **Move semantics / ownership as a user-facing concept.** Values are GC-managed
  references; no `move`, no `&`/`&mut` borrows in the surface language.
- **Macros (for v1).** Powerful but complex; deferred so the core can stabilize.
- **Lifetime-driven `unsafe`.** No raw-pointer story in v1.

## 4. What we deliberately drop from Python

- **Dynamic typing.** Everything is statically typed; "duck typing" is replaced by
  traits and generics. No runtime attribute injection, no `__getattr__` surprises.
- **Inheritance.** No class hierarchies; composition + traits instead.
- **`None` as an untyped sentinel.** Nullability is tracked in the type system; you
  cannot pass a `null` where a non-nullable type is expected.
- **"Everything is mutable global soup."** Mutable by default, yes, but lexically
  scoped, statically resolved, and type-checked.

## 5. Tone of the type system

- **Inference is local + bidirectional.** Locals, literals, and most expressions
  infer their types. Function *signatures* are the annotation boundary.
- **Strong, not weak.** No implicit numeric coercions that lose information, no
  truthiness-of-arbitrary-values surprises (conditions must be `Bool`).
- **Nullability is part of the type.** `String` and `String?` are different types;
  the checker forces you to handle the `null` case (via `?.`, `?:`, smart-narrowing,
  or `match`).
- **Exhaustiveness is enforced.** `match` over an enum must cover every variant (or
  use a wildcard). Adding a variant turns incomplete matches into compile errors —
  the Rust refactoring superpower.

## 6. Non-goals (at least for v1)

- Maximal runtime performance (we start interpreted; native/bytecode comes later).
- Concurrency / parallelism (deferred to v2; the design will avoid foreclosing it).
- A macro / compile-time metaprogramming system.
- C/FFI and systems-level programming.
- Backwards-compatibility guarantees — the language is pre-1.0 and will change.

## 7. Success test

A Python programmer should be able to read an Adder program and roughly follow it.
A Rust programmer should look at the same program and recognize the enums, matches,
traits, and iterator chains they love — and notice, with relief, that there isn't a
single lifetime annotation.
