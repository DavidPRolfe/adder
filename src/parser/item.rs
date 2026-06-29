use super::*;

/// One member of a `trait` body: a required signature or a default method.
enum TraitItem {
    Required(TraitSig),
    Default(FnDecl),
}

impl<'a> Parser<'a> {
    /// `fn NAME [type_params] "(" [param_list] ")" [ "->" type ] ":" suite`.
    ///
    /// The result clause is `-> type`. A function with no `->` returns unit. An
    /// optional `[T: Bound, …]` after the name declares type parameters (parsed,
    /// not checked).
    pub(crate) fn parse_fn_decl(&mut self) -> PResult<FnDecl> {
        let start = self.cur_span();
        let doc = self.cur_doc();
        self.advance(); // `fn`
        let (name, _) = self.expect_name("a function name after `fn`")?;
        let type_params = self.parse_opt_type_params()?;
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
        Ok(FnDecl { name, type_params, params, returns, body, doc, span })
    }

    /// `"[" type_param , … "]"` where `type_param = NAME [ ":" bound ]` and
    /// `bound = NAME { "and" NAME }` (spec §10). **Parsed, not checked** —
    /// the bounds document intent and the parameters are erased at runtime.
    pub(crate) fn parse_type_params(&mut self) -> PResult<Vec<TypeParam>> {
        self.expect(&TokenKind::LBracket, "`[` to open type parameters")?;
        let params = self.parse_separated(&TokenKind::RBracket, |p| {
            let start = p.cur_span();
            let (name, name_span) = p.expect_name("a type parameter name")?;
            let mut bounds = Vec::new();
            let mut end = name_span;
            if p.eat(&TokenKind::Colon) {
                loop {
                    let (b, bspan) = p.expect_name("a trait bound after `:`")?;
                    bounds.push(b);
                    end = bspan;
                    if !p.eat(&TokenKind::And) {
                        break;
                    }
                }
            }
            Ok(TypeParam { name, bounds, span: start.merge(end) })
        })?;
        self.expect(&TokenKind::RBracket, "`]` to close type parameters")?;
        Ok(params)
    }

    /// Parse a `[T, …]` type-parameter list if one is present, else empty.
    fn parse_opt_type_params(&mut self) -> PResult<Vec<TypeParam>> {
        if matches!(self.peek(), TokenKind::LBracket) {
            self.parse_type_params()
        } else {
            Ok(Vec::new())
        }
    }

    /// Consume and discard a `[T, …]` generic-argument list on a type path in an
    /// `impl` header — the arguments are erased at runtime.
    fn eat_opt_type_args(&mut self) -> PResult<()> {
        if matches!(self.peek(), TokenKind::LBracket) {
            self.advance();
            let _ = self.parse_separated(&TokenKind::RBracket, Self::parse_type)?;
            self.expect(&TokenKind::RBracket, "`]` to close type arguments")?;
        }
        Ok(())
    }

    /// `param_list = param , …` ; `param = "self" | NAME ":" type [ "=" expr ]`.
    /// A trailing `= expr` is a **default value**.
    pub(crate) fn parse_params(&mut self) -> PResult<Vec<Param>> {
        self.parse_separated(&TokenKind::RParen, |p| match p.peek() {
            TokenKind::SelfKw => {
                p.advance();
                Ok(Param::SelfRecv)
            }
            TokenKind::Name(_) => {
                let (name, name_span) = p.expect_name("a parameter name")?;
                p.expect(&TokenKind::Colon, "`:` after the parameter name")?;
                let ty = p.parse_type()?;
                // Optional default value: `NAME: type = expr`.
                let default = if p.eat(&TokenKind::Eq) {
                    Some(p.parse_expr()?)
                } else {
                    None
                };
                let mut span = name_span.merge(ty.span);
                if let Some(d) = &default {
                    span = span.merge(d.span);
                }
                Ok(Param::Named { name, ty, default, span })
            }
            other => Err(Diagnostic::parse(
                format!("expected a parameter, found {}", describe(other)),
                p.cur_span(),
            )),
        })
    }

