# Adder — Language Reference (Working Draft)

This is the concrete-syntax and semantics draft. Everything here is a **proposal**
meant to be iterated section by section. Examples favor showing the *feel* of the
language over exhaustive formal rules.

Guiding constraints (see [00-design-principles.md](00-design-principles.md)):
a modern Python successor — indentation syntax, strong static typing with heavy
inference, GC, traits without inheritance, mutable-by-default, Kotlin-style
nullability, hybrid error handling, and a standing preference for **English words
over symbols** so the language reads well to relative beginners.

---

## 1. Lexical structure

- **Encoding:** UTF-8 source. Identifiers are Unicode letters/digits/`_`, not
  starting with a digit.
- **Blocks** are introduced by a trailing `:` and an indented suite, exactly like
  Python. Indentation is significant (proposed: 4 spaces; tabs rejected). A block may
  be written inline after the colon for a single statement (`if ready: go()`).
- **Statements** end at a newline. A trailing open bracket/paren/brace continues a
  logical line (no line-continuation backslash needed inside brackets).
- **Comments:** `#` to end of line. Doc comments: `##` immediately above a
  declaration (attaches to it for tooling).
- **Keywords (proposed):** `fn val struct enum trait impl for in to while if
  elif else match return returns break continue import from as and or not is try
  panic true false null Self`
  - `mut` is **not** a keyword — variables are mutable by default; `val` opts *into*
    immutability.
  - There is no `pub`/`export` keyword and (for now) no `private`: every top-level
    item is importable. A privacy modifier is a planned, additive feature (see §12).

### Operators — words first

| Concern | Adder | Deliberately **not** |
| --- | --- | --- |
| Logical and/or/not | `and` `or` `not` | `&&` `\|\|` `!` |
| Equality | `==` `!=`, or the readable `is` / `is not` | — |
| Null check | `x is null` / `x is not null` | `x == null` (allowed but discouraged) |
| Error propagation | `try expr` | `expr?` |
| Null fallback | `x.or_else(default)` | `x ?: default` |
| Anonymous function | `x -> x + 1` | `\|x\| x + 1` |
| Return type | `fn f(...) returns Int:` | `fn f(...) -> Int:` |
| Comparison / math | `< <= > >= + - * / % **` | (kept — universally understood) |

> `is` / `is not` here mean **value equality** (and read naturally with `null`,
> `true`, `false`). Unlike Python, `is` is *not* identity comparison — that footgun
> is removed. `==` / `!=` remain as equivalents for those who prefer them.
>
> Note `->` is now reserved for lambdas only; the result type of a function uses the
> `returns` keyword, so the arrow never means two things.
>
> **The nullability sigils are a deliberate exception to "words win."** `T?` and `?.`
> attach to *types* and *member access*, where an English word would not compose
> cleanly (`String or null`, `maybe and then .length`). They are kept terse on
> purpose; this is the one carve-out from the symbols-to-words rule.

---

## 2. Variables and mutability

Mutable by default. A bare binding is reassignable:

```adder
count = 0            # inferred Int, mutable
count = count + 1    # fine

name: String = "Ada" # explicit type, still mutable
```

Opt into immutability with `val` (a single-assignment binding):

```adder
val pi = 3.14159     # cannot be reassigned
pi = 3.0             # ERROR: cannot reassign a `val`
```

> **Design note.** Because mutation is invisible at the use site, the type system is
> the safety net: a binding's *type* is fixed at first assignment even though its
> value is not. `count = "hi"` after `count = 0` is a type error.

---

## 3. The type system at a glance

- **Static, strong, inferred.** Every expression has a type known at compile time.
- **The annotation boundary is the function signature.** Inside a function, almost
  nothing is annotated — local variables, literals, and lambdas all infer their
  types. At a `fn`, types are **required**: both parameter types and the `returns`
  result type (see §4).
  - This is the honest version of "you rarely write types": you write them *at
    signatures*, not on every line. Inferring return types (and eventually local
    parameter types) so you could drop them on private helpers is planned ergonomics,
    not a v1 feature.
  - A local binding is only annotated when inference genuinely can't determine the
    type (e.g. an empty collection with no later evidence): `xs: List[Int] = []`.
