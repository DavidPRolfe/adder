# Adder — M3 Surface Grammar (Milestone 3)

A concrete grammar for the **Milestone 3** surface defined in
[06-m3-scope.md](06-m3-scope.md). Like [05-m2-grammar.md](05-m2-grammar.md) it is a set
of **deltas** — to the M1 grammar ([03-mvp-grammar.md](03-mvp-grammar.md)) as amended by
M2 ([05-m2-grammar.md](05-m2-grammar.md)). Only rules that change or are new appear here;
everything M1/M2 already defined (indentation, blocks, `if`/`while`/`for`, structs, enums,
inherent `impl`, ranges, arithmetic, the comparison ladder, strings/interpolation,
function/tuple types, comprehensions, `?.`, guards/or-/tuple patterns, default/named
args) stands unchanged unless a section below amends it. Read this file beside the M1 and
M2 grammars, not instead of them.

As before, the grammar is split into a **lexical** and a **syntactic** layer, and the two
static checks (match exhaustiveness, null-narrowing) are **semantic** — they run over the
tree this grammar produces and are not written here. M3 keeps the typed-lite posture: the
new **traits**, **generics**, and **`Self`** type are *parsed* and *interpreted* (trait
dispatch at runtime, generics erased), and `try` is a *runtime* early-return; none of them
becomes a full type check (see [06-m3-scope.md](06-m3-scope.md)).

The notation is identical to §0 of the M1 grammar:

| Form | Meaning |
| --- | --- |
| `"fn"` | a literal terminal (keyword or punctuation) |
| `NAME` | a named token from the lexical grammar (UPPER_CASE) |
| `a b` | `a` followed by `b` |
| `a \| b` | `a` or `b` |
| `( … )` | grouping |
| `[ a ]` | optional — zero or one |
| `{ a }` | repetition — zero or more |
| `a , …` | one-or-more `a` separated by `,` (shorthand for `a { "," a }`) |

---

## 1. Lexical deltas

### 1.1 Keywords

M3 **activates four reserved words** that M1/M2 listed as reserved-but-unused, and adds
one contextual keyword. The reserved set is now:

```
fn  val  struct  enum  trait  impl  for  return  derive
if  elif  else  match  while  for  in
break  continue  and  or  not  is  try
true  false  null  self  Self
```

