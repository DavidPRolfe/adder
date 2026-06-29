use super::*;

impl<'a> Parser<'a> {
    // =====================================================================
    // Expressions (§5) — precedence ladder, lowest → highest
    // =====================================================================

    /// `expr = lambda | ternary`.
    pub(crate) fn parse_expr(&mut self) -> PResult<Expr> {
        if let Some(lambda) = self.try_parse_lambda()? {
            return Ok(lambda);
        }
        self.parse_ternary()
    }

    /// Try to parse a lambda, returning `None` if the lookahead doesn't indicate
    /// one (so the caller falls through to a normal expression).
    ///
    /// Two surface forms (§5):
    ///   `NAME -> expr`
    ///   `"(" [NAME, …] ")" -> expr`
    /// The single-name form is unambiguous (`NAME` immediately followed by
    /// `->`). The parenthesized form must be distinguished from a grouped
    /// expression `( expr )`: a parameter list contains only `NAME`s, commas and
    /// the closing `)`, and is followed by `->`. We scan the balanced paren group
    /// and require `->` after it *and* a name-only interior.
    pub(crate) fn try_parse_lambda(&mut self) -> PResult<Option<Expr>> {
        match self.peek() {
            TokenKind::Name(_) if matches!(self.peek_n(1), TokenKind::Arrow) => {
                let start = self.cur_span();
                let (name, _) = self.expect_name("a lambda parameter")?;
                self.expect(&TokenKind::Arrow, "`->` in a lambda")?;
                let body = self.parse_expr()?;
                let span = start.merge(body.span);
                Ok(Some(Expr {
                    kind: ExprKind::Lambda(Lambda { params: vec![name], body: Box::new(body) }),
                    span,
                }))
            }
            TokenKind::LParen if self.paren_group_is_lambda_params() => {
                let start = self.cur_span();
                self.advance(); // `(`
                let params = self.parse_separated(&TokenKind::RParen, |p| {
                    let (name, _) = p.expect_name("a lambda parameter")?;
                    Ok(name)
                })?;
                self.expect(&TokenKind::RParen, "`)` to close lambda parameters")?;
                self.expect(&TokenKind::Arrow, "`->` in a lambda")?;
                let body = self.parse_expr()?;
                let span = start.merge(body.span);
                Ok(Some(Expr {
                    kind: ExprKind::Lambda(Lambda { params, body: Box::new(body) }),
                    span,
                }))
            }
            _ => Ok(None),
        }
    }

    /// Lookahead for the `( [NAME, …] ) ->` lambda head. The current token is
    /// `LParen`. True iff the parenthesized run holds only names separated by
    /// commas (possibly empty) and is immediately followed by `->`.
    pub(crate) fn paren_group_is_lambda_params(&self) -> bool {
        // Walk from just after `(` collecting NAME (, NAME)* until `)`.
        let mut i = 1;
        if matches!(self.peek_n(i), TokenKind::RParen) {
            // `()` — empty params, must be followed by `->`.
            return matches!(self.peek_n(i + 1), TokenKind::Arrow);
        }
        loop {
            if !matches!(self.peek_n(i), TokenKind::Name(_)) {
                return false;
            }
            i += 1;
            match self.peek_n(i) {
                TokenKind::Comma => {
                    i += 1;
                    // Allow a trailing comma: `(a,) ->` is not standard but
                    // tolerate `)` right after a comma.
                    if matches!(self.peek_n(i), TokenKind::RParen) {
                        return matches!(self.peek_n(i + 1), TokenKind::Arrow);
                    }
                }
                TokenKind::RParen => {
                    return matches!(self.peek_n(i + 1), TokenKind::Arrow);
                }
                _ => return false,
            }
        }
    }

    /// `ternary = or_expr [ "if" or_expr "else" expr ]` (value-if-cond-else).
    pub(crate) fn parse_ternary(&mut self) -> PResult<Expr> {
        let then = self.parse_or()?;
        if matches!(self.peek(), TokenKind::If) {
            self.advance(); // `if`
            let cond = self.parse_or()?;
            self.expect(&TokenKind::Else, "`else` in a ternary expression")?;
            let otherwise = self.parse_expr()?;
            let span = then.span.merge(otherwise.span);
            Ok(Expr {
                kind: ExprKind::Ternary {
                    then: Box::new(then),
                    cond: Box::new(cond),
                    otherwise: Box::new(otherwise),
                },
                span,
            })
        } else {
            Ok(then)
        }
    }

