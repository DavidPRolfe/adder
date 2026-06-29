use super::*;

impl<'a> Parser<'a> {
    // =====================================================================
    // Program & statements (§2, §4)
    // =====================================================================

    pub(crate) fn parse_program(&mut self) -> Program {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at_eof() {
            // A stray Dedent at top level shouldn't loop forever.
            if matches!(self.peek(), TokenKind::Dedent | TokenKind::Indent) {
                self.advance();
                continue;
            }
            match self.parse_statement() {
                Ok(stmt) => stmts.push(stmt),
                Err(d) => {
                    self.errors.push(d);
                    self.recover_to_stmt_boundary();
                }
            }
            self.skip_newlines();
        }
        Program { stmts }
    }

    /// `statement = simple_stmt NEWLINE | compound_stmt`.
    pub(crate) fn parse_statement(&mut self) -> PResult<Stmt> {
        match self.peek() {
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Fn => {
                let f = self.parse_fn_decl()?;
                let span = f.span;
                Ok(Stmt { kind: StmtKind::Fn(f), span })
            }
            TokenKind::Struct => self.parse_struct(Vec::new()),
            TokenKind::Enum => self.parse_enum(Vec::new()),
            TokenKind::Impl => self.parse_impl(),
            TokenKind::Trait => self.parse_trait(),
            // `derive` is a contextual keyword: only at a declaration head, and
            // only when a trait name follows (a bare `derive` is still an
            // ordinary identifier / expression statement).
            TokenKind::Name(n) if n == "derive" && matches!(self.peek_n(1), TokenKind::Name(_)) => {
                self.parse_derived_decl()
            }
            _ => {
                let stmt = self.parse_simple_stmt()?;
                self.expect_stmt_newline()?;
                Ok(stmt)
            }
        }
    }

    /// Consume the `Newline` that terminates a simple statement. At EOF (or just
    /// before a `Dedent`) the lexer may omit it; tolerate that.
    pub(crate) fn expect_stmt_newline(&mut self) -> PResult<()> {
        match self.peek() {
            TokenKind::Newline => {
                self.advance();
                Ok(())
            }
            TokenKind::Eof | TokenKind::Dedent => Ok(()),
            // The statement's expression ended with a block that consumed its own
            // terminating `Dedent` — e.g. a block-form `match` used as a binding /
            // `return` / expression RHS (`label = match x: …`). The block already
            // terminated the statement, so no separate `Newline` precedes the next
            // sibling statement; accept without consuming a token.
            _ if matches!(self.prev_kind(), Some(TokenKind::Dedent)) => Ok(()),
            other => Err(Diagnostic::parse(
                format!("expected end of line, found {}", describe(other)),
                self.cur_span(),
            )),
        }
    }

    /// The kind of the most-recently-consumed token, if any.
    pub(crate) fn prev_kind(&self) -> Option<&TokenKind> {
        self.pos.checked_sub(1).and_then(|i| self.tokens.get(i)).map(|t| &t.kind)
    }

    /// `simple_stmt = binding | assignment | return | break | continue | expr`.
    pub(crate) fn parse_simple_stmt(&mut self) -> PResult<Stmt> {
        match self.peek() {
            TokenKind::Val => self.parse_val_binding(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Break => {
                let span = self.cur_span();
                self.advance();
                Ok(Stmt { kind: StmtKind::Break, span })
            }
            TokenKind::Continue => {
                let span = self.cur_span();
                self.advance();
                Ok(Stmt { kind: StmtKind::Continue, span })
            }
            // A line starting with NAME is a binding/assignment iff a top-level
            // `=` (or `: type =`) follows the l-value (§4.1). Otherwise it is an
            // expression statement.
            TokenKind::Name(_) if self.looks_like_binding_or_assign() => {
                self.parse_binding_or_assign()
            }
            // A line starting with `self` is a `self.field`/`self[i]`
            // assignment iff a top-level `=` follows a non-empty path. A bare
            // `self` is not assignable and falls through to an expression.
            TokenKind::SelfKw if self.looks_like_self_assign() => self.parse_self_assign(),
            _ => {
                let expr = self.parse_expr()?;
                let span = expr.span;
                Ok(Stmt { kind: StmtKind::Expr(expr), span })
            }
        }
    }

    /// `return [expr]`.
    pub(crate) fn parse_return(&mut self) -> PResult<Stmt> {
        let kw = self.cur_span();
        self.advance(); // `return`
        // `return` with no value: the line ends immediately.
        if matches!(self.peek(), TokenKind::Newline | TokenKind::Eof | TokenKind::Dedent) {
            return Ok(Stmt { kind: StmtKind::Return(None), span: kw });
        }
        let expr = self.parse_expr()?;
        let span = kw.merge(expr.span);
        Ok(Stmt { kind: StmtKind::Return(Some(expr)), span })
    }

    /// `val ( NAME | "(" NAME, … ")" ) [":" type] "=" expr` (the tuple-binder
    /// form is `val (a, b) = pair`; a tuple binder may not carry a type
    /// annotation).
    pub(crate) fn parse_val_binding(&mut self) -> PResult<Stmt> {
        let kw = self.cur_span();
        self.advance(); // `val`
        // Tuple destructure binder: `val (a, b, …) = …`.
        if matches!(self.peek(), TokenKind::LParen) {
            let binder = self.parse_tuple_binder("a `val` binding")?;
            self.expect(&TokenKind::Eq, "`=` in a `val` binding")?;
            let value = self.parse_expr()?;
            let span = kw.merge(value.span);
            return Ok(Stmt {
                kind: StmtKind::Binding(Binding {
                    binder,
                    is_val: true,
                    ty: None,
                    value,
                }),
                span,
            });
        }
        let (name, _) = self.expect_name("a binding name after `val`")?;
        let ty = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&TokenKind::Eq, "`=` in a `val` binding")?;
        let value = self.parse_expr()?;
        let span = kw.merge(value.span);
        Ok(Stmt {
            kind: StmtKind::Binding(Binding {
                binder: Binder::Name(name),
                is_val: true,
                ty,
                value,
            }),
            span,
        })
    }

