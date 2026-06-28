use super::*;

impl<'a> Parser<'a> {
    /// `fn NAME "(" [param_list] ")" [ "->" type ] ":" suite`.
    ///
    /// The result clause is `-> type` (M2; the M1 `returns` keyword was
    /// dropped). A function with no `->` returns unit, exactly as before.
    pub(crate) fn parse_fn_decl(&mut self) -> PResult<FnDecl> {
        let start = self.cur_span();
        let doc = self.cur_doc();
        self.advance(); // `fn`
        let (name, _) = self.expect_name("a function name after `fn`")?;
        self.expect(&TokenKind::LParen, "`(` to open the parameter list")?;
        let params = self.parse_params()?;
        self.expect(&TokenKind::RParen, "`)` to close the parameter list")?;

        let returns = if matches!(self.peek(), TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };

        self.expect(&TokenKind::Colon, "`:` before the function body")?;
        let body = self.parse_suite()?;
        let span = start.merge(body.span);
        Ok(FnDecl { name, params, returns, body, doc, span })
    }

    /// `param_list = param , …` ; `param = "self" | NAME ":" type [ "=" expr ]`.
    /// A trailing `= expr` is a **default value** (M2 Wave 1).
    pub(crate) fn parse_params(&mut self) -> PResult<Vec<Param>> {
        self.parse_separated(&TokenKind::RParen, |p| match p.peek() {
            TokenKind::SelfKw => {
                p.advance();
                Ok(Param::SelfRecv)
            }
            TokenKind::Name(_) => {
                let (name, _) = p.expect_name("a parameter name")?;
                p.expect(&TokenKind::Colon, "`:` after the parameter name")?;
                let ty = p.parse_type()?;
                // Optional default value: `NAME: type = expr` (M2 Wave 1).
                let default = if p.eat(&TokenKind::Eq) {
                    Some(p.parse_expr()?)
                } else {
                    None
                };
                Ok(Param::Named { name, ty, default })
            }
            other => Err(Diagnostic::parse(
                format!("expected a parameter, found {}", describe(other)),
                p.cur_span(),
            )),
        })
    }

