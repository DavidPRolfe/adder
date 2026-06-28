# Adder — M2 Surface Grammar (Milestone 2)

A concrete grammar for the **Milestone 2** surface defined in
[04-m2-scope.md](04-m2-scope.md). It is written as a set of **deltas to the M1
grammar** ([03-mvp-grammar.md](03-mvp-grammar.md)) — only the rules that change or
are new appear here; everything M1 already defined (indentation/`NEWLINE`/`INDENT`/
`DEDENT`, blocks, `if`/`while`/`for`, structs, enums, inherent `impl`, ranges,
arithmetic, the comparison ladder, strings/interpolation) stands unchanged unless a
section below amends it. Read this file beside §0–§8 of the M1 grammar, not instead of
them.

As in M1, the grammar is split into a **lexical** layer and a **syntactic** layer, and
the two static checks (match exhaustiveness, null-narrowing) are **semantic** — they
run over the tree this grammar produces and are not written here. M2 keeps the
typed-lite posture: the new function/tuple **types** are *parsed*, and the new
runtime features are *interpreted*, but neither becomes a full type check (see
[04-m2-scope.md](04-m2-scope.md)).

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

The `returns` keyword is **dropped** — M2 has no `returns` token. The reserved set is
otherwise as in M1 §1.3:

```
fn  val  struct  enum  impl  return
if  elif  else  match  while  for  in
break  continue  and  or  not  is
true  false  null  self
```

`or` keeps its M1 meaning as the boolean operator (§5) and additionally introduces
**or-patterns** (§7) — which reading applies is positional (an `or` between patterns in
a `match` arm head vs. an `or` between expressions), so no new token is needed. The
reserved-but-unused words (`trait import from as to try Self`) remain reserved for M3.

### 1.2 Operators and punctuation

Two punctuators take on new roles; one new token sequence is recognized:

```
->        # the function/mapping arrow — now ALSO spells the fn result type and
          #   function-type results, not just the lambda body (§2, §3)
?.        # safe-call: a `?` immediately followed by `.` (§6).
.         # member access (unchanged); `?.` is lexed as the pair `?` `.`
```

- `->` is unchanged as a token; M2 simply uses it in three places — a lambda body
  (`x -> e`, M1), a **function result type** (`fn f(...) -> R:`, replacing M1's
  `returns`), and a **function type** (`(T1, …) -> R`, §3). It reads uniformly as
  "maps to."
- `?.` is the safe-call operator (§6). It is the existing `?` token directly followed by
  `.`; the parser recognizes the pair, exactly as M1 recognizes `is` `not` as a pair.
  A `?` with no following `.` is still the M1 nullable-type suffix (§3).
- `{` `}` gain literal meaning for `Map`/`Set`/comprehensions (§4); they remain the
  same tokens (block delimiters never use braces — Adder blocks are indentation-based).

No other lexical rules change: indentation, comments, `##` doc comments, literals,
strings/interpolation, and bracket-continuation are exactly as M1 §1.

---

## 2. Functions — `->` replaces `returns` (amends M1 §4.3)

The result type is introduced by `->`, not the (now-deleted) `returns` keyword. A
parameter may carry a **default value** (§8).

```
fn_decl    = "fn" NAME "(" [ param_list ] ")" [ "->" type ] ":" suite
param_list = param , …
param      = "self"                          # method receiver: first param only, untyped
           | NAME ":" type [ "=" expr ]      # annotated positional, optional default (§8)
```

- The result type is `-> type`; it is **omitted for a unit-returning function** (the
  arrow is what carries the type, so no arrow means `()`).
- Everything else from M1 §4.3 holds: signatures are fully annotated, `self` is only the
  first parameter of a method in an `impl` body, and the body is a value-producing
  suite.

---

## 3. Types — function and tuple types (amends M1 §6)

`base_type` gains a **function type** and a **tuple type**. The `?` nullable suffix and
generic application are unchanged.

