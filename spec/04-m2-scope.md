# Adder — M2 Scope (Milestone 2)

M1 answered *"does Adder feel right to write?"* for the **algebraic core** — enums,
exhaustive `match`, structs, methods, nullability. M2 answers the next question:

> **Does Adder's data-flow feel right?** — do iterator pipelines and comprehensions
> read like Python while carrying the Rust expressiveness, in the same indentation-
> structured language?

M1 has `for` loops but *none* of the functional toolkit. M2 fills that gap, plus the
ergonomic sugar that real programs reach for first. As with M1
([02-mvp-scope.md](02-mvp-scope.md)), everything not listed under "In scope" is
deferred *without redesign* — the cuts layer on cleanly.

## Strategy: stay "typed-lite"

M2 keeps M1's posture: a **tree-walking interpreter plus exactly the two static
checks** (match-exhaustiveness, null-narrowing). The full type checker + inference
remains **M3**. New features are runtime-checked in the tree-walker; the two existing
checks are *extended* to understand the new syntax (guards/or-patterns for
exhaustiveness; `?.`/`.expect` for null-narrowing) — they do not become a general
checker.

Two consequences of staying typed-lite:

- **Function types are parsed, not checked.** A higher-order signature like
  `f: (Int) -> Int` is expressible and documents intent, but M2 does **not** verify
  that a passed lambda's body matches it. Calling with the wrong arity is a *runtime*
  error (closures already enforce this — see [interp.rs](../src/interp.rs)).
- **Iterators are eager, not lazy.** Spec §11 calls for lazy pipelines; M2 ships
  **eager** ones (each stage returns a fresh `List`). This matches how the runtime
  already behaves — ranges (`0..n`) are *already* materialized to a `List`
  ([interp.rs](../src/interp.rs) `eval_range`), so pipeline methods over a range come
  for free. Laziness is real interpreter work and is deferred to M3.

> Rationale: M2's thesis is *feel*, not performance. Eager pipelines read identically
> to lazy ones at the call site; the user can't tell the difference in a 50-line
> program, and laziness can be added later behind the same surface.

## In scope (Milestone 2)

### Functional / data core (the headline)

**Passable lambdas + function types**
- Function-type syntax `(T1, T2, …) -> R` (zero-arg: `() -> R`), usable anywhere a
  type appears — parameter types especially: `fn apply(f: (Int) -> Int, x: Int)`.
- **Declarations adopt the same `->`** for their result type (`fn f(x: Int) -> Int:`),
  replacing M1's `returns` keyword. One arrow now types every function-like result —
  named `fn`, lambda, and function type — read uniformly as "maps to"; omit it for unit.
  *(Decided in M2 scoping; resolves the function-type-spelling open question. The
  `returns` keyword is dropped from the language. This requires migrating the M1
  surface — see the implementation sweep.)*
- Lambdas (`x -> e`, `(a, b) -> e`) graduate from "evaluated head-start" to a
  **validated, passable** feature: a lambda or a named `fn` can be passed to a
  function-typed parameter and called. Single-expression bodies only (multi-statement
  → name a `fn`), exactly as spec §4.
- Closures capture by reference (already implemented).

**Iterator pipelines (eager)** — a built-in **method table** on the iterable types
(`List`, `Map`, `Set`, and ranges, which are lists). Each transforming stage returns a
new `List`; terminal stages return a scalar:

- transforms: `map`, `filter`, `take`, `skip`, `enumerate`, `zip`, `reverse`, `sorted`
- terminals: `fold`, `reduce`, `sum`, `count`/`len`, `any`, `all`, `find`, `contains`,
  `is_empty`, `first`, `last`, `min`, `max`, `collect`
- in-place (List): `append`, `pop_last`, indexing (already present)

(The exact final method set is settled during implementation; this is the target
surface. `sorted`/`min`/`max` use structural ordering on comparable values — the
opt-in `Ord` *derive* for user types is M3, but built-in scalars order fine.)

**Comprehensions** — sugar over `for`:
```adder
squares = [x * x for x in 1..=5]            # list comprehension
evens   = [x for x in xs if x % 2 == 0]     # with a filter
lookup  = {name: name.length for name in names}   # map comprehension
seen    = {x % 3 for x in xs}               # set comprehension
```
The loop variable is scoped to the comprehension.

**`Map` and `Set`** (cut from M1, landing now) — spec §3:
- Literals: `{k: v, …}` is a `Map`; `{x, …}` is a `Set`; `{}` is an empty **Map**; an
  empty set is `Set()`.
- Methods: `Map` — `get` (yields `V?`), `insert`, `contains`/`has`, `keys`, `values`,
  `items` (yields `List[(K, V)]`), `len`. `Set` — `insert`, `contains`, `union`,
  `intersect`, `len`.
- Keys/elements must be hashable. In typed-lite, hashing is **structural** (reusing
  the value-walking that already backs `==`/`Show`); a non-hashable key (a closure, a
  function) is a *runtime* error. (`Float` keys are allowed but `NaN` is its usual
  footgun — documented, not specially blocked.)

### Cheap ergonomic wins (additive, low risk)

- **Match guards** — `pattern if cond:`. A guarded arm does **not** count toward
  exhaustiveness (the check already owned by [checks.rs](../src/checks.rs) is extended
  to know this).
- **Or-patterns** — `1 or 2 or 3:` and `.A or .B:`; understood by exhaustiveness.
- **Nested destructuring** — sub-patterns may themselves be variant/tuple patterns
  (`SubPattern` becomes recursive). Patterns are no longer flat.
