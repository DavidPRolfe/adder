# Adder — M3 Scope (Milestone 3)

M1 answered *"does Adder's **algebraic core** feel right?"* — enums, exhaustive
`match`, structs, methods, nullability. M2 answered *"does Adder's **data-flow** feel
right?"* — iterator pipelines, comprehensions, `Map`/`Set`, tuples. M3 answers the next
question:

> **Does Adder's *abstraction* and *error model* feel right?** — do traits carry shared
> behavior the way Rust's do, and does `Result`/`try` read as cleanly as the spec
> promises, in the same indentation-structured, typed-lite language?

These are the last two unproven pieces of the **"expressive like Rust"** pillar
([00-design-principles.md](00-design-principles.md) §2). M1+M2 delivered the Python
surface and the *algebraic* half of Rust-expressiveness; what has never been felt is
**traits** — the language's *only* polymorphism mechanism (§7 of the reference) — and
the **`Result` + `try`** error model, a locked-in decision the project has substituted
`panic` for since M1. As with M1 ([02-mvp-scope.md](02-mvp-scope.md)) and M2
([04-m2-scope.md](04-m2-scope.md)), everything not listed under "In scope" is deferred
*without redesign*.

## Strategy: stay "typed-lite" for one more milestone

M3 keeps the M1/M2 posture: a **tree-walking interpreter plus exactly the two static
checks** (match-exhaustiveness, null-narrowing). The full type checker + inference is
its own milestone — see **M4**, below — and M3 does **not** start it. New features are
runtime-checked in the tree-walker; the two existing checks are essentially unchanged
(the only ripple is that exhaustiveness must recognize the prelude `Result` enum like
any other enum — it already handles enums, so this is free).

Three consequences of staying typed-lite, each a direct extension of a precedent M2
already set:

- **Traits dispatch at runtime, not by static resolution.** Inherent-method dispatch
  already exists in the interpreter ([src/interp/](../src/interp/) `call_method`); a
  trait `impl` is an extension of that dispatch table keyed by `(type, method)`, with
  the trait's default-method body as the fallback. A **trait-typed parameter**
  (`fn total_area(shapes: List[Area])`) is *parsed* as a type and accepted at runtime
  for any value whose type implements the trait — it reads as bounded, and runs as
  dynamic dispatch. A receiver that does not implement a called method is a **runtime**
  error, exactly as M2 made a wrong-arity lambda call a runtime error.