```
type       = base_type [ "?" ]                 # trailing ? = nullable (M1 §6)
base_type  = NAME [ "[" type , … "]" ]         # name, optionally generic-applied (M1)
           | "(" ")"                           # unit type (M1)
           | fn_type                           # function type (M2)
           | tuple_type                        # tuple type   (M2)

fn_type    = "(" [ type , … ] ")" "->" type    # (T1, …, Tn) -> R ; zero params: () -> R
tuple_type = "(" type "," [ type , … ] ")"     # (A, B, …) — needs at least one comma
```

- **Function type** `(T1, …) -> R`: the parenthesized parameter types, then `->`, then a
  single result type. `() -> R` is the zero-argument form. The result is mandatory
  (every function type has a `-> R`); a unit result is written `() -> ()`. Function types
  appear anywhere a `type` appears — most usefully a parameter type
  (`fn apply(f: (Int) -> Int, x: Int) -> Int:`). They are **parsed, not checked** in M2.
- **Tuple type** `(A, B, …)` requires at least one comma, mirroring the literal rule
  (§4): `(T)` is a parenthesized type (grouping), **not** a 1-tuple; `()` stays the unit
  type. A trailing comma is not required.
- Disambiguation of the three `(`-led type forms is by lookahead after the closing `)`:
  a following `->` makes it a `fn_type`; a `,` inside makes it a `tuple_type`; `( )` is
  unit; `( type )` is grouping.

---

## 4. Collection literals & comprehensions (new; spec §3, §11)

### 4.1 New primaries

`primary` (M1 §5.5) gains map, set, tuple, and comprehension literals:

```
primary    += map_literal
            | set_literal
            | tuple_literal
            | comprehension
```

```
map_literal   = "{" [ map_entry , … ] "}"     # "{}" is an empty MAP
map_entry     = expr ":" expr                  # key : value
set_literal   = "{" expr , … "}"               # one-or-more bare elements
tuple_literal = "(" expr "," [ expr , … ] ")"  # needs at least one comma
```

- **`{}` / Map / Set disambiguation.** A brace group whose entries are `key : value`
  pairs is a `map_literal`; a brace group whose entries are bare expressions is a
  `set_literal`. The **empty** `{}` is an **empty Map** (matching Python). There is no
  empty-set literal — an empty set is constructed with the ordinary call `Set()` (no
  special grammar; it parses as a `postfix` call, M1 §5.5). A non-empty set needs at
  least one element, so `{}` is never a set.
- **Tuple vs. grouping.** A `tuple_literal` needs **at least one comma**: `(a, b)` and
  `(a,)` (one-tuple, trailing comma) are tuples; `(expr)` is pure grouping (M1); `()` is
  the unit value. This mirrors the tuple *type* rule (§3).

### 4.2 Comprehensions

A comprehension is sugar over a single `for`; the binder is scoped to the comprehension.
The three collection kinds reuse the literal delimiters:

```
comprehension   = list_comp | set_comp | map_comp
list_comp       = "[" expr            comp_clause "]"   # [out for x in it (if c)?]
set_comp        = "{" expr            comp_clause "}"   # {out for x in it (if c)?}
map_comp        = "{" expr ":" expr   comp_clause "}"   # {k: v for x in it (if c)?}

comp_clause     = "for" comp_binder "in" expr [ "if" expr ]
comp_binder     = NAME
                | "(" NAME , … ")"                      # tuple destructuring binder
```

- The leading expression(s) are the per-element output; the kind follows the delimiter
  and entry shape exactly as for literals (§4.1): `[ … ]` → list, `{ x … }` → set,
  `{ k : v … }` → map.
- `comp_binder` is a single `NAME` or a parenthesized tuple of names
  (`for (k, v) in m.items()`); it is the comprehension-local subset of the destructuring
  binders in §8.
- The optional trailing `if expr` filter keeps only elements for which the condition is
  `Bool`-true (no truthiness — a runtime rule, as in M1).
- A `{ … for … }` body is what distinguishes a comprehension from a plain set/map
  literal: the `for` keyword after the first entry switches the parse from `4.1` to
  `4.2`.

> M2 comprehensions support exactly one `for` clause and one optional `if` (matching
> [04-m2-scope.md](04-m2-scope.md)); multiple `for`/`if` clauses are not in M2.

---

## 5. Iterator-pipeline methods (no grammar change)