- `trait` and `Self` are activated (§2, §3). `try` is activated as a prefix expression
  operator (§6). `for` keeps its M1 loop meaning **and** now also introduces the
  `impl Trait for Type` clause (§2.2) — which reading applies is positional (a `for` after
  `impl … `'s type vs. the `for_stmt` keyword), so no new token is needed.
- `derive` is a **contextual keyword**: it is reserved only at the start of a `struct`/
  `enum` declaration (§4); elsewhere `derive` remains a usable identifier. (This mirrors
  how `self`/`Self` are positional.)
- `import`, `from`, `as`, `to` remain **reserved-but-unused** (modules and word-ranges
  stay deferred — [06-m3-scope.md](06-m3-scope.md)).

### 1.2 Operators and punctuation

No new tokens. M3 reuses existing punctuation in new positions only:

```
[ … ]     # now ALSO encloses type-parameter lists on declarations (§2, §3),
          #   in addition to list literals / indexing (M1) and generic application (M1)
for       # the "impl Trait for Type" connector (§2.2), in addition to the for-loop
```

Generic *application* in a type (`List[T]`, `Result[T, E]`) already lexes and parses via
M1 §6 `base_type = NAME [ "[" type , … "]" ]`; M3 adds generic *declaration* (binding the
parameters) and `Self`. No indentation, comment, literal, string, or continuation rule
changes.

---

## 2. Items — traits, trait impls, generic declarations (amends M1 §4.2–§4.6)

`compound_stmt` gains a trait declaration; `impl_decl` grows a trait clause; `fn`/
`struct`/`enum`/`impl` all gain an optional type-parameter list.

```
compound_stmt += trait_decl
```

### 2.1 Type parameters (shared)

A declaration may bind type parameters in `[ ]`, each with an optional bound. **Parsed,
not checked** (§10).

```
type_params = "[" type_param , … "]"
type_param  = NAME [ ":" bound ]
bound       = NAME { "and" NAME }              # T: Ord  /  T: Ord and Show  (spec §10)
```

The bracketed list binds *new* type names for the declaration; an occurrence of one of
those names later in the same signature is an ordinary `base_type` `NAME` (M1 §6).

### 2.2 Traits and trait impls

```
trait_decl  = "trait" NAME [ type_params ] ":" trait_body
trait_body  = NEWLINE INDENT trait_item { trait_item } DEDENT
trait_item  = method_sig                       # required method (no body)
            | fn_decl                          # default method (full body, M2 §2)
method_sig  = "fn" NAME [ type_params ] "(" [ param_list ] ")" [ "->" type ] NEWLINE

impl_decl   = "impl" [ type_params ] type_path [ "for" type_path ] ":" impl_body
impl_body   = NEWLINE INDENT fn_decl { fn_decl } DEDENT
type_path   = NAME [ "[" type , … "]" ]        # a type, optionally generic-applied
```

- A **`method_sig`** is a `fn` header terminated by `NEWLINE` — *no* `:` suite. That
  trailing `:`-or-`NEWLINE` is exactly what distinguishes a **required** method (signature
  only) from a **default** method (a full `fn_decl`, which has a `:` suite). Trait methods
  take `self` as their first parameter (M2 §2).
- **`impl_decl`** now has two forms, distinguished by the optional `for`:
  - `impl Type:` / `impl Stack[T]:` — an **inherent** impl (M1 §4.6), unchanged except for
    the optional leading `type_params` (`impl[T] Stack[T]:`).
  - `impl Trait for Type:` / `impl[T] Ordered for Stack[T]:` — a **trait** impl. The first
    `type_path` is the trait, the second is the implementing type. Which is which is purely
    positional (the `for` is the pivot); the parser does not need to know names are traits.
- A **trait-typed parameter** needs no new grammar: `fn total_area(shapes: List[Area])` is
  an ordinary annotated `param` whose `type` is the trait name (M1 §6). Whether a passed
  value implements the trait is resolved at **runtime** (§10).

### 2.3 Generic fn / struct / enum (amends M2 §2, M1 §4.4–§4.5)

The decl heads gain an optional `type_params` immediately after the name:

```
fn_decl     = "fn" NAME [ type_params ] "(" [ param_list ] ")" [ "->" type ] ":" suite
struct_decl = [ derive_clause ] "struct" NAME [ type_params ] ":" struct_body
enum_decl   = [ derive_clause ] "enum"   NAME [ type_params ] ":" enum_body
```

(`derive_clause` is §4; `param`, `struct_body`, `enum_body`, and the suite are unchanged
from M2/M1.) The parameter and field/variant-payload **types** may now reference the bound
names and use generic application freely (`List[T]`, `Map[K, V]`, `Result[T, E]`) — all of
which already parse via M1 §6.

---

## 3. Types — `Self` (amends M1 §6)

`base_type` gains the `Self` type. Generic application and the `?` suffix are unchanged.

```
base_type  += "Self"                           # the implementing type, inside a trait/impl
```

- `Self` is only meaningful inside a `trait_decl` or `impl_decl` body, where it denotes the
  implementing type; the resolution is **semantic**, not grammatical. Outside such a body it
  parses but has no binding (a runtime error if used).
- No other type rule changes: `fn_type`, `tuple_type`, generic application, and `?`
  nullability are all as M2 §3 / M1 §6.

---

## 4. `derive` annotations (new; spec §7.1)

A `struct`/`enum` may be preceded by a single `derive` line requesting opt-in traits:

```
derive_clause = "derive" NAME , … NEWLINE      # e.g.  derive Ord
```

- The clause sits **immediately above** the `struct`/`enum` head (see the decl rules in
  §2.3). It binds to that one declaration.
- In M3 the only meaningful name is **`Ord`** (`Eq`/`Hash`/`Show` are automatic and need no
  `derive` — spec §7.1). The grammar accepts a comma-separated list for forward
  compatibility; an unknown derive name is a **semantic** error, not a grammar one.
- M3 settles the spec §7.1 open question in favor of this **annotation** form
  (`derive Ord`) over a `derive(Ord)` call form.

---

## 5. Trait method dispatch (no grammar change)

Calling a trait method (`shape.area()`, `shape.describe()`) needs **no new grammar**: it is
an ordinary `postfix` → `member_suffix` → `call_suffix` chain (M1 §5.5). Whether the call
resolves to an inherent method, a trait-`impl` method, or a trait **default** method — and
whether the receiver's type implements the trait at all — is a **runtime** dispatch
decision, not surface syntax. Listed here only so the absence of a rule is deliberate and
reviewable (cf. M2 §5 for the iterator pipeline).

---

## 6. `try` — early-return operator (amends M1 §5.4)

`try` is a prefix expression operator. It binds **tighter than arithmetic** (so
`try f(a) + try f(b)` is `(try f(a)) + (try f(b))`, and `try f(a) / d` is `(try f(a)) / d`,
matching spec §9), and applies to the expression to its right at the unary level:

```
unary   = "-" unary
        | "try" unary                          # unwrap Ok / early-return Err (M3)
        | power
```

- `try expr` evaluates `expr` (which must produce a `Result`); on `Ok(v)` the value is `v`,
  on `Err(e)` it returns `Err(e)` from the enclosing function. The early-return and the
  "enclosing function must return `Result`" rule are **runtime** semantics, not grammar.
- `try` is an **expression**, so it composes anywhere an expression is allowed:
  `total = total + try parse_int(line)`, `Ok(try eval(a) / divisor)`, `r = try make(x)`.
- `Result`, `Ok`, and `Err` are **not** grammar — like `print`/`panic` (M1 §5.6) they are
  prelude bindings (`Result` an enum, `Ok`/`Err` its variant constructors), so they parse
  through the generic call/construction rules and may be shadowed.
- **Prelude `Ok`/`Err` patterns.** As the one exception to M2's qualified-variant rule
  (M2 grammar §7.4 — user variants must write `.V` or `Enum.V`), the prelude `Result`
  variants may be matched **unqualified**: `Ok(v):` / `Err(e):`, mirroring their bare
  constructors. This is a targeted prelude accommodation (`Ok`/`Err` only), not a general
  return to unqualified variant patterns; user enum variants stay qualified. The
  leading-dot forms `.Ok(v)` / `.Err(e)` also work, as for any enum.

---

## 7. Worked example — the M3 showcase parses

Cross-checking the showcase from [06-m3-scope.md](06-m3-scope.md):

```adder
trait Area:                                     # trait_decl (§2.2)
    fn area(self) -> Float                       #   method_sig — required, no suite
    fn describe(self) -> String:                 #   fn_decl — default method, has a suite
        return "area = {self.area()}"

impl Area for Circle:                            # trait impl (§2.2): Trait `for` Type
    fn area(self) -> Float:
        return 3.14159 * self.radius * self.radius

impl Area for Rect:
    fn area(self) -> Float:
        return self.w * self.h
    fn describe(self) -> String:                 # overrides the default (semantic)
        return "rect {self.w} x {self.h} = {self.area()}"

fn checked_rect(w: Float, h: Float) -> Result[Rect, ShapeError]:   # Result[...] = generic app (M1 §6)
    if w < 0.0 or h < 0.0:
        return Err(ShapeError.NegativeSize)      # Err(...) = prelude construction
    return Ok(Rect(w: w, h: h))

fn scaled_area(w: Float, h: Float, k: Float) -> Result[Float, ShapeError]:
    r = try checked_rect(w, h)                    # try at the unary level (§6)
    return Ok(r.area() * k)

fn total_area(shapes: List[Area]) -> Float:       # trait-typed param — ordinary annotation
    return shapes.map(s -> s.area()).sum()        # dispatch is runtime (§5)

derive Ord                                         # derive_clause (§4)
struct Score:
    points: Int
    name: String

fn main():
    match scaled_area(-1.0, 3.0, 2.0):            # match over a Result — plain enum match
        Ok(a):  print("scaled = {a}")
        Err(e): print("rejected: {e}")
    board.sort()                                   # legal because of derive Ord (semantic)
```

Every M3 construct is covered: a `trait_decl` with a required `method_sig` and a default
`fn_decl` (§2.2), two `impl Trait for Type` blocks (§2.2), a generic-applied `Result[T, E]`
result type (M1 §6), prelude `Ok`/`Err` construction, a `try` prefix at the unary level
(§6), a trait-typed parameter (§2.2/§5), a `derive Ord` annotation (§4), and a `match` over
a `Result` (M1 §5.7, exhaustiveness unchanged).

---

## 8. Deferred to M4 (not in this grammar)

Tracked so the cuts past M3 are explicit (per [06-m3-scope.md](06-m3-scope.md)):

- **Full type checker + inference**, **generic bound *checking***, and
  **trait-conformance as a static check** — M3 keeps only the two M1/M2 checks; bounds and
  missing-impl methods are runtime errors.
- **Lazy iterator pipelines** — eager since M2; a runtime change, not a grammar one.
- **Modules / imports** (`import`, `from … import`, `as`) — `import`/`from`/`as` stay
  reserved-but-unused (spec §12).
- **Associated types/constants** on traits (`type Item`), and **multiple-`for`** or
  conditional impls — M3 has single required/default methods only.
- **`Char`**, **word-based ranges** (`0 to 10`; `to` stays reserved), the **REPL**, and
  **`private`** visibility — all later.