    /// `or_expr = and_expr { "or" and_expr }` (left-associative).
    pub(crate) fn parse_or(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), TokenKind::Or) {
            self.advance();
            let rhs = self.parse_and()?;
            lhs = make_binary(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    /// `and_expr = not_expr { "and" not_expr }` (left-associative).
    pub(crate) fn parse_and(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_not()?;
        while matches!(self.peek(), TokenKind::And) {
            self.advance();
            let rhs = self.parse_not()?;
            lhs = make_binary(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    /// `not_expr = "not" not_expr | comparison`.
    pub(crate) fn parse_not(&mut self) -> PResult<Expr> {
        if matches!(self.peek(), TokenKind::Not) {
            let start = self.cur_span();
            self.advance();
            let operand = self.parse_not()?;
            let span = start.merge(operand.span);
            Ok(Expr {
                kind: ExprKind::Unary { op: UnOp::Not, operand: Box::new(operand) },
                span,
            })
        } else {
            self.parse_comparison()
        }
    }

    /// `comparison = range_expr [ comp_op range_expr ]` — **non-associative**.
    /// A second comparison operator (`a < b < c`) is a syntax error (§5.3).
    pub(crate) fn parse_comparison(&mut self) -> PResult<Expr> {
        let lhs = self.parse_range()?;
        if let Some(op) = self.peek_comp_op() {
            self.consume_comp_op(); // consumes the 1 or 2 tokens
            let rhs = self.parse_range()?;
            let expr = make_binary(op, lhs, rhs);
            // Reject a chained comparison explicitly.
            if self.peek_comp_op().is_some() {
                return Err(Diagnostic::parse(
                    "comparison operators do not chain in Adder; \
                     write `a < b and b < c` instead",
                    self.cur_span(),
                ));
            }
            Ok(expr)
        } else {
            Ok(lhs)
        }
    }

    /// Peek a comparison operator (`==`/`!=`/`<`/`<=`/`>`/`>=`/`is`/`is not`)
    /// without consuming. `is not` is the `Is`-then-`Not` token pair.
    pub(crate) fn peek_comp_op(&self) -> Option<BinOp> {
        match self.peek() {
            TokenKind::EqEq => Some(BinOp::Eq),
            TokenKind::NotEq => Some(BinOp::NotEq),
            TokenKind::Lt => Some(BinOp::Lt),
            TokenKind::LtEq => Some(BinOp::LtEq),
            TokenKind::Gt => Some(BinOp::Gt),
            TokenKind::GtEq => Some(BinOp::GtEq),
            TokenKind::Is => {
                if matches!(self.peek_n(1), TokenKind::Not) {
                    Some(BinOp::IsNot)
                } else {
                    Some(BinOp::Is)
                }
            }
            _ => None,
        }
    }

    /// Consume the token(s) of the comparison operator peeked by
    /// [`Self::peek_comp_op`].
    pub(crate) fn consume_comp_op(&mut self) {
        match self.peek() {
            TokenKind::Is => {
                self.advance();
                if matches!(self.peek(), TokenKind::Not) {
                    self.advance();
                }
            }
            _ => {
                self.advance();
            }
        }
    }

    /// `range_expr = add_expr [ (".." | "..=") add_expr ]`.
    pub(crate) fn parse_range(&mut self) -> PResult<Expr> {
        let lhs = self.parse_add()?;
        let op = match self.peek() {
            TokenKind::DotDot => Some(BinOp::Range),
            TokenKind::DotDotEq => Some(BinOp::RangeIncl),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let rhs = self.parse_add()?;
            Ok(make_binary(op, lhs, rhs))
        } else {
            Ok(lhs)
        }
    }

    /// `add_expr = mul_expr { ("+" | "-") mul_expr }` (left-associative).
    pub(crate) fn parse_add(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_mul()?;
            lhs = make_binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    /// `mul_expr = unary { ("*" | "/" | "%") unary }` (left-associative).
    pub(crate) fn parse_mul(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Rem,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_unary()?;
            lhs = make_binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    /// `unary = "-" unary | "try" unary | power`.
    ///
    /// `**` binds tighter than unary minus, so `-2 ** 2` = `-(2 ** 2)`: unary
    /// minus wraps the *power* result, and `parse_power` is what consumes the
    /// `**`. `try` (spec §9) sits at the same prefix level, so it binds
    /// tighter than arithmetic — `try f(a) + try f(b)` is `(try f(a)) + (try
    /// f(b))` and `try f(a) / d` is `(try f(a)) / d`.
    pub(crate) fn parse_unary(&mut self) -> PResult<Expr> {
        if matches!(self.peek(), TokenKind::Minus) {
            let start = self.cur_span();
            self.advance();
            let operand = self.parse_unary()?;
            let span = start.merge(operand.span);
            Ok(Expr {
                kind: ExprKind::Unary { op: UnOp::Neg, operand: Box::new(operand) },
                span,
            })
        } else if matches!(self.peek(), TokenKind::Try) {
            let start = self.cur_span();
            self.advance();
            let operand = self.parse_unary()?;
            let span = start.merge(operand.span);
            Ok(Expr { kind: ExprKind::Try(Box::new(operand)), span })
        } else {
            self.parse_power()
        }
    }

    /// `power = postfix [ "**" unary ]` — **right-associative**.
    ///
    /// The right operand is a `unary` (not another `power`), which gives both
    /// right-associativity (`2 ** 3 ** 2` = `2 ** (3 ** 2)`, since the rhs
    /// `unary` descends back into `power`) and the "`**` tighter than unary
    /// minus" rule (`2 ** -3` is valid; the rhs may start with `-`).
    pub(crate) fn parse_power(&mut self) -> PResult<Expr> {
        let base = self.parse_postfix()?;
        if matches!(self.peek(), TokenKind::StarStar) {
            self.advance();
            let exp = self.parse_unary()?;
            Ok(make_binary(BinOp::Pow, base, exp))
        } else {
            Ok(base)
        }
    }

    /// `postfix = primary { call_suffix | index_suffix | member_suffix }`.
    pub(crate) fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                TokenKind::LParen => {
                    self.advance();
                    let args = self.parse_args()?;
                    let close = self.expect(&TokenKind::RParen, "`)` to close a call")?;
                    let span = expr.span.merge(close.span);
                    expr = Expr {
                        kind: ExprKind::Call { callee: Box::new(expr), args },
                        span,
                    };
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.parse_expr()?;
                    let close = self.expect(&TokenKind::RBracket, "`]` to close an index")?;
                    let span = expr.span.merge(close.span);
                    expr = Expr {
                        kind: ExprKind::Index {
                            base: Box::new(expr),
                            index: Box::new(index),
                        },
                        span,
                    };
                }
                TokenKind::Dot => {
                    self.advance();
                    let (name, nspan) = self.expect_name("a member name after `.`")?;
                    let span = expr.span.merge(nspan);
                    expr = Expr {
                        kind: ExprKind::Member { base: Box::new(expr), name, safe: false },
                        span,
                    };
                }
                // `?.` (safe access): same shape as `.`, but `safe:
                // true`. A safe method call `x?.m(args)` is a `Call` whose callee
                // is this safe `Member` — the `LParen` arm above picks it up on
                // the next loop turn, exactly as for a plain `.m()`.
                TokenKind::QuestionDot => {
                    self.advance();
                    let (name, nspan) = self.expect_name("a member name after `?.`")?;
                    let span = expr.span.merge(nspan);
                    expr = Expr {
                        kind: ExprKind::Member { base: Box::new(expr), name, safe: true },
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// `arg = expr | NAME ":" expr` (positional / named).
    pub(crate) fn parse_args(&mut self) -> PResult<Vec<Arg>> {
        self.parse_separated(&TokenKind::RParen, |p| {
            // Named arg: `NAME ":" expr`. The `:` after a bare NAME disambiguates
            // from a positional NAME expression.
            if matches!(p.peek(), TokenKind::Name(_)) && matches!(p.peek_n(1), TokenKind::Colon) {
                let (name, name_span) = p.expect_name("an argument name")?;
                p.advance(); // `:`
                let value = p.parse_expr()?;
                let span = name_span.merge(value.span);
                Ok(Arg::Named { name, value, span })
            } else {
                let value = p.parse_expr()?;
                Ok(Arg::Positional(value))
            }
        })
    }

    /// `primary = INT | FLOAT | BOOL | NULL | STRING | self | NAME
    ///          | list_literal | brace_literal | tuple_or_group | match_expr`.
    ///
    /// `brace_literal` is a `Map`/`Set` literal or a set/map comprehension;
    /// `tuple_or_group` is `( expr )` grouping or `( a, b, … )` tuple.
    pub(crate) fn parse_primary(&mut self) -> PResult<Expr> {
        let span = self.cur_span();
        match self.peek().clone() {
            TokenKind::Int(n) => {
                self.advance();
                Ok(Expr { kind: ExprKind::Int(n), span })
            }
            TokenKind::Float(f) => {
                self.advance();
                Ok(Expr { kind: ExprKind::Float(f), span })
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr { kind: ExprKind::Bool(true), span })
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr { kind: ExprKind::Bool(false), span })
            }
            TokenKind::Null => {
                self.advance();
                Ok(Expr { kind: ExprKind::Null, span })
            }
            TokenKind::Str(parts) => {
                self.advance();
                let lit = self.build_string_lit(&parts)?;
                Ok(Expr { kind: ExprKind::Str(lit), span })
            }
            TokenKind::SelfKw => {
                self.advance();
                Ok(Expr { kind: ExprKind::SelfExpr, span })
            }
            TokenKind::Name(s) => {
                self.advance();
                Ok(Expr { kind: ExprKind::Name(s), span })
            }
            TokenKind::LBracket => self.parse_list_literal_or_comprehension(),
            TokenKind::LBrace => self.parse_brace_literal(),
            TokenKind::LParen => self.parse_tuple_or_group(),
            TokenKind::Match => self.parse_match_expr(),
            other => Err(Diagnostic::parse(
                format!("expected an expression, found {}", describe(&other)),
                span,
            )),
        }
    }

    /// `list_literal = "[" [ expr , … ] "]"`, or a **list comprehension**
    /// `"[" expr "for" binder "in" iter [ "if" cond ] "]"`. A top-level
    /// `for` inside the brackets selects the comprehension form.
    pub(crate) fn parse_list_literal_or_comprehension(&mut self) -> PResult<Expr> {
        let start = self.cur_span();
        self.expect(&TokenKind::LBracket, "`[` to open a list literal")?;
        if self.brace_has_top_level_for(&TokenKind::RBracket) {
            let output = ComprehensionOutput::List(Box::new(self.parse_expr()?));
            return self.finish_comprehension(output, start, &TokenKind::RBracket, "list");
        }
        let items = self.parse_separated(&TokenKind::RBracket, Self::parse_expr)?;
        let close = self.expect(&TokenKind::RBracket, "`]` to close a list literal")?;
        Ok(Expr { kind: ExprKind::List(items), span: start.merge(close.span) })
    }

    /// A brace-delimited literal: a `Map` `{ k: v, … }`, a `Set`
    /// `{ x, … }`, the empty `Map` `{}`, or a set/map comprehension. The current
    /// token is the opening `{`.
    ///
    /// Disambiguation, by the first token(s) after `{`:
    /// - `}`                     → empty **Map** (`{}` is never a set; use `Set()`).
    /// - top-level `for` present → comprehension (map form if a top-level `:`
    ///   precedes the `for`, else set form).
    /// - first element is `expr ":"` → **Map** literal.
    /// - otherwise               → **Set** literal.
    pub(crate) fn parse_brace_literal(&mut self) -> PResult<Expr> {
        let start = self.cur_span();
        self.expect(&TokenKind::LBrace, "`{` to open a map or set literal")?;

        // Empty `{}` is an empty Map.
        if matches!(self.peek(), TokenKind::RBrace) {
            let close = self.advance();
            return Ok(Expr { kind: ExprKind::Map(Vec::new()), span: start.merge(close.span) });
        }

        // Comprehension? A top-level `for` inside the braces selects it.
        if self.brace_has_top_level_for(&TokenKind::RBrace) {
            // Map comprehension iff a top-level `:` precedes that `for`.
            if self.brace_colon_before_for() {
                let key = self.parse_expr()?;
                self.expect(&TokenKind::Colon, "`:` between a map comprehension's key and value")?;
                let value = self.parse_expr()?;
                let output = ComprehensionOutput::Map {
                    key: Box::new(key),
                    value: Box::new(value),
                };
                return self.finish_comprehension(output, start, &TokenKind::RBrace, "map");
            }
            let output = ComprehensionOutput::Set(Box::new(self.parse_expr()?));
            return self.finish_comprehension(output, start, &TokenKind::RBrace, "set");
        }

        // Map vs. Set literal: a Map's first element is `expr ":"`. Parse the
        // first expression, then peek for `:`.
        let first = self.parse_expr()?;
        if matches!(self.peek(), TokenKind::Colon) {
            self.advance(); // `:`
            let first_val = self.parse_expr()?;
            let mut pairs = vec![(first, first_val)];
            while self.eat(&TokenKind::Comma) {
                if matches!(self.peek(), TokenKind::RBrace) {
                    break;
                }
                let k = self.parse_expr()?;
                self.expect(&TokenKind::Colon, "`:` between a map key and value")?;
                let v = self.parse_expr()?;
                pairs.push((k, v));
            }
            let close = self.expect(&TokenKind::RBrace, "`}` to close a map literal")?;
            Ok(Expr { kind: ExprKind::Map(pairs), span: start.merge(close.span) })
        } else {
            let mut items = vec![first];
            while self.eat(&TokenKind::Comma) {
                if matches!(self.peek(), TokenKind::RBrace) {
                    break;
                }
                items.push(self.parse_expr()?);
            }
            let close = self.expect(&TokenKind::RBrace, "`}` to close a set literal")?;
            Ok(Expr { kind: ExprKind::Set(items), span: start.merge(close.span) })
        }
    }

    /// Parse the tail of a comprehension after the output expression has been
    /// consumed: `"for" binder "in" iter [ "if" cond ] CLOSE`. `close` is the
    /// matching `]` / `}`; `kind` labels it for error messages.
    pub(crate) fn finish_comprehension(
        &mut self,
        output: ComprehensionOutput,
        start: Span,
        close: &TokenKind,
        kind: &str,
    ) -> PResult<Expr> {
        self.expect(&TokenKind::For, "`for` in a comprehension")?;
        let binder = self.parse_comprehension_binder()?;
        self.expect(&TokenKind::In, "`in` in a comprehension")?;
        // Parse the iterable and the filter below ternary precedence: a trailing
        // `if cond` is the comprehension's filter, not a ternary on the iterable
        // (`… for x in 1..=5 if x != 3` must not read `1..=5 if … else …`).
        let iter = self.parse_or()?;
        let cond = if matches!(self.peek(), TokenKind::If) {
            self.advance();
            Some(Box::new(self.parse_or()?))
        } else {
            None
        };
        let close_msg = match kind {
            "list" => "the bracket closing a list comprehension",
            "map" => "the bracket closing a map comprehension",
            "set" => "the bracket closing a set comprehension",
            _ => "the bracket closing a comprehension",
        };
        let close_tok = self.expect(close, close_msg)?;
        Ok(Expr {
            kind: ExprKind::Comprehension(Comprehension {
                output,
                binder,
                iter: Box::new(iter),
                cond,
            }),
            span: start.merge(close_tok.span),
        })
    }

    /// Parse a comprehension binder: a single `NAME` or a tuple `"(" NAME, … ")"`.
    pub(crate) fn parse_comprehension_binder(&mut self) -> PResult<ComprehensionBinder> {
        if matches!(self.peek(), TokenKind::LParen) {
            match self.parse_tuple_binder("a comprehension")? {
                Binder::Tuple(names) => Ok(ComprehensionBinder::Tuple(names)),
                // `parse_tuple_binder` never yields `Name`, but keep total.
                Binder::Name(n) => Ok(ComprehensionBinder::Name(n)),
            }
        } else {
            let (name, _) = self.expect_name("a comprehension binder name")?;
            Ok(ComprehensionBinder::Name(name))
        }
    }

    /// Lookahead: is there a top-level `for` keyword inside the just-opened
    /// bracket group? The cursor sits just past the opening bracket; we scan to
    /// the matching `close`, ignoring `for`s nested in inner bracket groups.
    pub(crate) fn brace_has_top_level_for(&self, close: &TokenKind) -> bool {
        let mut depth = 0i32;
        let mut i = 0;
        loop {
            let k = self.peek_n(i);
            match k {
                TokenKind::Eof => return false,
                _ if k == close && depth == 0 => return false,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth -= 1,
                TokenKind::For if depth == 0 => return true,
                _ => {}
            }
            // A stray over-close means our open's close is reached.
            if depth < 0 {
                return false;
            }
            i += 1;
        }
    }

    /// Lookahead for a brace comprehension: does a top-level `:` appear before
    /// the top-level `for`? Distinguishes a map comprehension (`{k: v for …}`)
    /// from a set comprehension (`{x for …}`). The cursor sits just past `{`.
    pub(crate) fn brace_colon_before_for(&self) -> bool {
        let mut depth = 0i32;
        let mut i = 0;
        loop {
            match self.peek_n(i) {
                TokenKind::Eof => return false,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                TokenKind::For if depth == 0 => return false,
                TokenKind::Colon if depth == 0 => return true,
                _ => {}
            }
            i += 1;
        }
    }

    /// `( expr )` grouping or `( a, b, … )` tuple. The current token is `(`.
    /// A single parenthesized expression stays pure grouping; a tuple needs at
    /// least one comma (two or more elements).
    pub(crate) fn parse_tuple_or_group(&mut self) -> PResult<Expr> {
        let start = self.cur_span();
        self.advance(); // `(`
        let first = self.parse_expr()?;
        if matches!(self.peek(), TokenKind::Comma) {
            // Tuple: collect the remaining comma-separated elements.
            let mut items = vec![first];
            while self.eat(&TokenKind::Comma) {
                if matches!(self.peek(), TokenKind::RParen) {
                    break; // tolerate a trailing comma
                }
                items.push(self.parse_expr()?);
            }
            let close = self.expect(&TokenKind::RParen, "`)` to close a tuple")?;
            Ok(Expr { kind: ExprKind::Tuple(items), span: start.merge(close.span) })
        } else {
            let close = self.expect(&TokenKind::RParen, "`)` to close a grouped expression")?;
            // Grouping is transparent; widen the span to include the parens.
            Ok(Expr { kind: first.kind, span: start.merge(close.span) })
        }
    }

    // =====================================================================
    // String interpolation sub-parsing (§1.5)
    // =====================================================================

    /// Build a [`StringLit`] from the lexer's `StrPart`s, recursively parsing
    /// each interpolation's nested token stream into an [`Expr`].
    pub(crate) fn build_string_lit(&mut self, parts: &[StrPart]) -> PResult<StringLit> {
        let mut segs = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                StrPart::Text(t) => segs.push(StrSeg::Text(t.clone())),
                StrPart::Interp(tokens) => {
                    let expr = self.parse_interpolation(tokens)?;
                    segs.push(StrSeg::Expr(Box::new(expr)));
                }
            }
        }
        Ok(StringLit { parts: segs })
    }

    /// Parse a single interpolation's nested token stream (terminated by `Eof`)
    /// as one `expr`. Runs a fresh sub-parser over the borrowed slice so cursor
    /// state never leaks back into the outer stream.
    pub(crate) fn parse_interpolation(&mut self, tokens: &[Token]) -> PResult<Expr> {
        let mut sub = Parser::new(tokens);
        // Tolerate a stray leading newline (defensive; interpolations are
        // single logical lines).
        sub.skip_newlines();
        let expr = sub.parse_expr()?;
        sub.skip_newlines();
        if !matches!(sub.peek(), TokenKind::Eof) {
            return Err(Diagnostic::parse(
                format!(
                    "unexpected {} after interpolation expression",
                    describe(sub.peek())
                ),
                sub.cur_span(),
            ));
        }
        Ok(expr)
    }
}