- **`?.` safe-call and `.expect(msg)`** — `x?.field` yields `null` if `x` is null;
  `x.expect("…")` asserts non-null and `panic`s with the message otherwise. Both are
  recognized by the null-narrowing check as valid ways to handle a `T?`.
- **Tuples** — `(a, b)` literals, `(A, B)` types, and tuple patterns. `(expr)` stays
  pure grouping; a tuple needs at least one comma. Pairs naturally with `map.items()`
  and `for (k, v) in …`.
- **Default + named function arguments** — `fn greet(name: String, greeting: String =
  "Hello")`; call as `greet("Ada")` or `greet("Ada", greeting: "Hi")`. (Named args
  already parse for construction; M2 makes them valid for function calls too.)
- **Destructuring binders** in `val`/`for` — `val (a, b) = pair`, `for (k, v) in
  map.items()`.

## Contract changes (the M1 AST is no longer frozen)

M1's [ast.rs](../src/ast.rs) was declared "complete for M1." M2 deliberately extends
it. The additions, so the ripple is explicit and reviewable:

| Node | Addition |
| --- | --- |
| `BaseType` | `Fn { params, ret }` (function type), `Tuple(Vec<Type>)` |
| `ExprKind` | `Map`, `Set`, `Tuple`, `Comprehension { … }`; `?.` (a flag on `Member`/`Call`) |
| `MatchArm` | `guard: Option<Expr>` |
| `PatternKind` | `Or(Vec<Pattern>)`, `Tuple(Vec<Pattern>)`; `SubPattern` → recursive `Pattern` |
| `Param` | `default: Option<Expr>` |
| `Value` (interp) | `Map`, `Set`, `Tuple`; a built-in **method dispatch** path for `List`/`Map`/`Set`/`String` (today `call_method` only handles `Struct`/`Enum`) |

## Deferred (Milestone 3+)

| Feature | Why it's safe to defer past M2 | Phase |
| --- | --- | --- |
| **`Result` + `try`** | `panic` still covers failure for now; `try`'s early-return desugaring is self-contained and best done as its own slice | **M2-late / M3 (recommended next)** |
| Traits / `impl … for` / `Self` / default methods | Inherent methods + the structural auto-`Eq`/`Show` already in the runtime carry the feel; trait *resolution* is the biggest build cost | M3 |
| Opt-in `derive Ord` + user `.sort()` | Built-in scalars order without it; derive needs the trait machinery | M3 |
| Full type checker + inference; user generics (`[T]`) + bounds | The two checks carry the thesis; generics need traits + a checker | M3 |
| Lazy iterator pipelines | M2 ships eager; laziness is real interpreter work behind the same surface | M3 |
| Modules / imports / package graph | Single-file programs still validate M2's data-flow feel | M3 |
| `Char` type | Strings suffice for M2 | M3 |
| Word-based ranges, `private`, per-field `val` | Additive; nothing depends on them | later |
| Macros, concurrency, native/bytecode backend | Out of scope by design | v2+ |

> **Note on Result/try.** It was the runner-up for the M2 headline. It is *not* in M2,
> but it is the recommended **immediately-next** milestone: it's self-contained
> (a generic prelude enum + `try` early-return) and completes the §9 error story.

## M2 showcase program

```adder
## M2 showcase: data flows the Adder way.

fn pipeline(xs: List[Int], keep: (Int) -> Bool, f: (Int) -> Int) -> Int:
    return xs.filter(keep).map(f).sum()      # higher-order: lambdas passed to a fn

fn main():
    nums = [1, 2, 3, 4, 5, 6]

    # iterator pipeline with passable lambdas
    result = pipeline(nums, n -> n % 2 == 0, n -> n * n)
    print("sum of even squares = {result}")  # = 56

    # comprehension with a filter
    squares = [x * x for x in 1..=5 if x != 3]
    print(squares)                           # [1, 4, 16, 25]

    # Map + tuples via .items(), with a default-arg helper and a guard
    prices = {"apple": 3, "pear": 2, "fig": 5}
    for (name, cost) in prices.items():
        label = match cost:
            c if c >= 5: "pricey"
            _:           "cheap"
        print("{name}: {label}")

    print("total = {prices.values().sum()}") # = 10
```

This exercises the whole M2 feel: function-typed parameters, lambdas passed to a `fn`,
an eager `filter`/`map`/`sum` pipeline, a list comprehension with a filter, a `Map`
literal, tuple destructuring in a `for`, and a guarded `match` arm.

## Definition of done for Milestone 2

- The showcase above (shipped as [`examples/m2_showcase.adr`](../examples/m2_showcase.adr),
  guarded by `tests/m2_showcase.rs`) runs and prints `sum of even squares = 56`,
  `[1, 4, 16, 25]`, the three `name: label` lines, and `total = 10`.
- A lambda passed to a function-typed parameter is called; a wrong-arity call is a
  **runtime** error.
- `xs.filter(p).map(f).fold(init, g)` runs end-to-end over a `List` and a range.
- A `Map` literal round-trips through `.keys()` / `.values()` / `.items()`; a `Set`
  literal deduplicates; `{}` is a `Map` and `Set()` is an empty set.
- A list comprehension with an `if` filter produces the expected list.
- A **match guard** parses, runs, and is correctly treated as non-exhaustive by the
  exhaustiveness check (removing the `_` after a guarded arm is a compile-time error);
  an **or-pattern** is accepted and counts its variants toward exhaustiveness.
- **Nested destructuring** binds inner payloads; a **tuple** constructs and
  destructures.
- `x?.field` yields `null` on a null receiver; `x.expect("msg")` panics with `msg`;
  both satisfy the null-narrowing check.
- **Default** and **named** function arguments resolve correctly.
- Everything M1 did still passes (the M1 acceptance suite is unchanged).