    /// Parse a tuple binder `"(" NAME, … ")"` (two or more names), used by `val`
    /// and `for` destructuring. The current token is the opening `(`.
    pub(crate) fn parse_tuple_binder(&mut self, ctx: &str) -> PResult<Binder> {
        self.expect(&TokenKind::LParen, "`(` to open a tuple binder")?;
        let names = self.parse_separated(&TokenKind::RParen, |p| {
            let (n, _) = p.expect_name("a name in the tuple binder")?;
            Ok(n)
        })?;
        self.expect(&TokenKind::RParen, "`)` to close a tuple binder")?;
        if names.len() < 2 {
            return Err(Diagnostic::parse(
                format!("a tuple binder in {ctx} needs at least two names"),
                self.cur_span(),
            ));
        }
        Ok(Binder::Tuple(names))
    }

    /// Lookahead: does a line starting with `NAME` begin a binding/assignment?
    ///
    /// True iff after the l-value (`NAME { ".NAME" | "[expr]" }`) the next
    /// top-level token is `=`, or it is `NAME ":" type "="` (a typed mutable
    /// binding — only for a *bare* name, since `target` has no type form).
    pub(crate) fn looks_like_binding_or_assign(&self) -> bool {
        // `NAME ":" ... "="` typed binding (bare name only).
        if matches!(self.peek_n(1), TokenKind::Colon) {
            return self.colon_type_then_eq(2);
        }
        // Otherwise scan a target path and check for a top-level `=`.
        let mut i = 1; // we already know index 0 is NAME
        loop {
            match self.peek_n(i) {
                TokenKind::Dot => {
                    // `.NAME`
                    if matches!(self.peek_n(i + 1), TokenKind::Name(_)) {
                        i += 2;
                    } else {
                        return false;
                    }
                }
                TokenKind::LBracket => {
                    // `[ ... ]` — skip a balanced bracket group.
                    match self.skip_balanced(i, &TokenKind::LBracket, &TokenKind::RBracket) {
                        Some(next) => i = next,
                        None => return false,
                    }
                }
                TokenKind::Eq => return true,
                _ => return false,
            }
        }
    }

    /// Starting at offset `start` (pointing just past a `:`), does a `type`
    /// followed by `=` appear at the top level of this line? Used to recognise
    /// `NAME ":" type "="` typed mutable bindings without fully parsing the type.
    pub(crate) fn colon_type_then_eq(&self, start: usize) -> bool {
        let mut i = start;
        // A type is `NAME ["[" ... "]"] ["?"]` or `()`. We only need to walk to
        // the `=`, skipping any balanced brackets/parens.
        loop {
            match self.peek_n(i) {
                TokenKind::Eq => return true,
                TokenKind::Newline | TokenKind::Eof | TokenKind::Dedent => return false,
                TokenKind::LBracket => match self.skip_balanced(
                    i,
                    &TokenKind::LBracket,
                    &TokenKind::RBracket,
                ) {
                    Some(next) => i = next,
                    None => return false,
                },
                TokenKind::LParen => match self.skip_balanced(
                    i,
                    &TokenKind::LParen,
                    &TokenKind::RParen,
                ) {
                    Some(next) => i = next,
                    None => return false,
                },
                _ => i += 1,
            }
        }
    }