- **No `Any` by accident.** An explicit escape hatch may exist later, but it is never
  inferred. This is the core "scales better than Python" property.

### Built-in types (proposed)

| Type | Notes |
| --- | --- |
| `Int` | **Arbitrary precision** — never overflows |
| `Float` | 64-bit IEEE-754 |
| `Bool` | `true` / `false`. Conditions must be `Bool` (no truthiness coercion) |
| `String` | UTF-8, immutable value |
| `Char` | A single Unicode scalar |
| `List[T]` | Growable ordered sequence |
| `Map[K, V]` | Hash map |
| `Set[T]` | Hash set |
| `(A, B, ...)` | Tuples (fixed-size, heterogeneous) |
| `T?` | Nullable `T` (see §8) |
| `Result[T, E]` | Recoverable error carrier (see §9) |
| `()` | Unit / "no value" |

### Literals & interpolation

```adder
n      = 42
big    = 1_000_000_000_000_000_000_000   # still an exact Int
ratio  = 3.14
flag   = true
text   = "hello"
ch     = 'a'
items  = [1, 2, 3]              # List[Int]
pair   = (1, "one")             # (Int, String)
scores = {"ada": 10, "bob": 7} # Map[String, Int]  (key: value)
unique = {1, 2, 3}             # Set[Int]           (bare elements)
empty_map = {}                 # empty Map (the {} default, as in Python)
empty_set = Set()             # empty Set is constructed explicitly (avoids ambiguity)
nothing = null

name = "Ada"
greeting = "Hello, {name}!"     # interpolation with { }
math = "2 + 2 = {2 + 2}"
literal_brace = "use {{ and }}" # escaped braces
```

> **`{}` disambiguation.** A `{ }` with `key: value` entries is a `Map`; with bare
> elements it's a `Set`. The empty `{}` is an empty **Map** (matching Python); an
> empty `Set` is written `Set()`.

---

## 4. Functions

A function signature is **fully annotated** — parameter types and a `returns` clause
for the result. The signature is the one place types are mandatory; everything inside
the body is inferred (see §3).

```adder
fn add(a: Int, b: Int) returns Int:
    return a + b

# Implicit return of the final expression is allowed:
fn square(x: Int) returns Int:
    x * x

# A function that returns nothing omits the `returns` clause:
fn log(msg: String):
    print(msg)
```

> *(Ergonomics note: requiring an explicit `returns` type is a deliberate v1
> simplification. Inferring the return type from the body — so you could drop it on
> local helpers — is a planned later improvement, not a v1 feature.)*

### Default and named arguments *(proposed for v1)*

```adder
fn greet(name: String, greeting: String = "Hello") returns String:
    return "{greeting}, {name}!"

greet("Ada")                       # "Hello, Ada!"
greet("Ada", greeting = "Hi")      # named argument
```

### Anonymous functions (lambdas)

Single-expression lambdas use an arrow; the parameter list drops the parens when
there's exactly one:

```adder
inc = x -> x + 1
add = (a, b) -> a + b

nums.map(n -> n * n)
```