- **Generics are parsed, not checked — and erased at runtime.** This is the *exact*
  precedent of M2's "function types are parsed, not checked." `fn first[T](xs: List[T])
  -> T?`, `struct Stack[T]`, and bounds (`[T: Ord]`) all parse and document intent, but
  the tree-walker erases `T` and does not verify bounds. A bound violation (calling `>`
  on a value that has no ordering) surfaces as a **runtime** error. Parsing generics is
  what makes the prelude `Result[T, E]` spellable in user signatures.
- **`Result`/`try` is runtime control flow, not a checked effect.** `Result[T, E]` is a
  prelude generic enum (`Ok`/`Err`), seeded like `print`/`panic`. `try expr` is an
  early-return desugaring in the interpreter: evaluate `expr`; if `Err(e)`, return
  `Err(e)` from the enclosing function; if `Ok(v)`, the expression's value is `v`.
  Whether a `try` is well-typed against the enclosing `-> Result[_, _]` signature is an
  M4 concern; in M3 a `try` in a non-`Result` function is a **runtime** error.

> Rationale: M3's thesis is *feel*, not soundness. A reader cannot tell whether
> `shape.area()` dispatched statically or dynamically, nor whether `try` was
> type-checked — the surface is identical. Keeping M3 typed-lite buys the entire
> abstraction + error story for a fraction of the build cost of the checker, and the
> checker (M4) slots in behind the same surface.

## In scope (Milestone 3)

### Traits (the headline) — spec §7

**Trait declarations**
- `trait Name:` with **required** method signatures (a `fn` header with no body) and
  optional **default methods** (a full `fn` with a body, built on the required ones).
- Trait methods take `self` as the first parameter, like inherent methods.
- `Self` refers to the implementing type inside a trait or `impl`.

**Trait implementations**
- `impl Trait for Type:` (extends today's inherent `impl Type:`). An `impl` supplies the
  required methods; any default method it omits is inherited from the trait.
- A hand-written `impl` of an **auto-derived** trait (`Eq`/`Hash`/`Show`) *replaces* the
  synthesized one — e.g. a prettier `Show` (spec §7.1 "Overriding a default").
- **Trait-typed parameters** — `fn describe(d: Drawable)` / `List[Drawable]`: accept any
  value whose type implements the trait; the method call dispatches at runtime.

**Conformance is runtime, not a new check.** Calling a required method that an `impl`
forgot to define is a runtime error (the method is simply not in the dispatch table) —
the project keeps **exactly two** static checks until M4. Promoting trait-conformance to
a compile-time check is natural once the M4 checker exists.

### `Result` + `try` (the error model) — spec §9

- Prelude **`Result[T, E]`** enum with `Ok(T)` / `Err(E)`, seeded as ordinary bindings
  alongside `print`/`panic` (so it needs no special grammar and can be shadowed).
- **`try expr`** — unwrap `Ok`, or early-return the `Err` from the enclosing function.
- `match` over a `Result` is just enum matching, so the **exhaustiveness** check covers
  `Ok`/`Err` with no special-casing.
- This completes the §9 hybrid: `T?` for absence, `Result` for failure-with-reason,
  `panic` for bugs.

### `derive Ord` + sorting — spec §7.1

- Opt-in **`derive Ord`** annotation above a `struct`/`enum`; ordering is lexicographic
  by declaration order (fields top-to-bottom; for enums, declaration order of variants
  then payloads).
- Enables `<` / `<=` / `>` / `>=` and `.sort()` / `sorted` on user types. This *extends*
  the existing structural machinery — the value-walking that already backs `==`/`Hash`/
  `Show` ([src/interp/](../src/interp/) `show.rs`/`value.rs`) gains an ordering sibling,
  gated by the `derive`.
- `Eq`/`Hash`/`Show` remain **automatic** and structural (unchanged from today). M3
  settles the spec §7.1 open question in favor of the **annotation** spelling
  (`derive Ord`), not a `derive(Ord)` call form.

### Generics — parsed, not checked

- Type parameters in `[ ]` on `fn`, `struct`, `enum`, and `impl`:
  `fn first[T](xs: List[T]) -> T?`, `struct Stack[T]`, `impl Stack[T]:`,
  `impl[T] Ordered for Stack[T]:`.
- Bounds via `:` with `and` for multiples (`[T: Ord]`, `[T: Ord and Show]`), per
  spec §10 — parsed and documented, **not** enforced.
- Erased at runtime; this mirrors M2's function-type posture exactly. It exists in M3 so
  that `Result[T, E]` and generic containers are *writable*, not because the runtime does
  anything with `T`.

## Contract changes (the M2 AST extends again)

The additions to [ast.rs](../src/ast.rs), so the ripple is explicit and reviewable:

| Node | Addition |
| --- | --- |
| Top-level item | `Trait { name, type_params, methods }`; methods are either a **signature** (required) or a full `fn` (default) |
| `impl` item | gains `trait: Option<TypePath>` (the `… for` target) and `type_params` |
| `fn` / `struct` / `enum` decls | gain `type_params: Vec<TypeParam>` (a name + optional bound list) |
| `struct` / `enum` decls | gain `derives: Vec<Name>` (the `derive Ord` annotation) |
| `BaseType` | `SelfType` (the `Self` type); generic application (`NAME[...]`) already parses (M1) |
| `ExprKind` | `Try(Box<Expr>)` |
| Prelude (interp) | `Result` enum + `Ok`/`Err` seeded as bindings; **no new `Value` variant** — a `Result` is an ordinary `Value::Enum` |
| `Value` (interp) | a **trait dispatch table** keyed by `(type, method)` with default-method fallback; an **ordering** path over values gated by `derive Ord` |

## Deferred (Milestone 4+) — including the posture change

| Feature | Why it's safe to defer past M3 | Phase |
| --- | --- | --- |
| **Full type checker + inference** | The two checks + runtime enforcement carry the thesis through M3; the real checker is its own milestone — *"does Adder catch what it promises before running?"* | **M4 (the posture change)** |
| Generic **bound** checking; trait-conformance as a static check | Both need the checker; in M3 they are runtime errors | M4 |
| Lazy iterator pipelines | M2/M3 ship eager; laziness is a runtime change behind the same surface | M4 |
| Modules / imports (`import`, `from … import`, `as`) | Orthogonal to traits — single-file programs still validate M3's abstraction feel; this is its own *"does Adder scale across files?"* slice (spec §12) | M3.5 / M4 |
| Associated types/constants on traits; multiple-`for` impls | Single required/default methods carry the trait feel | M4+ |
| `Char` type; REPL; word-based ranges (`0 to 10`); `private` | Additive; nothing depends on them | later |
| Macros, concurrency, native/bytecode backend | Out of scope by design | v2+ |

> **Why modules are *not* in M3.** Traits do not depend on the module system and the
> module system does not depend on traits; bundling them only inflates the milestone.
> Modules are a clean parallel slice (spec §12 is fully designed) that can land before or
> after M4 without touching the M3 work.

## M3 showcase program

```adder
## M3 showcase: abstraction + the real error model.