The eager iterator pipeline (`xs.filter(p).map(f).sum()`, etc.) needs **no new
grammar**: `map`/`filter`/`fold`/`sum`/… are ordinary method calls that already parse
through M1's `postfix` → `member_suffix` → `call_suffix` (M1 §5.5), with lambda
arguments parsing through the existing `lambda` rule (M1 §5). The pipeline is a
**runtime** method table on `List`/`Map`/`Set`/range, not surface syntax. Listed here
only so the absence of a grammar rule is deliberate and reviewable.

---

## 6. Safe-call `?.` and `.expect` (amends M1 §5.5)

```
member_suffix = "." NAME                       # plain member access (M1)
              | "?." NAME                       # safe-call: null if the receiver is null (M2)
```

- `x?.field` and `x?.method(args)` yield `null` when `x` is `null`, instead of erroring;
  otherwise they behave like `.`. `?.` composes in a `postfix` chain like `.`
  (`a?.b?.c`).
- **`.expect(msg)` is not new grammar.** It is an ordinary method call
  (`member_suffix` + `call_suffix`) — `x.expect("…")` asserts non-null and `panic`s with
  the message otherwise. Like `.or_else(...)` in M1, it needs no special rule.
- Both `?.` and `.expect(...)` are recognized by the **null-narrowing** check as valid
  ways to discharge a `T?` (a semantic extension owned by `checks.rs`, not grammar).

---

## 7. Match guards, or-patterns, nested & tuple patterns (amends M1 §5.7–§5.8)

### 7.1 Guards

A `match` arm may carry an `if`-guard between its pattern and the `:`:

```
match_arm  = pattern [ "if" expr ] ":" arm_body      # the `if expr` is the guard
```

The guard is an ordinary `Bool` expression evaluated after the pattern matches; the arm
is taken only if the guard holds. A **guarded arm does not count toward exhaustiveness**
(a semantic rule extended in `checks.rs`).

### 7.2 Patterns become richer and recursive

M1 patterns were **flat** (M1 §5.8). M2 makes them recursive and adds or-patterns and
tuple patterns:

```
pattern        = or_pattern

or_pattern     = primary_pattern { "or" primary_pattern }    # p1 or p2 or … (§7.3)

primary_pattern = "_"                       # wildcard
                | NULL                       # the null pattern
                | literal_pattern            # INT | FLOAT | BOOL | STRING
                | NAME                       # binds the whole scrutinee
                | variant_pattern            # qualified, possibly nested (§7.4)
                | tuple_pattern              # (p, p, …)            (§7.5)

variant_pattern = [ NAME ] "." NAME [ "(" [ pattern , … ] ")" ]   # subs are full patterns
tuple_pattern   = "(" pattern "," [ pattern , … ] ")"             # at least one comma

literal_pattern = INT | FLOAT | BOOL | STRING
```

### 7.3 Or-patterns

`1 or 2 or 3:` and `.A or .B:` match if **any** alternative matches. The alternatives
must bind the same set of names (a semantic rule, not grammatical). For exhaustiveness,
an or-pattern **counts each of its variant alternatives** (extended in `checks.rs`).

### 7.4 Nested variant patterns

A variant pattern's sub-patterns are now full `pattern`s (M1 restricted them to simple
bindings/`_`/`null`/literals). So a sub-pattern may itself be a variant, tuple, literal,
or or-pattern: `.Some(.Pair(a, b))`, `.Node(.Leaf, .Leaf)`. Variants remain
**qualified** exactly as M1: the leading-dot form `.Variant` (enum inferred from the
scrutinee) or the explicit `Enum.Variant`, with the parentheses dropped for a niladic
variant. A bare `NAME` is still always a binding.

### 7.5 Tuple patterns

`(a, b)` destructures a tuple value element-wise; like the tuple literal/type it needs at
least one comma (`(p)` is a parenthesized sub-pattern, grouping). Tuple patterns nest and
pair naturally with `map.items()` (`.Pair((k, v))`, `for (k, v) in …`).

---

## 8. Default & named function arguments; destructuring binders (amends M1 §4.1, §4.3, §5.5)

### 8.1 Default parameter values

A parameter may give a default (see the `param` rule in §2):

```
param = "self"
      | NAME ":" type [ "=" expr ]            # `= expr` is the default value
```