    /// `[derive_clause] "struct" NAME [type_params] ":" NEWLINE INDENT field_decl+ DEDENT`.
    /// Methods are **not** allowed in a struct body — they are defined in an
    /// `impl` block (§4.6), so there is exactly one way to add a method. The
    /// `derives` come from a preceding `derive` line (passed by the caller).
    pub(crate) fn parse_struct(&mut self, derives: Vec<String>) -> PResult<Stmt> {
        let start = self.cur_span();
        let doc = self.cur_doc();
        self.advance(); // `struct`
        let (name, _) = self.expect_name("a struct name")?;
        let type_params = self.parse_opt_type_params()?;
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
            kind: StmtKind::Struct(StructDecl { name, type_params, derives, fields, doc, span }),
            span,
        })
    }

    /// `[derive_clause] "enum" NAME [type_params] ":" NEWLINE INDENT variant_decl+ DEDENT`.
    pub(crate) fn parse_enum(&mut self, derives: Vec<String>) -> PResult<Stmt> {
        let start = self.cur_span();
        let doc = self.cur_doc();
        self.advance(); // `enum`
        let (name, _) = self.expect_name("an enum name")?;
        let type_params = self.parse_opt_type_params()?;
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
            kind: StmtKind::Enum(EnumDecl { name, type_params, derives, variants, doc, span }),
            span,
        })
    }

    /// `"derive" NAME , … NEWLINE` followed by a `struct`/`enum` (spec §7.1).
    /// `derive` is a contextual keyword recognized at declaration head when it is
    /// followed by a trait name; this attaches the requested derives to the
    /// declaration that follows.
    pub(crate) fn parse_derived_decl(&mut self) -> PResult<Stmt> {
        self.advance(); // `derive` (lexed as a `Name`)
        let mut derives = Vec::new();
        loop {
            let (name, _) = self.expect_name("a trait name to derive (e.g. `Ord`)")?;
            derives.push(name);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect_stmt_newline()?;
        self.skip_newlines();
        match self.peek() {
            TokenKind::Struct => self.parse_struct(derives),
            TokenKind::Enum => self.parse_enum(derives),
            other => Err(Diagnostic::parse(
                format!(
                    "`derive` must be followed by a `struct` or `enum`, found {}",
                    describe(other)
                ),
                self.cur_span(),
            )),
        }
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

    /// `impl [type_params] type_path [ "for" type_path ] ":" NEWLINE INDENT fn_decl+ DEDENT`.
    ///
    /// Two forms: an inherent `impl Type:` block (`trait_name = None`), or a
    /// trait impl `impl Trait for Type:` — the `for` is the pivot, so the first
    /// path is the trait and the second the implementing type. Generic arguments
    /// on either path (`impl Stack[T]:`) are parsed and erased.
    pub(crate) fn parse_impl(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        self.advance(); // `impl`
        let type_params = self.parse_opt_type_params()?;
        let (first, _) = self.expect_name("a type name after `impl`")?;
        self.eat_opt_type_args()?;

        let (trait_name, type_name) = if self.eat(&TokenKind::For) {
            let (ty, _) = self.expect_name("a type name after `for`")?;
            self.eat_opt_type_args()?;
            (Some(first), ty)
        } else {
            (None, first)
        };

        self.expect(&TokenKind::Colon, "`:` after the impl header")?;
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
        Ok(Stmt {
            kind: StmtKind::Impl(ImplDecl { type_name, trait_name, type_params, methods, span }),
            span,
        })
    }

    /// `trait NAME [type_params] ":" NEWLINE INDENT trait_item+ DEDENT` (spec
    /// §7). A trait item is either a **required** signature (`fn … NEWLINE`,
    /// no body) or a **default** method (`fn … ":" suite`).
    pub(crate) fn parse_trait(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        let doc = self.cur_doc();
        self.advance(); // `trait`
        let (name, _) = self.expect_name("a trait name after `trait`")?;
        let type_params = self.parse_opt_type_params()?;
        self.expect(&TokenKind::Colon, "`:` after the trait name")?;
        self.skip_newlines_to_indent()?;

        let mut required = Vec::new();
        let mut defaults = Vec::new();
        self.skip_newlines();
        while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
            match self.peek() {
                TokenKind::Fn => match self.parse_trait_method()? {
                    TraitItem::Required(sig) => required.push(sig),
                    TraitItem::Default(f) => defaults.push(f),
                },
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
        self.expect(&TokenKind::Dedent, "end of the trait body")?;
        let span = start.merge(end);
        Ok(Stmt {
            kind: StmtKind::Trait(TraitDecl { name, type_params, required, defaults, doc, span }),
            span,
        })
    }

    /// One `fn` in a trait body: a required signature (terminated by a newline)
    /// or a default method (a `:` body). The presence of the `:` is the pivot.
    fn parse_trait_method(&mut self) -> PResult<TraitItem> {
        let start = self.cur_span();
        let doc = self.cur_doc();
        self.advance(); // `fn`
        let (name, _) = self.expect_name("a method name after `fn`")?;
        let type_params = self.parse_opt_type_params()?;
        self.expect(&TokenKind::LParen, "`(` to open the parameter list")?;
        let params = self.parse_params()?;
        let rparen = self.expect(&TokenKind::RParen, "`)` to close the parameter list")?;
        let mut header_end = rparen.span;

        let returns = if matches!(self.peek(), TokenKind::Arrow) {
            self.advance();
            let ty = self.parse_type()?;
            header_end = ty.span;
            Some(ty)
        } else {
            None
        };

        if self.eat(&TokenKind::Colon) {
            // Default method — has a body.
            let body = self.parse_suite()?;
            let span = start.merge(body.span);
            Ok(TraitItem::Default(FnDecl { name, type_params, params, returns, body, doc, span }))
        } else {
            // Required signature — no body, terminated by the statement newline.
            self.expect_stmt_newline()?;
            let span = start.merge(header_end);
            Ok(TraitItem::Required(TraitSig { name, params, returns, doc, span }))
        }
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