    /// `struct NAME ":" NEWLINE INDENT field_decl+ DEDENT`. Methods are **not**
    /// allowed in a struct body — they are defined in an `impl` block (§4.6), so
    /// there is exactly one way to add a method.
    pub(crate) fn parse_struct(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        let doc = self.cur_doc();
        self.advance(); // `struct`
        let (name, _) = self.expect_name("a struct name")?;
        self.expect(&TokenKind::Colon, "`:` after the struct name")?;
        self.skip_newlines_to_indent()?;

        let mut fields = Vec::new();
        self.skip_newlines();
        while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
            match self.peek() {
                TokenKind::Fn => {
                    return Err(Diagnostic::parse(
                        "methods are defined in an `impl` block, not the struct body",
                        self.cur_span(),
                    ));
                }
                TokenKind::Name(_) => {
                    let fstart = self.cur_span();
                    let fdoc = self.cur_doc();
                    let (fname, _) = self.expect_name("a field name")?;
                    self.expect(&TokenKind::Colon, "`:` after the field name")?;
                    let ty = self.parse_type()?;
                    let span = fstart.merge(ty.span);
                    self.expect_stmt_newline()?;
                    fields.push(FieldDecl { name: fname, ty, doc: fdoc, span });
                }
                other => {
                    return Err(Diagnostic::parse(
                        format!("expected a field, found {}", describe(other)),
                        self.cur_span(),
                    ));
                }
            }
            self.skip_newlines();
        }
        let end = self.cur_span();
        self.expect(&TokenKind::Dedent, "end of the struct body")?;
        let span = start.merge(end);
        Ok(Stmt {
            kind: StmtKind::Struct(StructDecl { name, fields, doc, span }),
            span,
        })
    }

    /// `enum NAME ":" NEWLINE INDENT variant_decl+ DEDENT`.
    pub(crate) fn parse_enum(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        let doc = self.cur_doc();
        self.advance(); // `enum`
        let (name, _) = self.expect_name("an enum name")?;
        self.expect(&TokenKind::Colon, "`:` after the enum name")?;
        self.skip_newlines_to_indent()?;

        let mut variants = Vec::new();
        self.skip_newlines();
        while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
            let vstart = self.cur_span();
            let vdoc = self.cur_doc();
            let (vname, vname_span) = self.expect_name("a variant name")?;
            let mut end = vname_span;
            let payload = if matches!(self.peek(), TokenKind::LParen) {
                let (pl, close) = self.parse_payload()?;
                end = close;
                Some(pl)
            } else {
                None
            };
            self.expect_stmt_newline()?;
            let span = vstart.merge(end);
            variants.push(VariantDecl { name: vname, payload, doc: vdoc, span });
            self.skip_newlines();
        }
        let end = self.cur_span();
        self.expect(&TokenKind::Dedent, "end of the enum body")?;
        let span = start.merge(end);
        Ok(Stmt {
            kind: StmtKind::Enum(EnumDecl { name, variants, doc, span }),
            span,
        })
    }

    /// `"(" payload ")"` — positional `type, …` or named `NAME ":" type, …`.
    /// Returns the payload and the span of the closing `)`.
    pub(crate) fn parse_payload(&mut self) -> PResult<(Payload, Span)> {
        self.expect(&TokenKind::LParen, "`(` to open the payload")?;
        // Decide positional vs. named by lookahead: `NAME ":"` ⇒ named.
        let named = matches!(self.peek(), TokenKind::Name(_))
            && matches!(self.peek_n(1), TokenKind::Colon);

        let payload = if named {
            let fields = self.parse_separated(&TokenKind::RParen, |p| {
                let (fname, _) = p.expect_name("a payload field name")?;
                p.expect(&TokenKind::Colon, "`:` after the payload field name")?;
                let ty = p.parse_type()?;
                Ok((fname, ty))
            })?;
            Payload::Named(fields)
        } else {
            let types = self.parse_separated(&TokenKind::RParen, Self::parse_type)?;
            Payload::Positional(types)
        };
        let close = self.expect(&TokenKind::RParen, "`)` to close the payload")?;
        Ok((payload, close.span))
    }

    /// `impl NAME ":" NEWLINE INDENT fn_decl+ DEDENT`.
    pub(crate) fn parse_impl(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        self.advance(); // `impl`
        let (type_name, _) = self.expect_name("a type name after `impl`")?;
        self.expect(&TokenKind::Colon, "`:` after the impl type")?;
        self.skip_newlines_to_indent()?;

        let mut methods = Vec::new();
        self.skip_newlines();
        while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
            match self.peek() {
                TokenKind::Fn => methods.push(self.parse_fn_decl()?),
                other => {
                    return Err(Diagnostic::parse(
                        format!("expected a method (`fn`), found {}", describe(other)),
                        self.cur_span(),
                    ));
                }
            }
            self.skip_newlines();
        }
        let end = self.cur_span();
        self.expect(&TokenKind::Dedent, "end of the impl body")?;
        let span = start.merge(end);
        Ok(Stmt { kind: StmtKind::Impl(ImplDecl { type_name, methods, span }), span })
    }

    /// Consume `NEWLINE INDENT` introducing a declaration body, tolerating blank
    /// lines before the `INDENT`.
    pub(crate) fn skip_newlines_to_indent(&mut self) -> PResult<()> {
        if !matches!(self.peek(), TokenKind::Newline) {
            return Err(Diagnostic::parse(
                format!(
                    "expected a newline and indented body, found {}",
                    describe(self.peek())
                ),
                self.cur_span(),
            ));
        }
        self.skip_newlines();
        self.expect(&TokenKind::Indent, "an indented body")?;
        Ok(())
    }
}
