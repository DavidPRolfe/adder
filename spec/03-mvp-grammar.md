# Adder — MVP Formal Grammar (Milestone 1)

A concrete grammar for the **smallest runnable Adder** defined in
[02-mvp-scope.md](02-mvp-scope.md). It covers exactly the M1 surface syntax — nothing
from the deferred list (no traits, generics, `Result`/`try`, comprehensions, lazy
pipelines, modules, tuples, default/named *function* arguments, `?.`, match guards,
or-patterns, nested destructuring). Where the full language reference
([01-language-reference.md](01-language-reference.md)) is richer, this file is the
authority for M1.

The grammar is split into a **lexical** layer (source text → token stream, including
the indentation tokens) and a **syntactic** layer (token stream → parse tree). The two
static checks of the MVP — match exhaustiveness and null-narrowing — are **semantic**,
not grammatical, and are intentionally absent here; they run over the tree this grammar
produces.

---

## 0. Notation

A pragmatic EBNF:

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

`NEWLINE`, `INDENT`, and `DEDENT` are synthetic tokens produced by the lexer (§1.2);
they are written in the syntactic grammar like any other terminal.

---

## 1. Lexical grammar

### 1.1 Source, whitespace, comments

- Source is **UTF-8**. Indentation is **4 spaces** per level; a tab in leading
  whitespace is a lexical error.
- A `#` begins a comment running to end of line. A `##` (doc comment) immediately above
  a declaration is captured and attached to it for tooling; lexically it is otherwise a
  comment. Comments never produce tokens.
- Blank lines and comment-only lines produce **no** `NEWLINE` and do not affect
  indentation.

### 1.2 Line structure: NEWLINE / INDENT / DEDENT

The lexer turns layout into explicit tokens (the classic offside-rule algorithm):

- A logical line ends with `NEWLINE`. A logical line may span several physical lines
  while any `(`, `[`, or `{` is open — newlines inside brackets are suppressed
  (bracket-continuation; no backslash continuation exists).
- When a logical line's indentation is **deeper** than the enclosing block, emit one
  `INDENT`. When it is **shallower**, emit one `DEDENT` per level closed; the
  indentation must return exactly to a previous level (otherwise: indentation error).
- At end of file, emit a final `NEWLINE` (if needed) and a `DEDENT` for every open
  block.

Indentation is only opened after a `:`-introduced block (§3). An `INDENT` appearing
anywhere else is a syntax error.

### 1.3 Identifiers and keywords

```
NAME        = id_start { id_continue }
id_start    = unicode_letter | "_"
id_continue = unicode_letter | unicode_digit | "_"
```

Reserved keywords (a `NAME`-shaped token matching one of these lexes as that keyword,
not as `NAME`). M1 uses this subset of the language's reserved set:

```
fn  val  struct  enum  impl  return
if  elif  else  match  while  for  in
break  continue  and  or  not  is
true  false  null  self
```

Notes:
- `print` and `panic` are **not** keywords — they are ordinary identifiers bound to
  built-in functions in the prelude (§5.6), so they parse as plain function calls and
  may be shadowed like any name. `self` is the method receiver.
- Reserved-but-unused-in-M1 words (`trait import from as to try Self`) remain reserved
  so future milestones add them without breaking programs.
- `mut` is **not** a keyword (mutable-by-default); `val` opts into immutability.

### 1.4 Literals

```
INT     = digit { digit | "_" }                         # arbitrary precision
FLOAT   = digit { digit | "_" } "." digit { digit | "_" }
BOOL    = "true" | "false"
NULL    = "null"
```

- `INT` and `FLOAT` allow `_` as a digit separator (`1_000_000`). A leading or trailing
  `_`, or `_` adjacent to the `.`, is invalid. M1 has no `0x`/`0o`/`0b`/exponent forms.
- A `.` is only a `FLOAT` decimal point when digits follow it; `x.field` and `0..n`
  therefore lex unambiguously (`0..n` is `INT INT`-range, never `FLOAT`).

### 1.5 String literals and interpolation

A string is a `"`-delimited sequence of text, escapes, and interpolations. It lexes to
a single `STRING` token whose value is a list of *parts*; each interpolation part holds
a nested token stream re-lexed from between its braces.