Calling may omit a defaulted trailing parameter. (Whether defaults must be trailing, and
default-expression evaluation timing, are semantic rules owned by the runtime.)

### 8.2 Named call arguments

The M1 `arg` form already permits `NAME ":" expr`, but M1 only made it *valid* for
struct/enum construction. M2 makes a named argument valid for **function calls** too — a
semantic change, **no grammar change** (the surface `arg` rule from M1 §5.5 is reused):

```
arg = expr                  # positional
    | NAME ":" expr         # named — now valid for function calls AND construction (M2)
```

### 8.3 Destructuring binders in `val` and `for`

`val` bindings and `for` loop heads accept a tuple destructuring binder, not just a
single `NAME` (M1 allowed one `NAME` each):

```
binding   = "val" bind_target [ ":" type ] "=" expr     # immutable, single-assignment
          | NAME ":" type "=" expr                       # typed mutable (unchanged)
          | NAME "=" expr                                # inferred mutable (unchanged)
bind_target = NAME
            | "(" NAME , … ")"                           # tuple destructuring

for_stmt  = "for" for_binder "in" expr ":" suite
for_binder = NAME
           | "(" NAME , … ")"                            # tuple destructuring
```

- `val (a, b) = pair` binds `a` and `b` from a tuple; `for (k, v) in map.items():` binds
  each pair's components. Both are flat tuples of names in M2 (the binder is a name
  tuple, not an arbitrary pattern).
- Mutable assignment targets (M1's `target`/`assignment`) are unchanged — destructuring
  binders are an introduction form (`val` / `for`), not a reassignment l-value.

---

## 9. Worked example — the M2 showcase parses

Cross-checking the showcase from [04-m2-scope.md](04-m2-scope.md):

```adder
fn pipeline(xs: List[Int], keep: (Int) -> Bool, f: (Int) -> Int) -> Int:  # fn_type params, -> result
    return xs.filter(keep).map(f).sum()       # postfix method chain (§5), lambdas as args

fn main():
    nums = [1, 2, 3, 4, 5, 6]

    result = pipeline(nums, n -> n % 2 == 0, n -> n * n)   # lambdas passed to fn-typed params
    print("sum of even squares = {result}")

    squares = [x * x for x in 1..=5 if x != 3]             # list_comp with an if filter (§4.2)
    print(squares)

    prices = {"apple": 3, "pear": 2, "fig": 5}             # map_literal (§4.1)
    for (name, cost) in prices.items():                    # for_binder tuple destructuring (§8.3)
        label = match cost:                                # match expression
            c if c >= 5: "pricey"                          # guarded arm (§7.1)
            _:           "cheap"
        print("{name}: {label}")

    print("total = {prices.values().sum()}")               # postfix method chain (§5)
```

Every M2 construct above is covered: a function type `(Int) -> Bool` and the `->` result
type (§2–§3), lambdas as arguments through the M1 `lambda` rule (§5), an iterator
pipeline as plain method calls (§5), a list comprehension with a filter (§4.2), a map
literal (§4.1), a tuple destructuring binder in `for` (§8.3), and a guarded `match` arm
(§7.1).

---

## 10. Deferred to M3 (not in this grammar)

Tracked so the cuts past M2 are explicit (per [04-m2-scope.md](04-m2-scope.md)):

- **Lazy iterator pipelines** — M2's pipeline methods are *eager* (each transform returns
  a fresh `List`); the surface is identical, so laziness is a runtime change, not a
  grammar one.
- **Traits / `impl Trait for Type` / `Self` / default methods / opt-in `derive Ord`.**
- **`Result` + `try`** — `try expr` early-return desugaring; M2 still uses `panic`.
- **Modules / imports** (`import`, `from … import`, `as`).
- **Full type checker + inference**, and **user-declared generics** (`fn f[T]`,
  `struct S[T]`) with trait bounds — M2 keeps only the two M1 checks, extended for
  guards/or-patterns (exhaustiveness) and `?.`/`.expect` (null-narrowing).
- **Multi-clause comprehensions** (more than one `for`/`if`), match guards combined with
  arbitrary nesting beyond the forms above, and `Char` — all later.