    /// Skip a balanced `open..close` group that starts at offset `at`. Returns
    /// the offset just past the matching close, or `None` if unbalanced.
    pub(crate) fn skip_balanced(&self, at: usize, open: &TokenKind, close: &TokenKind) -> Option<usize> {
        debug_assert!(self.peek_n(at) == open);
        let mut depth = 0usize;
        let mut i = at;
        loop {
            let k = self.peek_n(i);
            if k == open {
                depth += 1;
            } else if k == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            } else if matches!(k, TokenKind::Eof) {
                return None;
            }
            i += 1;
        }
    }

    /// Parse a binding (`NAME ":" type "=" expr` / `NAME "=" expr`) or an
    /// assignment to a `target`. The caller has already confirmed via
    /// [`Self::looks_like_binding_or_assign`] that a top-level `=` follows.
    pub(crate) fn parse_binding_or_assign(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        let (name, name_span) = self.expect_name("a name")?;

        // Typed mutable binding: `NAME ":" type "=" expr`.
        if self.eat(&TokenKind::Colon) {
            let ty = self.parse_type()?;
            self.expect(&TokenKind::Eq, "`=` after the binding type")?;
            let value = self.parse_expr()?;
            let span = start.merge(value.span);
            return Ok(Stmt {
                kind: StmtKind::Binding(Binding {
                    binder: Binder::Name(name),
                    is_val: false,
                    ty: Some(ty),
                    value,
                }),
                span,
            });
        }

        // Either a plain-name binding/assignment or a path assignment.
        let (path, last_span) = self.parse_target_path(name_span)?;

        self.expect(&TokenKind::Eq, "`=` in an assignment")?;
        let value = self.parse_expr()?;
        let span = start.merge(value.span);

        if path.is_empty() {
            // A bare `NAME = expr` is an (inferred mutable) binding (§4.1): the
            // first occurrence introduces it; the interpreter decides intro vs.
            // reassign. We always emit a `Binding` for the bare-name form so the
            // surface shape is faithful.
            Ok(Stmt {
                kind: StmtKind::Binding(Binding {
                    binder: Binder::Name(name),
                    is_val: false,
                    ty: None,
                    value,
                }),
                span,
            })
        } else {
            let target_span = start.merge(last_span);
            Ok(Stmt {
                kind: StmtKind::Assign(Assign {
                    target: Target { base: name, path, span: target_span },
                    value,
                }),
                span,
            })
        }
    }

    /// Parse a target path `{ ".NAME" | "[" expr "]" }` after a base, returning
    /// the segments and the span of the last one (or `base_span` if empty).
    pub(crate) fn parse_target_path(&mut self, base_span: Span) -> PResult<(Vec<TargetSeg>, Span)> {
        let mut path = Vec::new();
        let mut last_span = base_span;
        loop {
            match self.peek() {
                TokenKind::Dot => {
                    self.advance();
                    let (field, fspan) = self.expect_name("a field name after `.`")?;
                    last_span = fspan;
                    path.push(TargetSeg::Field(field));
                }
                TokenKind::LBracket => {
                    self.advance();
                    let idx = self.parse_expr()?;
                    let close = self.expect(&TokenKind::RBracket, "`]` to close an index")?;
                    last_span = close.span;
                    path.push(TargetSeg::Index(idx));
                }
                _ => break,
            }
        }
        Ok((path, last_span))
    }

    /// Lookahead: does a line starting with `self` begin a `self.field` /
    /// `self[i]` assignment? True iff at least one path segment is followed by a
    /// top-level `=`. (A bare `self =` is not an assignment.)
    pub(crate) fn looks_like_self_assign(&self) -> bool {
        let mut i = 1; // index 0 is `self`
        let mut saw_segment = false;
        loop {
            match self.peek_n(i) {
                TokenKind::Dot => {
                    if matches!(self.peek_n(i + 1), TokenKind::Name(_)) {
                        i += 2;
                        saw_segment = true;
                    } else {
                        return false;
                    }
                }
                TokenKind::LBracket => {
                    match self.skip_balanced(i, &TokenKind::LBracket, &TokenKind::RBracket) {
                        Some(next) => {
                            i = next;
                            saw_segment = true;
                        }
                        None => return false,
                    }
                }
                TokenKind::Eq => return saw_segment,
                _ => return false,
            }
        }
    }

    /// Parse a `self`-rooted assignment: `self ( ".NAME" | "[" expr "]" )+ "=" expr`.
    /// The l-value `base` is the string `"self"`, which the interpreter resolves
    /// to the method receiver and mutates through its shared reference.
    pub(crate) fn parse_self_assign(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        self.advance(); // `self`
        let (path, last_span) = self.parse_target_path(start)?;
        self.expect(&TokenKind::Eq, "`=` in an assignment")?;
        let value = self.parse_expr()?;
        let span = start.merge(value.span);
        let target_span = start.merge(last_span);
        Ok(Stmt {
            kind: StmtKind::Assign(Assign {
                target: Target { base: "self".to_string(), path, span: target_span },
                value,
            }),
            span,
        })
    }
}