```
STRING        = '"' { str_part } '"'
str_part      = str_text | str_escape | interpolation
str_text      = any char except '"', '\', '{', '}'
str_escape    = "\" ( '"' | "\" | "n" | "t" | "r" | "0" | "{" | "}" )
              | "{{"  | "}}"            # literal brace, per spec
interpolation = "{" expr "}"           # expr is parsed by the syntactic grammar
```

- `{{` and `}}` are the spec's brace escapes (yield literal `{` / `}`); `\{` `\}` are
  accepted equivalently.
- An interpolation holds a full `expr` (§5). It does not span the closing `"`; an
  unbalanced `{` is a lexical error.
- M1 strings are single-line (no raw or triple-quoted forms).

### 1.6 Operators and punctuation

```
==  !=  <  <=  >  >=        # comparison
+   -   *   /   %   **      # arithmetic
=                          # assignment / binding
->                         # function/mapping arrow (lambda body, fn result type)
..  ..=                    # ranges
:   ,   .                  # block colon, separators, member access
(  )   [  ]   {  }         # grouping / call / index / collection
?                          # nullable type suffix (types only, §6)
```

`is not` is two tokens (`is` `not`); the parser recognizes the pair (§5.3).

---

## 2. Program structure

```
program   = { NEWLINE } { statement }
```

A program is a sequence of top-level statements. Per the entry-point rule, the runtime
executes top-level statements in order, then calls `main()` if a zero-arg `main` was
declared. (That call is runtime behavior, not grammar.)

```
statement = simple_stmt NEWLINE
          | compound_stmt
```

`compound_stmt` carries its own block terminator (a `DEDENT` or an inline `NEWLINE`),
so it is not followed by an extra `NEWLINE` here.

---

## 3. Blocks (the `:` suite)

Every block is introduced by a trailing `:`. It is either a single inline statement or
an indented suite:

```
suite     = simple_stmt NEWLINE
          | NEWLINE INDENT statement { statement } DEDENT
```

- Inline form (`if ready: go()`) allows exactly one **simple** statement.
- The indented form requires at least one statement.

Several constructs use a *value-producing* block — a `match` arm or a function body
whose value is its final expression. Grammatically these are an ordinary `suite`; "the
last statement is an expression and is the block's value" is a semantic rule, not a
syntactic one.

---

## 4. Statements

### 4.1 Simple statements

```
simple_stmt = binding
            | assignment
            | return_stmt
            | "break"
            | "continue"
            | expr                       # expression statement (e.g. a call, print)
```

```
binding     = "val" NAME [ ":" type ] "=" expr      # immutable, single-assignment
            | NAME ":" type "=" expr                # typed mutable binding
            | NAME "=" expr                         # inferred mutable binding

assignment  = target "=" expr
target      = ( NAME | "self" ) { "." NAME | "[" expr "]" }   # name/self, field, or index l-value

return_stmt = "return" [ expr ]
```

Disambiguation: a line beginning with `NAME` is a **binding/assignment** iff a top-level
`=` (or `: type =`) follows the l-value; otherwise it is an `expr` statement. The first
assignment to a plain `NAME` introduces it; a later `NAME = …` reassigns it (and
reassigning a `val` is the semantic error from the MVP definition-of-done). `count = 0`
introduces, `count = count + 1` reassigns, `p.x = 5.0` / `xs[0] = v` mutate through a
`target`. A line beginning with `self` is likewise an assignment when a top-level `=`
follows a non-empty `self.field` / `self[i]` path (a bare `self` is not assignable) —
this is how a method mutates its receiver.

### 4.2 Compound statements

```
compound_stmt = if_stmt | while_stmt | for_stmt
              | fn_decl | struct_decl | enum_decl | impl_decl
```

```
if_stmt    = "if" expr ":" suite
             { "elif" expr ":" suite }
             [ "else" ":" suite ]

while_stmt = "while" expr ":" suite

for_stmt   = "for" NAME "in" expr ":" suite
```

The `expr` in `if`/`elif`/`while` must evaluate to `Bool` (no truthiness) — enforced at
runtime per MVP scope, not by this grammar. In `for … in expr`, M1 expects `expr` to be
a range (`0..n`, `0..=n`) or a `List`; only a single loop variable `NAME` is supported
(no destructuring binder in M1).

### 4.3 Functions

```
fn_decl  = "fn" NAME "(" [ param_list ] ")" [ "->" type ] ":" suite
param_list = param , …
param    = "self"                       # method receiver: first param only, untyped
         | NAME ":" type                # fully annotated, positional
```

