use super::*;

impl<'a> Parser<'a> {
    // =====================================================================
    // Suites / blocks (§3)
    // =====================================================================

    /// `suite = simple_stmt NEWLINE | NEWLINE INDENT statement+ DEDENT`.
    ///
    /// The caller has already consumed the introducing `:`.
    pub(crate) fn parse_suite(&mut self) -> PResult<Block> {
        if matches!(self.peek(), TokenKind::Newline) {
            // Indented block. There may be blank lines before the INDENT.
            let start = self.cur_span();
            self.skip_newlines();
            self.expect(&TokenKind::Indent, "an indented block after `:`")?;
            let mut stmts = Vec::new();
            self.skip_newlines();
            while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
                let stmt = self.parse_statement()?;
                stmts.push(stmt);
                self.skip_newlines();
            }
            let end = self.cur_span();
            self.expect(&TokenKind::Dedent, "end of the indented block")?;
            let span = block_span(start, end, &stmts);
            Ok(Block { stmts, span })
        } else {
            // Inline form: exactly one simple statement.
            let stmt = self.parse_simple_stmt()?;
            let span = stmt.span;
            self.expect_stmt_newline()?;
            Ok(Block { stmts: vec![stmt], span })
        }
    }

    // =====================================================================
    // Compound statements (§4.2–§4.6)
    // =====================================================================

    /// `if expr ":" suite { elif expr ":" suite } [ else ":" suite ]`.
    pub(crate) fn parse_if(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        self.advance(); // `if`
        let mut arms = Vec::new();
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::Colon, "`:` after the `if` condition")?;
        let body = self.parse_suite()?;
        let mut end = body.span;
        arms.push((cond, body));

        while matches!(self.peek(), TokenKind::Elif) {
            self.advance();
            let cond = self.parse_expr()?;
            self.expect(&TokenKind::Colon, "`:` after the `elif` condition")?;
            let body = self.parse_suite()?;
            end = body.span;
            arms.push((cond, body));
        }

        let else_body = if matches!(self.peek(), TokenKind::Else) {
            self.advance();
            self.expect(&TokenKind::Colon, "`:` after `else`")?;
            let body = self.parse_suite()?;
            end = body.span;
            Some(body)
        } else {
            None
        };

        let span = start.merge(end);
        Ok(Stmt { kind: StmtKind::If(IfStmt { arms, else_body }), span })
    }

    /// `while expr ":" suite`.
    pub(crate) fn parse_while(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        self.advance(); // `while`
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::Colon, "`:` after the `while` condition")?;
        let body = self.parse_suite()?;
        let span = start.merge(body.span);
        Ok(Stmt { kind: StmtKind::While(WhileStmt { cond, body }), span })
    }

    /// `for ( NAME | "(" NAME, … ")" ) in expr ":" suite` (the tuple-binder form
    /// is `for (k, v) in m.items()`).
    pub(crate) fn parse_for(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        self.advance(); // `for`
        let binder = if matches!(self.peek(), TokenKind::LParen) {
            self.parse_tuple_binder("a `for` loop")?
        } else {
            let (var, _) = self.expect_name("a loop variable name after `for`")?;
            Binder::Name(var)
        };
        self.expect(&TokenKind::In, "`in` after the `for` variable")?;
        let iter = self.parse_expr()?;
        self.expect(&TokenKind::Colon, "`:` after the `for` iterable")?;
        let body = self.parse_suite()?;
        let span = start.merge(body.span);
        Ok(Stmt {
            kind: StmtKind::For(ForStmt { binder, iter, body }),
            span,
        })
    }

    /// `match_expr = "match" expr ":" NEWLINE INDENT match_arm+ DEDENT`.
    pub(crate) fn parse_match_expr(&mut self) -> PResult<Expr> {
        let start = self.cur_span();
        self.advance(); // `match`
        let scrutinee = self.parse_expr()?;
        self.expect(&TokenKind::Colon, "`:` after the match scrutinee")?;
        self.skip_newlines_to_indent()?;

        let mut arms = Vec::new();
        self.skip_newlines();
        while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
            arms.push(self.parse_match_arm()?);
            self.skip_newlines();
        }
        let end = self.cur_span();
        self.expect(&TokenKind::Dedent, "end of the match body")?;
        let span = start.merge(end);
        Ok(Expr {
            kind: ExprKind::Match(MatchExpr {
                scrutinee: Box::new(scrutinee),
                arms,
            }),
            span,
        })
    }

    /// `match_arm = pattern [ "if" expr ] ":" arm_body`.
    /// `arm_body = expr NEWLINE | NEWLINE INDENT statement+ DEDENT` — both stored
    /// as a [`Block`]. An inline expr arm is wrapped as a one-statement block.
    ///
    /// A match guard (`pattern if cond:`) makes the arm conditional: it only
    /// fires when `cond` is `Bool` and true. A guarded arm does not
    /// count toward exhaustiveness ([`crate::checks`]).
    pub(crate) fn parse_match_arm(&mut self) -> PResult<MatchArm> {
        let start = self.cur_span();
        let pattern = self.parse_pattern()?;

        // Optional `if COND` guard between the pattern and the `:`.
        let guard = if self.eat(&TokenKind::If) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        self.expect(&TokenKind::Colon, "`:` after a match pattern")?;

        let body = if matches!(self.peek(), TokenKind::Newline) {
            // Block arm.
            self.parse_suite()?
        } else {
            // Inline expression arm → wrap as a one-expr-statement block.
            let expr = self.parse_expr()?;
            let espan = expr.span;
            self.expect_stmt_newline()?;
            let stmt = Stmt { kind: StmtKind::Expr(expr), span: espan };
            Block { stmts: vec![stmt], span: espan }
        };

        let span = start.merge(body.span);
        Ok(MatchArm { pattern, guard, body, span })
    }
}