trait Area:
    fn area(self) -> Float                      # required

    fn describe(self) -> String:                # default, built on the required method
        return "area = {self.area()}"

struct Circle:
    radius: Float

struct Rect:
    w: Float
    h: Float

impl Area for Circle:
    fn area(self) -> Float:
        return 3.14159 * self.radius * self.radius

impl Area for Rect:
    fn area(self) -> Float:
        return self.w * self.h

    fn describe(self) -> String:                # overrides the trait default
        return "rect {self.w} x {self.h} = {self.area()}"

enum ShapeError:
    NegativeSize

fn checked_rect(w: Float, h: Float) -> Result[Rect, ShapeError]:
    if w < 0.0 or h < 0.0:
        return Err(ShapeError.NegativeSize)
    return Ok(Rect(w: w, h: h))

fn scaled_area(w: Float, h: Float, k: Float) -> Result[Float, ShapeError]:
    r = try checked_rect(w, h)                   # try: unwrap Ok, or early-return Err
    return Ok(r.area() * k)

fn total_area(shapes: List[Area]) -> Float:      # trait-typed parameter (runtime dispatch)
    return shapes.map(s -> s.area()).sum()

derive Ord                                        # opt-in: order by points, then name
struct Score:
    points: Int
    name: String

fn main():
    shapes = [Circle(radius: 1.0), Rect(w: 2.0, h: 3.0)]
    for s in shapes:
        print(s.describe())                       # Circle: trait default; Rect: override
    print("total = {total_area(shapes)}")

    match scaled_area(2.0, 3.0, 2.0):
        Ok(a):  print("scaled = {a}")             # scaled = 12.0
        Err(e): print("rejected: {e}")

    match scaled_area(-1.0, 3.0, 2.0):
        Ok(a):  print("scaled = {a}")
        Err(e): print("rejected: {e}")            # rejected: ShapeError.NegativeSize

    board = [Score(points: 2, name: "z"), Score(points: 2, name: "a")]
    board.sort()                                  # derive Ord makes this legal
    print(board[0])                               # Score(points: 2, name: a)
```

This exercises the whole M3 feel: a trait with a required method and a default, two
`impl … for` blocks (one inheriting the default, one overriding it), runtime dispatch
through a trait-typed parameter, the prelude generic `Result[T, E]`, `try` early-return,
`match` over a `Result`, and `derive Ord` + `.sort()`.

## Definition of done for Milestone 3

- The showcase above (shipped as `examples/m3_showcase.adr`, guarded by
  `tests/m3_showcase.rs`) runs and prints the two `describe` lines, `total = …`,
  `scaled = 12.0`, `rejected: ShapeError.NegativeSize`, and the sorted `Score`.
- A `trait` with a **default method** is declared; an `impl … for` that omits the default
  **inherits** it, and an `impl` that defines it **overrides** it — verified by dispatch
  producing the two different `describe` outputs.
- A **trait-typed parameter** accepts values of two distinct implementing types and
  dispatches each correctly; calling a method the receiver's type does not implement is a
  **runtime** error.
- `Result[T, E]` round-trips: `try` unwraps `Ok` and early-returns `Err`, and a `match`
  over the `Result` is accepted by the **exhaustiveness** check (dropping the `Err` arm is
  a compile-time error).
- A `try` in a function whose result is not a `Result` is a **runtime** error.
- `derive Ord` makes `.sort()` / comparison legal on a user struct and orders it by
  declaration order; the same struct **without** the derive cannot be `.sort()`ed
  (runtime error).
- A **generic** signature (`fn first[T](xs: List[T]) -> T?`, `struct Stack[T]`) parses and
  runs with `T` erased; a bound (`[T: Ord]`) parses but is not enforced.
- An automatic `Eq`/`Hash`/`Show` can be **overridden** by a hand-written `impl` (a custom
  `Show` changes `print` output).
- Everything M1 and M2 did still passes (the M1 and M2 acceptance suites are unchanged).