- Signatures are fully annotated: every non-`self` parameter has a type; a result type
  is given by `-> type`, and omitted for a unit-returning function. M1 has **no**
  default or named function parameters.
- `self` is only valid as the first parameter of a method declared inside an `impl`
  body (§4.6).
- The body is a value-producing suite: an explicit `return`, or the final expression as
  an implicit return.

### 4.4 Structs

```
struct_decl   = "struct" NAME ":" struct_body
struct_body   = NEWLINE INDENT field_decl { field_decl } DEDENT
field_decl    = NAME ":" type NEWLINE
```

Fields are mutable by default (no per-field `val` in M1). A struct body holds **only**
fields; methods are defined in a separate `impl` block (§4.6), so there is exactly one
way to add a method. (A `fn` in a struct body is a parse error.)

### 4.5 Enums

```
enum_decl     = "enum" NAME ":" enum_body
enum_body     = NEWLINE INDENT variant_decl { variant_decl } DEDENT
variant_decl  = NAME [ "(" payload ")" ] NEWLINE
payload       = payload_field , …
payload_field = type                    # positional, e.g. Add(Expr, Expr)
              | NAME ":" type           # named,      e.g. Circle(radius: Float)
```

A variant may carry positional or named data, or none (`Empty`). M1 does not allow
mixing methods into an `enum` body; enum methods, if any, go in an `impl` block.

### 4.6 Inherent impl blocks

```
impl_decl = "impl" NAME ":" impl_body
impl_body = NEWLINE INDENT fn_decl { fn_decl } DEDENT
```

M1 supports only inherent `impl Type:` blocks (methods for a struct or enum). Trait
impls (`impl Trait for Type:`) are deferred to M2.

---

## 5. Expressions

Listed lowest-precedence first; each rule descends to the next-tighter level. Binary
operators are left-associative unless noted.

```
expr       = lambda
           | ternary

lambda     = lambda_params "->" expr
lambda_params = NAME
              | "(" [ NAME , … ] ")"

ternary    = or_expr [ "if" or_expr "else" expr ]      # value-if-cond-else-value
```

> **Lambdas are deferred to M2** (see [02-mvp-scope.md](02-mvp-scope.md)). The M1
> implementation already parses and evaluates them as a head start, but they are not
> part of the validated M1 surface: there is no function *type* (§6) to annotate a
> parameter with, so a lambda can only be used locally — passing one to a `fn` needs
> the function type that arrives with iterator pipelines in M2.

```
or_expr    = and_expr   { "or"  and_expr }
and_expr   = not_expr   { "and" not_expr }
not_expr   = "not" not_expr
           | comparison
```

### 5.3 Comparison (non-associative)

```
comparison = range_expr [ comp_op range_expr ]
comp_op    = "==" | "!=" | "<" | "<=" | ">" | ">="
           | "is" [ "not" ]                  # value (in)equality, never identity
```

Comparisons do **not** chain in M1: `a < b < c` is a syntax error (write `a < b and b <
c`). `is` / `is not` mean value equality, equivalent to `==` / `!=`.

### 5.4 Range, arithmetic

```
range_expr = add_expr [ ( ".." | "..=" ) add_expr ]    # half-open / inclusive
add_expr   = mul_expr  { ( "+" | "-" ) mul_expr }
mul_expr   = unary     { ( "*" | "/" | "%" ) unary }
unary      = "-" unary
           | power
power      = postfix [ "**" unary ]                    # right-associative
```

Precedence summary, tightest last:

```
or  <  and  <  not  <  comparison  <  range  <  (+ -)  <  (* / %)  <  unary -  <  **  <  postfix
```

Note `**` binds tighter than unary minus: `-2 ** 2` parses as `-(2 ** 2)`, and `**` is
right-associative (`2 ** 3 ** 2` = `2 ** (3 ** 2)`).

### 5.5 Postfix and primary

```
postfix    = primary { call_suffix | index_suffix | member_suffix }
call_suffix   = "(" [ arg , … ] ")"
index_suffix  = "[" expr "]"
member_suffix = "." NAME

arg        = expr                       # positional (function call)
           | NAME ":" expr              # named (struct / enum construction)
```

```
primary    = INT | FLOAT | BOOL | NULL | STRING
           | "self"
           | NAME
           | list_literal
           | "(" expr ")"
           | match_expr

list_literal = "[" [ expr , … ] "]"
```