Lambdas are single-expression (like Python's `lambda`). For anything multi-statement,
define a named `fn` — it keeps inline code readable:

```adder
fn clean(item: String) returns String:
    trimmed = item.trim()
    return trimmed.to_upper()

names.map(clean)
```

Closures capture their environment by reference (the GC keeps captures alive).

---

## 5. Structs

```adder
struct Point:
    x: Float
    y: Float

impl Point:
    fn distance_to(self, other: Point) returns Float:
        dx = self.x - other.x
        dy = self.y - other.y
        return (dx * dx + dy * dy).sqrt()

p = Point(x: 1.0, y: 2.0)   # named construction (preferred)
q = Point(3.0, 4.0)         # positional also allowed
d = p.distance_to(q)
```

A struct body holds only fields; methods live in an `impl` block (§7) — there is one
way to add a method. Fields are mutable by default (`p.x = 5.0`), and a method mutates
its receiver through `self` (`self.x = 5.0`). *(Open question: per-field `val`.)*

---

## 6. Enums (algebraic data types) and pattern matching

The centerpiece expressive feature — the main tool for "making illegal states
unrepresentable" as a program grows. Variants may carry data (named or positional):

```adder
enum Shape:
    Circle(radius: Float)
    Rectangle(width: Float, height: Float)
    Empty                       # data-less variant

enum Json:
    Null
    Bool(Bool)
    Number(Float)
    Text(String)
    Array(List[Json])
    Object(Map[String, Json])
```

### `match` is an expression and is exhaustive

Arms use the same `:` block syntax as everything else (inline or indented). No
special arrow:

```adder
fn area(s: Shape) returns Float:
    return match s:
        Circle(r):       3.14159 * r * r
        Rectangle(w, h): w * h
        Empty:           0.0
        # Omitting a variant is a COMPILE ERROR (exhaustiveness).
```

Patterns can destructure, bind, guard, and use literals / wildcards:

```adder
match value:
    0:            "zero"
    1 or 2 or 3:  "small"      # `or` reads naturally in patterns too
    n if n < 0:   "negative"
    _:            "big"
```

An indented block works when an arm needs more than one line:

```adder
match s:
    Circle(r):
        d = 2.0 * r
        log("circle, diameter {d}")
        3.14159 * r * r
    _:
        0.0
```

Destructuring also works in `val` / `for` / function parameters:

```adder
val (a, b) = (1, 2)
Point(x: px, y: py) = p
for (key, value) in scores.items():
    print("{key} = {value}")
```

---

## 7. Traits (shared behavior, no inheritance)

A trait declares method signatures and optional default methods:

```adder
trait Drawable:
    fn area(self) returns Float

    ## default method built on the required ones
    fn describe(self) returns String:
        return "a shape with area {self.area()}"
```

Implement a trait for a type with `impl ... for`:

```adder
impl Drawable for Circle:
    fn area(self) returns Float:
        return 3.14159 * self.radius * self.radius
    # describe() is inherited from the trait default

impl Drawable for Rectangle:
    fn area(self) returns Float:
        return self.width * self.height
```

Inherent methods (not tied to a trait) live in a plain `impl`:

```adder
impl Circle:
    fn diameter(self) returns Float:
        return self.radius * 2.0
```

Traits are the **only** polymorphism mechanism — there is no class inheritance.
`Self` refers to the implementing type inside a trait/impl.

> *(Open question: associated types/constants. Proposed: defer, then add `type Item`
> style associated types.)*

### 7.1 Automatically provided traits (the "it just works" defaults)

In Python, every object prints and compares out of the box. Adder keeps that feel
with a **hybrid** rule: the traits where the right behavior is obvious are derived
**automatically**; the one where it requires a human decision is **opt-in**.

#### Automatic — derived structurally, no annotation

For every `struct` and `enum` you declare, the compiler synthesizes:

| Trait | What you get | Spelling |
| --- | --- | --- |
| `Eq` | value equality / inequality, compared field-by-field (recursively) | `a == b`, `a != b`, `a is b` |
| `Hash` | usable as a `Map` key or `Set` element | `{p: 1}`, `{p}` |
| `Show` | a readable default text form for printing & interpolation | `print(p)`, `"{p}"` |

```adder
struct Point:
    x: Int
    y: Int

a = Point(x: 1, y: 2)
b = Point(x: 1, y: 2)

a == b              # true  — structural equality, free
seen = {a}          # Hash, free — Point works as a Set element
print(a)            # Point(x: 1, y: 2)  — default Show, free
"here: {a}"         # "here: Point(x: 1, y: 2)"
```

These derive **recursively**: a type gets them only if all its fields/variant
payloads have them, exactly when it makes sense.

#### Opt-in — because the compiler shouldn't guess

`Ord` (ordering: `<`, `<=`, `>`, `>=`, and sortability) is **not** automatic. There
is no single obvious order for a multi-field type — should a `Person` sort by name or
by age? Guessing silently is the kind of footgun Adder avoids. You request it, and
the field order in the declaration defines the sort order (lexicographic):

```adder
derive Ord                  # opt in; sorts by `last`, then `first`, then `age`
struct Person:
    last: String
    first: String
    age: Int

people.sort()               # now legal and unambiguous
```

#### Overriding a default

An automatic trait is only a *default*. Writing your own `impl` for a type replaces
the synthesized one — e.g. a case-insensitive `Eq`, or a prettier `Show`:

```adder
impl Show for Point:
    fn show(self) returns String:
        return "({self.x}, {self.y})"

print(Point(x: 1, y: 2))    # (1, 2)
```

> **Why this split.** Equality, hashing, and printing have one obviously-correct
> structural meaning, so making you ask for them would be pure ceremony — un-Pythonic.
> Ordering encodes a real decision, so it stays explicit. *(Open questions: the exact
> `derive` spelling — annotation vs `derive(Ord)` call form; whether `Show` should
> distinguish a developer-facing debug form from a user-facing display form, à la
> Rust's `Debug`/`Display`.)*

---

## 8. Nullability (the "easy Option")

Absence is modeled in the type system with a trailing `?`. `T` and `T?` are distinct
types; you cannot use a `T?` as a `T` without handling `null`.

```adder
name: String   = "Ada"   # never null
maybe: String? = null    # may be null

maybe.length             # ERROR: maybe could be null
```

### Smart-narrowing is the primary, most readable path

After a null check, the variable narrows to its non-null type inside the branch:

```adder
if maybe is not null:
    print(maybe.length)   # OK: maybe is `String` here, not `String?`
```

`match` handles nullables too, with `null` as a pattern:

```adder
match maybe:
    null: print("nothing")
    s:    print("got {s} of length {s.length}")
```

### Compact operators (secondary sugar)

```adder
maybe?.length              # safe call: yields Int? (null if maybe is null)
maybe.or_else("anonymous") # null fallback: the value, or the default if null
maybe.expect("name was required")  # assert non-null; panics (with reason) if null
```

> **`or_else`, not `else`.** The fallback is a method, deliberately, so the keyword
> `else` keeps a single meaning (conditionals only). `maybe.or_else("anon")` is also
> discoverable via autocomplete and chains like any other method.

> **Why not `Option<T>`?** Same guarantee — the checker forces you to handle absence —
> with far less wrapping/unwrapping ceremony. An `Option[T]` library type may still
> exist for when a first-class value is needed (e.g. storing absence generically in a
> collection), and converts to/from `T?`.

---

## 9. Error handling (hybrid)

Two distinct concerns, two mechanisms — a clear division of labor that prevents the
"every failure is an exception three frames down" problem Python codebases hit:

| Mechanism | Meaning |
| --- | --- |
| `T?` | the value might legitimately be **absent** |
| `Result[T, E]` | an operation can **fail with a reason** |
| `panic` | this should never happen — it's a **bug** |

### Recoverable failures → `Result[T, E]`

```adder
enum Result[T, E]:      # built-in / prelude
    Ok(T)
    Err(E)

fn parse_int(s: String) returns Result[Int, ParseError]:
    ...
```

Propagate with `try` (returns early on `Err`, unwraps on `Ok`):

```adder
fn sum_file(path: String) returns Result[Int, IoError]:
    text  = try read_file(path)        # unwrap Ok, or return the Err
    total = 0
    for line in text.lines():
        total = total + try parse_int(line)
    return Ok(total)
```

Handle explicitly with `match` when you want to:

```adder
match parse_int(input):
    Ok(n):  print("parsed {n}")
    Err(e): print("bad input: {e}")
```

### Unrecoverable bugs → `panic`

```adder
fn get(self, i: Int) returns T:
    if i < 0 or i >= self.len():
        panic("index {i} out of bounds")
    ...
```

`panic` aborts with a message + backtrace. It is for broken invariants, **not**
expected failure modes (those use `Result`). The `.expect(...)` method on a nullable
(§8) is a `panic` in disguise.

---

## 10. Generics

Type parameters in `[ ]` (no `< >` ambiguity); bounds via `:`.

```adder
fn first[T](xs: List[T]) returns T?:
    if xs.is_empty():
        return null
    return xs[0]

fn max[T: Ord](a: T, b: T) returns T:
    return a if a > b else b      # ternary form (proposed)

struct Stack[T]:
    items: List[T]

impl Stack[T]:
    fn push(self, x: T):
        self.items.append(x)

    fn pop(self) returns T?:
        return self.items.pop_last()   # T? when empty
```

Multiple bounds: `T: Ord and Display` *(proposed — using `and` to stay word-based,
rather than Rust's `+`)*. Generic traits/impls follow the same syntax.

---

## 11. Collections, iterators, and comprehensions

### Iterator pipelines (lazy)

```adder
nums = [1, 2, 3, 4, 5, 6]

result = nums
    .filter(n -> n % 2 == 0)
    .map(n -> n * n)
    .sum()                       # 4 + 16 + 36 = 56
```

Chain `map`, `filter`, `take`, `skip`, `enumerate`, `zip`, `fold`, `reduce`, `any`,
`all`, `find`, `collect`, ... Intermediate steps are lazy; a terminal operation
(`sum`, `collect`, a `for` loop) drives them.

### Comprehensions (Python-style sugar)

```adder
squares = [x * x for x in nums]
evens   = [x for x in nums if x % 2 == 0]
lookup  = {name: name.length for name in names}
```

### Loops and ranges

```adder
for i in 0..10:        # 0..9 (half-open)
    print(i)

for i in 0..=10:       # 0..10 (inclusive)
    print(i)

while not done:
    done = step()
```

`break` and `continue` behave as expected.

> *(Open question: offer word-based ranges `0 to 10` / `0 through 10` for beginners,
> alongside or instead of `..` / `..=`. `to` is reserved as a keyword to keep this
> open.)*

---

## 12. Modules and imports

Adder keeps Python's familiar import ergonomics but fixes the thing that bites at
scale: the module graph is resolved **statically** — no import-time surprises, no
runtime monkey-patching, misspelled imports fail before the program runs.

For now there is **no visibility system**: every top-level item in a module is
importable. A `private` modifier (to confine items to their own module, and to
encapsulate struct fields/methods) is a planned, purely additive feature, left out of
v1 to keep the model small. Until then, treat a leading-underscore name (`_helper`)
as a *convention* for "internal," exactly as in Python.

### 12.1 Files, modules, and packages

- **One file is one module.** A file `geometry/shapes.adder` is the module
  `geometry.shapes`. The dotted name mirrors the path on disk — no separate manifest
  needed to know what a module is called.
- **A directory is a package: just a namespace.** `geometry/` groups the modules
  beneath it. There is **no special package file** (no `__init__.py` equivalent) — a
  folder is importable simply by existing. You import the submodules directly.

```
myapp/
  main.adder             # module `myapp.main`      (program entry point)
  geometry/
    shapes.adder         # module `myapp.geometry.shapes`
    transforms.adder     # module `myapp.geometry.transforms`
```

### 12.2 Importing

Python-style, with the same forms:

```adder
import math                              # bind the module; use as `math.sqrt(x)`
import geometry.shapes                   # nested; use as `geometry.shapes.Circle`
import geometry.shapes as geo            # aliased module

from math import sqrt, pi                # bind names directly
from geometry.shapes import Circle, Rectangle as Rect
from geometry.shapes import *            # bind every name (discouraged)
```

Imports are resolved at **compile time** against the module graph. An unresolved or
misspelled import fails before the program runs, rather than at the moment of use.

### 12.3 Resolution & roots

- Imports are **absolute** from a project root by default. The root is the directory
  containing the project manifest *(name TBD — see open questions)*; everything under
  it is addressable by its dotted path.
- *(Proposed)* a leading dot means **relative to the current package**:
  `from .transforms import rotate` imports a sibling module. Kept optional and
  secondary, since absolute paths read more clearly for beginners.
- The standard library is a reserved top-level namespace (e.g. `std.collections`),
  so user packages never shadow it.

### 12.4 Program entry point

Running a file executes its top-level statements, then calls its `main` function if
one is defined:

```adder
# myapp/main.adder
fn main():
    print("hello")
```

There is no `if __name__ == "__main__"` ceremony — being run vs. being imported is a
property the runtime already knows, and a module that is merely imported never has
its `main` invoked.

### 12.5 No cyclic imports

A cycle in the module graph is a **compile-time error** with the cycle path
reported, rather than Python's partially-initialized-module hazard. Cycles are
almost always a design smell; forbidding them keeps load order trivial to reason
about. *(Open question: allow cycles that only involve type references, which a
static checker can resolve safely.)*

---

## 13. A taste of Adder (putting it together)

```adder
## A tiny expression evaluator.

enum Expr:
    Num(Float)
    Add(Expr, Expr)
    Mul(Expr, Expr)
    Div(Expr, Expr)

enum EvalError:
    DivideByZero

fn eval(e: Expr) returns Result[Float, EvalError]:
    return match e:
        Num(n):    Ok(n)
        Add(a, b): Ok(try eval(a) + try eval(b))
        Mul(a, b): Ok(try eval(a) * try eval(b))
        Div(a, b):
            divisor = try eval(b)
            if divisor == 0.0:
                Err(EvalError.DivideByZero)
            else:
                Ok(try eval(a) / divisor)

fn main():
    # (1 + 2) * 3
    program = Mul(Add(Num(1.0), Num(2.0)), Num(3.0))
    match eval(program):
        Ok(value): print("= {value}")
        Err(e):    print("error: {e}")
```

What's present: enums, exhaustive match, `Result` + `try`, interpolation, a
word-based feel. What's absent: lifetimes, type annotations on locals, inheritance,
`Option` wrapping, and cryptic sigils.

---

## 14. Index of open questions

1. **Return-type inference ergonomics** — let private helpers drop the `returns`
   clause (and eventually param types) once inference is stronger. Deferred, not v1.
2. **Default/named arguments** — confirm for v1 (leaning defer; see MVP scope).
3. **Per-field immutability** on structs (`val` fields).
4. **Associated types/constants** on traits.
5. **Word-based ranges** — `0 to 10` vs `0..10`.
6. **Derive residuals (§7.1)** — exact `derive` spelling (annotation vs `derive(Ord)`
   call form); whether `Show` splits into debug vs display forms.
7. **Multiple trait bounds** — `T: Ord and Display` vs another spelling.
8. **`try` keyword vs Python's `try:` block** — revisit when `Result` lands; same
   word, different grammar.
9. **`is` as value-equality** — revisit whether it earns its keep alongside `==`
   given Python readers expect identity.
10. **Concurrency (v2)** — keep the surface language from foreclosing async/threads.
11. **Module residuals (§12)** — project manifest/root name; whether to allow
    type-only import cycles; privacy modifier (`private`) when added later.

*Resolved in the MVP-scoping review: MVP = typed-lite (tree-walker + exhaustiveness +
null-narrowing, see [02-mvp-scope.md](02-mvp-scope.md)); function signatures fully
annotated (params + `returns`); null-fallback is `x.or_else(y)`; `returns` keyword
frees `->` for lambdas only.*