A `call_suffix` covers both function calls (`eval(a)`, `print(x)`) and construction.
**Struct** construction is a bare call (`Point(x: 1.0, y: 2.0)`). **Enum-variant**
construction is *qualified* — a `member_suffix` then a `call_suffix`
(`Expr.Num(1.0)`, `Expr.Add(a, b)`), or just the `member_suffix` for a niladic variant
(`Color.Red`). `arg` permits positional or `name:`-prefixed values. Which a call *is*
(function vs. construction, named vs. positional validity) is resolved semantically.
`.or_else(default)` is likewise a `member_suffix` + `call_suffix`, requiring no special
grammar.

### 5.6 Prelude built-ins

`print` and `panic` are **not** grammar — they are ordinary identifiers bound to
built-in functions in a prelude scope the interpreter seeds before running the program
(the same global scope that holds top-level `fn`s and bindings). They parse through the
generic `postfix` + `call_suffix` rule, exactly like any user call:

- `print(args, …)` writes the default `Show` rendering of each argument.
- `panic(msg)` aborts with `msg` and a backtrace; it never returns normally.

Because they are plain bindings, more built-ins can be added later with no grammar
change, and a user may shadow them.

### 5.7 Match

```
match_expr = "match" expr ":" NEWLINE INDENT match_arm { match_arm } DEDENT
match_arm  = pattern ":" arm_body
arm_body   = expr NEWLINE                                  # inline arm value
           | NEWLINE INDENT statement { statement } DEDENT # block; value = last expr
```

`match` is an expression, so it appears wherever `primary` is allowed (notably
`return match e: …` and `x = match …`). Exhaustiveness over an enum is the
compile-time check from the MVP scope — semantic, enforced over the arm set, not by
this grammar. M1 omits the inline `match x: a: …` single-line form; arms are always an
indented suite.

### 5.8 Patterns (M1 subset)

```
pattern        = "_"                    # wildcard
               | NULL                   # the null pattern
               | literal_pattern        # INT | FLOAT | BOOL | STRING
               | NAME                   # binds the whole scrutinee
               | variant_pattern

variant_pattern = [ NAME ] "." NAME [ "(" [ sub_pattern , … ] ")" ]
sub_pattern     = "_" | NAME | NULL | literal_pattern   # "simple bindings" only

literal_pattern = INT | FLOAT | BOOL | STRING
```

Per MVP scope, M1 patterns are **flat**: a wildcard, a literal, `null`, a binding name,
or a single-level variant destructure whose sub-patterns are simple (a binding, `_`,
`null`, or a literal — no nested variant patterns). Match guards (`if …`), or-patterns
(`1 or 2`), and nested destructuring are deferred to M2.

Variant patterns are **qualified**: the leading-dot form `.Variant` (the enum is
inferred from the scrutinee) or the explicit `Enum.Variant`, with an optional
`( sub, … )` payload that is omitted for a niladic variant (`.Empty`, `Color.Red`).
The leading `.` is what marks a variant, so a **bare `NAME` pattern is always a
binding** (it matches anything and names the value) and a bare `NAME(…)` is *not* a
variant pattern. This removes the identifier-vs-constructor ambiguity and lets niladic
variants be matched precisely.

---

## 6. Types

Types appear only at the annotation boundaries: parameter types, the `->` result type,
struct fields, enum payloads, and the occasional local annotation (`xs: List[Int] = []`).

```
type       = base_type [ "?" ]                 # trailing ? = nullable (§8 of ref)
base_type  = NAME [ "[" type , … "]" ]         # name, optionally generic-applied
           | "(" ")"                           # unit type
```

- `?` is the one place a sigil attaches to a type (`String?`, `List[Int]?`). It applies
  once, to the whole `base_type`.
- `NAME [ … ]` covers built-in generic *applications* the M1 runtime understands
  (`List[Int]`, and — if Map/Set land in M1 — `Map[K, V]`, `Set[T]`). **User-defined**
  generics (declaring `[T]` on your own `fn`/`struct`) are deferred to M2; this rule is
  type *use*, not type-parameter *declaration*.
- `()` is the unit type, the result of a function with no `->` clause.

---

## 7. Worked example — the MVP showcase parses

Cross-checking the showcase from [02-mvp-scope.md](02-mvp-scope.md) against this grammar:

```adder
enum Expr:                       # enum_decl → variant_decl × 4
    Num(Float)                   #   payload = positional type
    Add(Expr, Expr)
    Mul(Expr, Expr)
    Div(Expr, Expr)

fn eval(e: Expr) -> Float:       # fn_decl with param + -> result type
    return match e:              # return_stmt of a match_expr (primary)
        .Num(n):    n            # leading-dot variant_pattern → inline arm_body
        .Add(a, b): eval(a) + eval(b)
        .Mul(a, b): eval(a) * eval(b)
        .Div(a, b):              # block arm_body: stmts, last expr is value
            divisor = eval(b)    # inferred binding
            if divisor == 0.0:   # if_stmt, comparison condition
                panic("division by zero")   # call_form
            eval(a) / divisor    # final expression = arm value

fn main():                       # fn_decl, no -> clause (unit)
    program = Expr.Mul(Expr.Add(Expr.Num(1.0), Expr.Num(2.0)), Expr.Num(3.0))  # qualified construction
    print("= {eval(program)}")   # print form + STRING with interpolation
```

Every construct above is covered: indentation suite, `enum`/variant payloads, `fn`
signature with/without an `->` result type, `match` as a returned expression with both inline and
block arms, flat variant patterns, an `if` with a `Bool` comparison, `panic`/`print`
forms, inferred bindings, construction calls, and an interpolated string.

---

## 8. Deliberately excluded from this grammar (deferred per MVP scope)

Tracked here so the cuts are explicit and reviewable:

- **Traits / `impl Trait for Type` / `Self` / `derive`** — only inherent `impl Type:`.
- **User-declared generics** (`fn f[T]`, `struct S[T]`) and trait bounds.
- **`Result` / `try` / error propagation** — M1 uses `panic`.
- **Lambdas / closures** (`x -> expr`, `(a, b) -> expr`) — parsed/evaluated by the M1
  implementation as a head start, but deferred from the validated M1 surface: with no
  function type (§6) they can't be passed to a `fn`, so they pair with iterator
  pipelines in M2.
- **Lazy pipelines** (`map`/`filter`/`fold`/`.sum()` chains) and **comprehensions** —
  these are method-call / sugar syntax left out of M1 (ordinary `.method()` calls still
  parse via `postfix`, but the iterator prelude is M2).
- **Modules / imports** (`import`, `from … import`, `as`).
- **Tuples** — `(a, b)` literals/types and tuple patterns. M1 has the `()` unit type
  only; `( expr )` is strictly grouping.
- **Default / named *function* arguments** — `name:` args parse only for construction.
- **`?.` safe-call and `.expect`** — `?` is types-only in M1.
- **Match guards, or-patterns, nested destructuring** — patterns are flat (§5.8).
- **`Char`, `Map`, `Set` literals** — `Map`/`Set` may appear as types/`Set()` if they
  land as the M1 "nice-to-have"; their literal `{ … }` syntax is otherwise M2.
- **`val`/destructuring binders in `for` / `val (a, b) = …`** — single `NAME` binders.

---

## 9. Open questions for review

**Resolved**
- **`print` / `panic`** → ordinary prelude built-in functions, not grammar forms
  (§1.3, §5.6). No special syntax; resolved via a seeded global scope, so built-ins
  extend without grammar changes.
- **`for` iterables** → M1 stays restricted to ranges and lists with a single `NAME`
  binder; `for (a, b) in …` (tuples / destructuring) is deferred to M2.
- **Inline `match`** → arms are always an indented suite; the one-line whole-`match`
  form (`match x: 0: "zero"`) is not in M1 (§5.7).

**Still open**
1. **Non-chaining comparisons (§5.3).** Forbidding `a < b < c` is simple and avoids
   Python's special chaining semantics. Confirm we're happy requiring `a < b and b < c`.
2. **`**` vs unary-minus precedence.** Grammar binds `-2 ** 2` as `-(2 ** 2)` (Python
   semantics). Confirm that's the intended reading.
3. **Implicit final-expression value as a syntactic vs. semantic rule.** Kept semantic
   here (any block can end in an expr; only fn bodies / match arms *use* it as a value).
   Fine, or should the grammar distinguish value-blocks from statement-blocks?
4. **Statement terminator after compound forms.** This grammar lets `compound_stmt`
   own its terminator (no trailing `NEWLINE` in `statement`). Mostly an implementation
   note for the lexer/parser; flagging in case we prefer a uniform `NEWLINE`.
