//! Stage 2 — **parsing**: token stream → AST.
//!
//! Owned by the *parser agent*. Consumes the `Vec<Token>` from
//! [`crate::lexer::lex`] and produces a [`Program`] following grammar §2–§7.
//! Recursively parses each string-interpolation's nested token stream
//! ([`crate::token::StrPart::Interp`]) into an [`crate::ast::Expr`] to fill
//! [`crate::ast::StrSeg::Expr`].
//!
//! Contract: returns the parsed [`Program`] on success, or **all** collected
//! [`Diagnostic`]s on failure (the parser may recover and report several
//! errors at once).
//!
//! ## Approach
//!
//! Recursive descent over the grammar, with the expression precedence ladder
//! (§5.4) realized as one method per precedence level (lowest → highest). The
//! ladder is short and fixed, so explicit per-level methods are clearer than a
//! table-driven precedence climber and make the awkward rules — non-associative
//! comparison, right-associative `**` binding tighter than unary minus, and the
//! lambda/grouping lookahead — easy to encode exactly.
//!
//! Statement-level errors are recovered: on a parse error inside a top-level
//! statement we record the diagnostic and resync to the next `Newline`/`Dedent`
//! so a single bad line doesn't mask the rest of the file.

use crate::ast::*;
use crate::error::Diagnostic;
use crate::token::{Span, StrPart, Token, TokenKind};

/// Parse a token stream into a [`Program`].
pub fn parse(tokens: &[Token]) -> Result<Program, Vec<Diagnostic>> {
    let mut p = Parser::new(tokens);
    let program = p.parse_program();
    if p.errors.is_empty() {
        Ok(program)
    } else {
        Err(p.errors)
    }
}

/// Recursive-descent parser over a borrowed token slice.
struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    errors: Vec<Diagnostic>,
}

/// A typed early-exit used internally so a failed sub-parse can unwind to the
/// nearest statement boundary, where it is recorded and recovered from.
type PResult<T> = Result<T, Diagnostic>;

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Parser { tokens, pos: 0, errors: Vec::new() }
    }

    // ----- token cursor helpers ------------------------------------------

    /// The current token's kind (or `Eof` if past the end).
    fn peek(&self) -> &TokenKind {
        self.tokens
            .get(self.pos)
            .map(|t| &t.kind)
            .unwrap_or(&TokenKind::Eof)
    }

    /// The kind `n` tokens ahead (or `Eof`).
    fn peek_n(&self, n: usize) -> &TokenKind {
        self.tokens
            .get(self.pos + n)
            .map(|t| &t.kind)
            .unwrap_or(&TokenKind::Eof)
    }

    /// The `##` doc comment attached to the current token, if any (grammar
    /// §1.1). The lexer attaches a doc block to the first real token of the
    /// logical line it precedes, so a declaration's doc lives on its leading
    /// token (the `fn`/`struct`/`enum` keyword, or a field/variant name). Read
    /// this *before* advancing past that token.
    fn cur_doc(&self) -> Option<String> {
        self.tokens.get(self.pos).and_then(|t| t.doc.clone())
    }

    /// The span of the current token (or a dummy span at the end of input,
    /// reusing the last real token's span so errors point somewhere sensible).
    fn cur_span(&self) -> Span {
        if let Some(t) = self.tokens.get(self.pos) {
            t.span
        } else if let Some(t) = self.tokens.last() {
            t.span
        } else {
            Span::dummy()
        }
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    /// Advance past the current token, returning it.
    fn advance(&mut self) -> &'a Token {
        let t = &self.tokens[self.pos.min(self.tokens.len().saturating_sub(1))];
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    /// If the current token matches `kind`, consume it and return true.
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek() == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume a token of the given kind or produce a parse error.
    fn expect(&mut self, kind: &TokenKind, what: &str) -> PResult<&'a Token> {
        if self.peek() == kind {
            Ok(self.advance())
        } else {
            Err(Diagnostic::parse(
                format!("expected {}, found {}", what, describe(self.peek())),
                self.cur_span(),
            ))
        }
    }

    /// Consume a `NAME` token, returning its text and span.
    fn expect_name(&mut self, what: &str) -> PResult<(String, Span)> {
        let span = self.cur_span();
        match self.peek() {
            TokenKind::Name(s) => {
                let s = s.clone();
                self.advance();
                Ok((s, span))
            }
            other => Err(Diagnostic::parse(
                format!("expected {}, found {}", what, describe(other)),
                span,
            )),
        }
    }

    /// Skip any run of `Newline` tokens (blank logical lines between statements).
    fn skip_newlines(&mut self) {
        while matches!(self.peek(), TokenKind::Newline) {
            self.advance();
        }
    }

    // ----- error recovery -------------------------------------------------

    /// After a statement-level error, skip tokens until the next plausible
    /// statement boundary so we can keep parsing and report further errors.
    /// Balances brackets so we don't stop on a `Newline` that the lexer would
    /// have suppressed (defensive — real input rarely needs it).
    fn recover_to_stmt_boundary(&mut self) {
        let mut depth: i32 = 0;
        while !self.at_eof() {
            match self.peek() {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    depth = (depth - 1).max(0)
                }
                TokenKind::Newline if depth == 0 => {
                    self.advance();
                    return;
                }
                TokenKind::Dedent if depth == 0 => return,
                _ => {}
            }
            self.advance();
        }
    }

    // =====================================================================
    // Program & statements (§2, §4)
    // =====================================================================

    fn parse_program(&mut self) -> Program {
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
    fn parse_statement(&mut self) -> PResult<Stmt> {
        match self.peek() {
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Fn => {
                let f = self.parse_fn_decl()?;
                let span = f.span;
                Ok(Stmt { kind: StmtKind::Fn(f), span })
            }
            TokenKind::Struct => self.parse_struct(),
            TokenKind::Enum => self.parse_enum(),
            TokenKind::Impl => self.parse_impl(),
            _ => {
                let stmt = self.parse_simple_stmt()?;
                self.expect_stmt_newline()?;
                Ok(stmt)
            }
        }
    }

    /// Consume the `Newline` that terminates a simple statement. At EOF (or just
    /// before a `Dedent`) the lexer may omit it; tolerate that.
    fn expect_stmt_newline(&mut self) -> PResult<()> {
        match self.peek() {
            TokenKind::Newline => {
                self.advance();
                Ok(())
            }
            TokenKind::Eof | TokenKind::Dedent => Ok(()),
            other => Err(Diagnostic::parse(
                format!("expected end of line, found {}", describe(other)),
                self.cur_span(),
            )),
        }
    }

    /// `simple_stmt = binding | assignment | return | break | continue | expr`.
    fn parse_simple_stmt(&mut self) -> PResult<Stmt> {
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
    fn parse_return(&mut self) -> PResult<Stmt> {
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

    /// `val NAME [":" type] "=" expr`.
    fn parse_val_binding(&mut self) -> PResult<Stmt> {
        let kw = self.cur_span();
        self.advance(); // `val`
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
            kind: StmtKind::Binding(Binding { name, is_val: true, ty, value }),
            span,
        })
    }

    /// Lookahead: does a line starting with `NAME` begin a binding/assignment?
    ///
    /// True iff after the l-value (`NAME { ".NAME" | "[expr]" }`) the next
    /// top-level token is `=`, or it is `NAME ":" type "="` (a typed mutable
    /// binding — only for a *bare* name, since `target` has no type form).
    fn looks_like_binding_or_assign(&self) -> bool {
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
    fn colon_type_then_eq(&self, start: usize) -> bool {
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
    fn skip_balanced(&self, at: usize, open: &TokenKind, close: &TokenKind) -> Option<usize> {
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
    fn parse_binding_or_assign(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        let (name, name_span) = self.expect_name("a name")?;

        // Typed mutable binding: `NAME ":" type "=" expr`.
        if self.eat(&TokenKind::Colon) {
            let ty = self.parse_type()?;
            self.expect(&TokenKind::Eq, "`=` after the binding type")?;
            let value = self.parse_expr()?;
            let span = start.merge(value.span);
            return Ok(Stmt {
                kind: StmtKind::Binding(Binding { name, is_val: false, ty: Some(ty), value }),
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
                kind: StmtKind::Binding(Binding { name, is_val: false, ty: None, value }),
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
    fn parse_target_path(&mut self, base_span: Span) -> PResult<(Vec<TargetSeg>, Span)> {
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
    fn looks_like_self_assign(&self) -> bool {
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
    fn parse_self_assign(&mut self) -> PResult<Stmt> {
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

    // =====================================================================
    // Suites / blocks (§3)
    // =====================================================================

    /// `suite = simple_stmt NEWLINE | NEWLINE INDENT statement+ DEDENT`.
    ///
    /// The caller has already consumed the introducing `:`.
    fn parse_suite(&mut self) -> PResult<Block> {
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
            let start = self.cur_span();
            let stmt = self.parse_simple_stmt()?;
            let span = stmt.span;
            self.expect_stmt_newline()?;
            let _ = start;
            Ok(Block { stmts: vec![stmt], span })
        }
    }

    // =====================================================================
    // Compound statements (§4.2–§4.6)
    // =====================================================================

    /// `if expr ":" suite { elif expr ":" suite } [ else ":" suite ]`.
    fn parse_if(&mut self) -> PResult<Stmt> {
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
    fn parse_while(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        self.advance(); // `while`
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::Colon, "`:` after the `while` condition")?;
        let body = self.parse_suite()?;
        let span = start.merge(body.span);
        Ok(Stmt { kind: StmtKind::While(WhileStmt { cond, body }), span })
    }

    /// `for NAME in expr ":" suite`.
    fn parse_for(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        self.advance(); // `for`
        let (var, _) = self.expect_name("a loop variable name after `for`")?;
        self.expect(&TokenKind::In, "`in` after the `for` variable")?;
        let iter = self.parse_expr()?;
        self.expect(&TokenKind::Colon, "`:` after the `for` iterable")?;
        let body = self.parse_suite()?;
        let span = start.merge(body.span);
        Ok(Stmt { kind: StmtKind::For(ForStmt { var, iter, body }), span })
    }

    /// `fn NAME "(" [param_list] ")" [ "->" type ] ":" suite`.
    ///
    /// The result clause is `-> type` (M2; the M1 `returns` keyword was
    /// dropped). A function with no `->` returns unit, exactly as before.
    fn parse_fn_decl(&mut self) -> PResult<FnDecl> {
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

    /// `param_list = param , …` ; `param = "self" | NAME ":" type`.
    fn parse_params(&mut self) -> PResult<Vec<Param>> {
        let mut params = Vec::new();
        if matches!(self.peek(), TokenKind::RParen) {
            return Ok(params);
        }
        loop {
            match self.peek() {
                TokenKind::SelfKw => {
                    self.advance();
                    params.push(Param::SelfRecv);
                }
                TokenKind::Name(_) => {
                    let (name, _) = self.expect_name("a parameter name")?;
                    self.expect(&TokenKind::Colon, "`:` after the parameter name")?;
                    let ty = self.parse_type()?;
                    // Default values (`= expr`) are M2 Wave 1; until then the
                    // parser always produces `default: None`.
                    // TODO(W1-B): parse an optional `"=" expr` default here.
                    params.push(Param::Named { name, ty, default: None });
                }
                other => {
                    return Err(Diagnostic::parse(
                        format!("expected a parameter, found {}", describe(other)),
                        self.cur_span(),
                    ));
                }
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            // Allow a trailing comma before `)`.
            if matches!(self.peek(), TokenKind::RParen) {
                break;
            }
        }
        Ok(params)
    }

    /// `struct NAME ":" NEWLINE INDENT field_decl+ DEDENT`. Methods are **not**
    /// allowed in a struct body — they are defined in an `impl` block (§4.6), so
    /// there is exactly one way to add a method.
    fn parse_struct(&mut self) -> PResult<Stmt> {
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
    fn parse_enum(&mut self) -> PResult<Stmt> {
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
    fn parse_payload(&mut self) -> PResult<(Payload, Span)> {
        self.expect(&TokenKind::LParen, "`(` to open the payload")?;
        // Decide positional vs. named by lookahead: `NAME ":"` ⇒ named.
        let named = matches!(self.peek(), TokenKind::Name(_))
            && matches!(self.peek_n(1), TokenKind::Colon);

        let payload = if named {
            let mut fields = Vec::new();
            loop {
                let (fname, _) = self.expect_name("a payload field name")?;
                self.expect(&TokenKind::Colon, "`:` after the payload field name")?;
                let ty = self.parse_type()?;
                fields.push((fname, ty));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                if matches!(self.peek(), TokenKind::RParen) {
                    break;
                }
            }
            Payload::Named(fields)
        } else {
            let mut types = Vec::new();
            loop {
                let ty = self.parse_type()?;
                types.push(ty);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                if matches!(self.peek(), TokenKind::RParen) {
                    break;
                }
            }
            Payload::Positional(types)
        };
        let close = self.expect(&TokenKind::RParen, "`)` to close the payload")?;
        Ok((payload, close.span))
    }

    /// `impl NAME ":" NEWLINE INDENT fn_decl+ DEDENT`.
    fn parse_impl(&mut self) -> PResult<Stmt> {
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
    fn skip_newlines_to_indent(&mut self) -> PResult<()> {
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

    // =====================================================================
    // Expressions (§5) — precedence ladder, lowest → highest
    // =====================================================================

    /// `expr = lambda | ternary`.
    fn parse_expr(&mut self) -> PResult<Expr> {
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
    fn try_parse_lambda(&mut self) -> PResult<Option<Expr>> {
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
                let mut params = Vec::new();
                if !matches!(self.peek(), TokenKind::RParen) {
                    loop {
                        let (name, _) = self.expect_name("a lambda parameter")?;
                        params.push(name);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                        if matches!(self.peek(), TokenKind::RParen) {
                            break;
                        }
                    }
                }
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
    fn paren_group_is_lambda_params(&self) -> bool {
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
    fn parse_ternary(&mut self) -> PResult<Expr> {
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
    fn parse_or(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), TokenKind::Or) {
            self.advance();
            let rhs = self.parse_and()?;
            lhs = make_binary(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    /// `and_expr = not_expr { "and" not_expr }` (left-associative).
    fn parse_and(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_not()?;
        while matches!(self.peek(), TokenKind::And) {
            self.advance();
            let rhs = self.parse_not()?;
            lhs = make_binary(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    /// `not_expr = "not" not_expr | comparison`.
    fn parse_not(&mut self) -> PResult<Expr> {
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
    fn parse_comparison(&mut self) -> PResult<Expr> {
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
    fn peek_comp_op(&self) -> Option<BinOp> {
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
    fn consume_comp_op(&mut self) {
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
    fn parse_range(&mut self) -> PResult<Expr> {
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
    fn parse_add(&mut self) -> PResult<Expr> {
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
    fn parse_mul(&mut self) -> PResult<Expr> {
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

    /// `unary = "-" unary | power`.
    ///
    /// `**` binds tighter than unary minus, so `-2 ** 2` = `-(2 ** 2)`: unary
    /// minus wraps the *power* result, and `parse_power` is what consumes the
    /// `**`.
    fn parse_unary(&mut self) -> PResult<Expr> {
        if matches!(self.peek(), TokenKind::Minus) {
            let start = self.cur_span();
            self.advance();
            let operand = self.parse_unary()?;
            let span = start.merge(operand.span);
            Ok(Expr {
                kind: ExprKind::Unary { op: UnOp::Neg, operand: Box::new(operand) },
                span,
            })
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
    fn parse_power(&mut self) -> PResult<Expr> {
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
    fn parse_postfix(&mut self) -> PResult<Expr> {
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
                // `?.` (safe access) is M2 Wave 2 — the token is lexed but the
                // parser does not yet consume it. TODO(W2-B): handle QuestionDot
                // here, producing `Member { safe: true, .. }`.
                _ => break,
            }
        }
        Ok(expr)
    }

    /// `arg = expr | NAME ":" expr` (positional / named).
    fn parse_args(&mut self) -> PResult<Vec<Arg>> {
        let mut args = Vec::new();
        if matches!(self.peek(), TokenKind::RParen) {
            return Ok(args);
        }
        loop {
            // Named arg: `NAME ":" expr`. The `:` after a bare NAME disambiguates
            // from a positional NAME expression.
            if matches!(self.peek(), TokenKind::Name(_))
                && matches!(self.peek_n(1), TokenKind::Colon)
            {
                let (name, _) = self.expect_name("an argument name")?;
                self.advance(); // `:`
                let value = self.parse_expr()?;
                args.push(Arg::Named { name, value });
            } else {
                let value = self.parse_expr()?;
                args.push(Arg::Positional(value));
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            if matches!(self.peek(), TokenKind::RParen) {
                break;
            }
        }
        Ok(args)
    }

    /// `primary = INT | FLOAT | BOOL | NULL | STRING | self | NAME
    ///          | list_literal | "(" expr ")" | match_expr`.
    fn parse_primary(&mut self) -> PResult<Expr> {
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
            TokenKind::LBracket => self.parse_list_literal(),
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse_expr()?;
                let close = self.expect(&TokenKind::RParen, "`)` to close a grouped expression")?;
                // Grouping is transparent; widen the span to include the parens.
                Ok(Expr { kind: inner.kind, span: span.merge(close.span) })
            }
            TokenKind::Match => self.parse_match_expr(),
            other => Err(Diagnostic::parse(
                format!("expected an expression, found {}", describe(&other)),
                span,
            )),
        }
    }

    /// `list_literal = "[" [ expr , … ] "]"`.
    fn parse_list_literal(&mut self) -> PResult<Expr> {
        let start = self.cur_span();
        self.expect(&TokenKind::LBracket, "`[` to open a list literal")?;
        let mut items = Vec::new();
        if !matches!(self.peek(), TokenKind::RBracket) {
            loop {
                items.push(self.parse_expr()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                if matches!(self.peek(), TokenKind::RBracket) {
                    break;
                }
            }
        }
        let close = self.expect(&TokenKind::RBracket, "`]` to close a list literal")?;
        Ok(Expr { kind: ExprKind::List(items), span: start.merge(close.span) })
    }

    /// `match_expr = "match" expr ":" NEWLINE INDENT match_arm+ DEDENT`.
    fn parse_match_expr(&mut self) -> PResult<Expr> {
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

    /// `match_arm = pattern ":" arm_body`.
    /// `arm_body = expr NEWLINE | NEWLINE INDENT statement+ DEDENT` — both stored
    /// as a [`Block`]. An inline expr arm is wrapped as a one-statement block.
    fn parse_match_arm(&mut self) -> PResult<MatchArm> {
        let start = self.cur_span();
        let pattern = self.parse_pattern()?;
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
        // Match guards (`pattern if cond:`) are M2 Wave 2 — the parser produces
        // `guard: None` for now.
        // TODO(W2-A): parse an optional `if cond` guard before the `:`.
        Ok(MatchArm { pattern, guard: None, body, span })
    }

    // =====================================================================
    // Patterns (§5.8) — recursive as of M2
    // =====================================================================

    /// `pattern = "_" | NULL | literal_pattern | NAME | variant_pattern`.
    ///
    /// Or-patterns (`p1 or p2`) and tuple patterns (`(p1, p2)`) are M2 Wave 2
    /// and are not parsed here yet, though the AST ([`PatternKind::Or`] /
    /// [`PatternKind::Tuple`]) can represent them. Variant sub-patterns are
    /// recursive, so nested destructuring already parses.
    // TODO(W2-A): parse or-patterns and tuple patterns here.
    fn parse_pattern(&mut self) -> PResult<Pattern> {
        let span = self.cur_span();
        match self.peek().clone() {
            TokenKind::Null => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Null, span })
            }
            TokenKind::Int(n) => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LitPattern::Int(n)), span })
            }
            TokenKind::Float(f) => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LitPattern::Float(f)), span })
            }
            TokenKind::True => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LitPattern::Bool(true)), span })
            }
            TokenKind::False => {
                self.advance();
                Ok(Pattern { kind: PatternKind::Literal(LitPattern::Bool(false)), span })
            }
            TokenKind::Str(parts) => {
                self.advance();
                let s = string_parts_as_literal(&parts).ok_or_else(|| {
                    Diagnostic::parse(
                        "a string pattern may not contain interpolation",
                        span,
                    )
                })?;
                Ok(Pattern { kind: PatternKind::Literal(LitPattern::Str(s)), span })
            }
            // Leading-dot variant pattern `.Variant[(subs)]` — enum inferred
            // from the scrutinee.
            TokenKind::Dot => {
                self.advance(); // `.`
                let (variant, vspan) = self.expect_name("a variant name after `.`")?;
                let (subs, end) = self.parse_variant_subs(vspan)?;
                Ok(Pattern {
                    kind: PatternKind::Variant { enum_name: None, name: variant, subs },
                    span: span.merge(end),
                })
            }
            TokenKind::Name(name) => {
                // `_` is a NAME-shaped token; treat it as the wildcard.
                if name == "_" {
                    self.advance();
                    return Ok(Pattern { kind: PatternKind::Wildcard, span });
                }
                self.advance();
                if matches!(self.peek(), TokenKind::Dot) {
                    // Qualified variant pattern `Enum.Variant[(subs)]`.
                    self.advance(); // `.`
                    let (variant, vspan) = self.expect_name("a variant name after `.`")?;
                    let (subs, end) = self.parse_variant_subs(vspan)?;
                    Ok(Pattern {
                        kind: PatternKind::Variant {
                            enum_name: Some(name),
                            name: variant,
                            subs,
                        },
                        span: span.merge(end),
                    })
                } else if matches!(self.peek(), TokenKind::LParen) {
                    // A bare `NAME(...)` is no longer a variant pattern — variants
                    // are qualified.
                    Err(Diagnostic::parse(
                        format!(
                            "variant patterns name their enum: write `.{name}(...)` or `Enum.{name}(...)`"
                        ),
                        span,
                    ))
                } else {
                    // Bare binding name.
                    Ok(Pattern { kind: PatternKind::Binding(name), span })
                }
            }
            other => Err(Diagnostic::parse(
                format!("expected a pattern, found {}", describe(&other)),
                span,
            )),
        }
    }

    /// Parse the optional `"(" pattern , … ")"` payload of a variant pattern.
    /// A niladic variant (no parens) yields an empty `subs` list. Returns the
    /// sub-patterns and the span of the variant's last token.
    ///
    /// As of M2 the sub-patterns are full [`Pattern`]s (recursive), so a
    /// variant's payload may itself contain variant/tuple/or patterns (nested
    /// destructuring). The flat M1 cases (`_`, a name, `null`, a literal) are
    /// produced unchanged because [`Self::parse_pattern`] yields the same base
    /// kinds for them.
    fn parse_variant_subs(&mut self, name_span: Span) -> PResult<(Vec<Pattern>, Span)> {
        if !matches!(self.peek(), TokenKind::LParen) {
            return Ok((Vec::new(), name_span));
        }
        self.advance(); // `(`
        let mut subs = Vec::new();
        if !matches!(self.peek(), TokenKind::RParen) {
            loop {
                subs.push(self.parse_pattern()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                if matches!(self.peek(), TokenKind::RParen) {
                    break;
                }
            }
        }
        let close = self.expect(&TokenKind::RParen, "`)` to close a variant pattern")?;
        Ok((subs, close.span))
    }

    // =====================================================================
    // Types (§6)
    // =====================================================================

    /// `type = base_type [ "?" ]`.
    fn parse_type(&mut self) -> PResult<Type> {
        let start = self.cur_span();
        let base = match self.peek().clone() {
            TokenKind::LParen => {
                // Unit type `()`.
                self.advance();
                self.expect(&TokenKind::RParen, "`)` to close the unit type `()`")?;
                BaseType::Unit
            }
            TokenKind::Name(name) => {
                self.advance();
                let mut args = Vec::new();
                if matches!(self.peek(), TokenKind::LBracket) {
                    self.advance();
                    loop {
                        args.push(self.parse_type()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                        if matches!(self.peek(), TokenKind::RBracket) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RBracket, "`]` to close type arguments")?;
                }
                BaseType::Named { name, args }
            }
            other => {
                return Err(Diagnostic::parse(
                    format!("expected a type, found {}", describe(&other)),
                    start,
                ));
            }
        };

        let mut end = self.prev_span_or(start);
        let nullable = if matches!(self.peek(), TokenKind::Question) {
            end = self.cur_span();
            self.advance();
            true
        } else {
            end = self.prev_span_or(end);
            false
        };
        Ok(Type { base, nullable, span: start.merge(end) })
    }

    /// The span of the previously consumed token, or `fallback` if at the start.
    fn prev_span_or(&self, fallback: Span) -> Span {
        if self.pos == 0 {
            fallback
        } else {
            self.tokens
                .get(self.pos - 1)
                .map(|t| t.span)
                .unwrap_or(fallback)
        }
    }

    // =====================================================================
    // String interpolation sub-parsing (§1.5)
    // =====================================================================

    /// Build a [`StringLit`] from the lexer's `StrPart`s, recursively parsing
    /// each interpolation's nested token stream into an [`Expr`].
    fn build_string_lit(&mut self, parts: &[StrPart]) -> PResult<StringLit> {
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
    fn parse_interpolation(&mut self, tokens: &[Token]) -> PResult<Expr> {
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

// ===========================================================================
// Free helpers
// ===========================================================================

/// Build a `Binary` expression node, merging operand spans.
fn make_binary(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    let span = lhs.span.merge(rhs.span);
    Expr {
        kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
        span,
    }
}

/// Compute a block's span from its delimiters and statements.
fn block_span(start: Span, end: Span, stmts: &[Stmt]) -> Span {
    let mut span = start.merge(end);
    for s in stmts {
        span = span.merge(s.span);
    }
    span
}

/// If a `Str` token's parts are pure text (no interpolation), return the
/// concatenated string; otherwise `None`. Used for string *patterns*, which may
/// not interpolate (§5.8).
fn string_parts_as_literal(parts: &[StrPart]) -> Option<String> {
    let mut out = String::new();
    for part in parts {
        match part {
            StrPart::Text(t) => out.push_str(t),
            StrPart::Interp(_) => return None,
        }
    }
    Some(out)
}

/// A short human description of a token kind for error messages.
fn describe(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Name(s) => format!("name `{s}`"),
        TokenKind::Int(_) => "an integer".into(),
        TokenKind::Float(_) => "a float".into(),
        TokenKind::Str(_) => "a string".into(),
        TokenKind::Fn => "`fn`".into(),
        TokenKind::Val => "`val`".into(),
        TokenKind::Struct => "`struct`".into(),
        TokenKind::Enum => "`enum`".into(),
        TokenKind::Impl => "`impl`".into(),
        TokenKind::Return => "`return`".into(),
        TokenKind::If => "`if`".into(),
        TokenKind::Elif => "`elif`".into(),
        TokenKind::Else => "`else`".into(),
        TokenKind::Match => "`match`".into(),
        TokenKind::While => "`while`".into(),
        TokenKind::For => "`for`".into(),
        TokenKind::In => "`in`".into(),
        TokenKind::Break => "`break`".into(),
        TokenKind::Continue => "`continue`".into(),
        TokenKind::And => "`and`".into(),
        TokenKind::Or => "`or`".into(),
        TokenKind::Not => "`not`".into(),
        TokenKind::Is => "`is`".into(),
        TokenKind::True => "`true`".into(),
        TokenKind::False => "`false`".into(),
        TokenKind::Null => "`null`".into(),
        TokenKind::SelfKw => "`self`".into(),
        TokenKind::EqEq => "`==`".into(),
        TokenKind::NotEq => "`!=`".into(),
        TokenKind::Lt => "`<`".into(),
        TokenKind::LtEq => "`<=`".into(),
        TokenKind::Gt => "`>`".into(),
        TokenKind::GtEq => "`>=`".into(),
        TokenKind::Plus => "`+`".into(),
        TokenKind::Minus => "`-`".into(),
        TokenKind::Star => "`*`".into(),
        TokenKind::Slash => "`/`".into(),
        TokenKind::Percent => "`%`".into(),
        TokenKind::StarStar => "`**`".into(),
        TokenKind::Eq => "`=`".into(),
        TokenKind::Arrow => "`->`".into(),
        TokenKind::DotDot => "`..`".into(),
        TokenKind::DotDotEq => "`..=`".into(),
        TokenKind::Colon => "`:`".into(),
        TokenKind::Comma => "`,`".into(),
        TokenKind::Dot => "`.`".into(),
        TokenKind::LParen => "`(`".into(),
        TokenKind::RParen => "`)`".into(),
        TokenKind::LBracket => "`[`".into(),
        TokenKind::RBracket => "`]`".into(),
        TokenKind::LBrace => "`{`".into(),
        TokenKind::RBrace => "`}`".into(),
        TokenKind::Question => "`?`".into(),
        TokenKind::QuestionDot => "`?.`".into(),
        TokenKind::Newline => "end of line".into(),
        TokenKind::Indent => "an indent".into(),
        TokenKind::Dedent => "a dedent".into(),
        TokenKind::Eof => "end of input".into(),
    }
}

// ===========================================================================
// Tests
// ===========================================================================
//
// The lexer is being built in parallel, so these tests do NOT call
// `adder::lexer::lex`. Instead they hand-build `Vec<Token>` inputs with dummy
// spans and assert the resulting AST.

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;

    /// Construct a token with a dummy span.
    fn t(kind: TokenKind) -> Token {
        Token::new(kind, Span::dummy())
    }

    fn name(s: &str) -> Token {
        t(TokenKind::Name(s.to_string()))
    }

    fn int(n: i64) -> Token {
        t(TokenKind::Int(BigInt::from(n)))
    }

    fn nl() -> Token {
        t(TokenKind::Newline)
    }

    fn eof() -> Token {
        t(TokenKind::Eof)
    }

    /// Parse a program from a token list, asserting success and returning it.
    fn parse_ok(tokens: Vec<Token>) -> Program {
        match parse(&tokens) {
            Ok(p) => p,
            Err(e) => panic!("expected parse success, got errors: {:?}", e),
        }
    }

    /// Parse, expecting failure; return the diagnostics.
    fn parse_err(tokens: Vec<Token>) -> Vec<Diagnostic> {
        match parse(&tokens) {
            Ok(p) => panic!("expected parse error, got program: {:?}", p),
            Err(e) => e,
        }
    }

    /// Helper: pull the single statement out of a one-statement program.
    fn only_stmt(p: &Program) -> &Stmt {
        assert_eq!(p.stmts.len(), 1, "expected exactly one statement: {:?}", p.stmts);
        &p.stmts[0]
    }

    /// Parse a list of tokens (without trailing Eof) as a single expression by
    /// wrapping it as an expr-statement.
    fn parse_expr_tokens(mut body: Vec<Token>) -> Expr {
        body.push(nl());
        body.push(eof());
        let p = parse_ok(body);
        match &only_stmt(&p).kind {
            StmtKind::Expr(e) => e.clone(),
            other => panic!("expected an expr statement, got {:?}", other),
        }
    }

    // ----- bindings (three forms) ----------------------------------------

    #[test]
    fn binding_val_inferred() {
        // val x = 1
        let toks = vec![t(TokenKind::Val), name("x"), t(TokenKind::Eq), int(1), nl(), eof()];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Binding(b) => {
                assert_eq!(b.name, "x");
                assert!(b.is_val);
                assert!(b.ty.is_none());
                assert_eq!(b.value.kind, ExprKind::Int(BigInt::from(1)));
            }
            other => panic!("expected binding, got {:?}", other),
        }
    }

    #[test]
    fn binding_val_typed() {
        // val x: Int = 1
        let toks = vec![
            t(TokenKind::Val),
            name("x"),
            t(TokenKind::Colon),
            name("Int"),
            t(TokenKind::Eq),
            int(1),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Binding(b) => {
                assert!(b.is_val);
                let ty = b.ty.as_ref().expect("typed");
                assert!(matches!(&ty.base, BaseType::Named { name, args } if name == "Int" && args.is_empty()));
                assert!(!ty.nullable);
            }
            other => panic!("expected binding, got {:?}", other),
        }
    }

    #[test]
    fn binding_typed_mutable() {
        // x: Int = 1
        let toks = vec![
            name("x"),
            t(TokenKind::Colon),
            name("Int"),
            t(TokenKind::Eq),
            int(1),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Binding(b) => {
                assert!(!b.is_val);
                assert!(b.ty.is_some());
                assert_eq!(b.name, "x");
            }
            other => panic!("expected binding, got {:?}", other),
        }
    }

    #[test]
    fn binding_inferred_mutable() {
        // count = 0
        let toks = vec![name("count"), t(TokenKind::Eq), int(0), nl(), eof()];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Binding(b) => {
                assert!(!b.is_val);
                assert!(b.ty.is_none());
                assert_eq!(b.name, "count");
            }
            other => panic!("expected binding, got {:?}", other),
        }
    }

    // ----- assignment vs expr-stmt disambiguation ------------------------

    #[test]
    fn assign_to_field_target() {
        // p.x = 5
        let toks = vec![
            name("p"),
            t(TokenKind::Dot),
            name("x"),
            t(TokenKind::Eq),
            int(5),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Assign(a) => {
                assert_eq!(a.target.base, "p");
                assert_eq!(a.target.path.len(), 1);
                assert!(matches!(&a.target.path[0], TargetSeg::Field(f) if f == "x"));
            }
            other => panic!("expected assign, got {:?}", other),
        }
    }

    #[test]
    fn assign_to_index_target() {
        // xs[0] = 9
        let toks = vec![
            name("xs"),
            t(TokenKind::LBracket),
            int(0),
            t(TokenKind::RBracket),
            t(TokenKind::Eq),
            int(9),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Assign(a) => {
                assert_eq!(a.target.base, "xs");
                assert!(matches!(&a.target.path[0], TargetSeg::Index(_)));
            }
            other => panic!("expected assign, got {:?}", other),
        }
    }

    #[test]
    fn name_call_is_expr_stmt_not_assign() {
        // print(x)  -- a NAME line with no top-level `=` is an expr statement
        let toks = vec![
            name("print"),
            t(TokenKind::LParen),
            name("x"),
            t(TokenKind::RParen),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Expr(e) => assert!(matches!(e.kind, ExprKind::Call { .. })),
            other => panic!("expected expr statement, got {:?}", other),
        }
    }

    #[test]
    fn member_chain_is_expr_stmt() {
        // a.b.c  -- no `=`, so an expression statement (member access)
        let toks = vec![
            name("a"),
            t(TokenKind::Dot),
            name("b"),
            t(TokenKind::Dot),
            name("c"),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        assert!(matches!(&only_stmt(&p).kind, StmtKind::Expr(_)));
    }

    // ----- if / elif / else ----------------------------------------------

    #[test]
    fn if_elif_else() {
        // if a: x()
        // elif b: y()
        // else: z()
        let call = |n: &str| {
            vec![name(n), t(TokenKind::LParen), t(TokenKind::RParen)]
        };
        let mut toks = vec![t(TokenKind::If), name("a"), t(TokenKind::Colon)];
        toks.extend(call("x"));
        toks.push(nl());
        toks.push(t(TokenKind::Elif));
        toks.push(name("b"));
        toks.push(t(TokenKind::Colon));
        toks.extend(call("y"));
        toks.push(nl());
        toks.push(t(TokenKind::Else));
        toks.push(t(TokenKind::Colon));
        toks.extend(call("z"));
        toks.push(nl());
        toks.push(eof());

        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::If(i) => {
                assert_eq!(i.arms.len(), 2);
                assert!(i.else_body.is_some());
            }
            other => panic!("expected if, got {:?}", other),
        }
    }

    // ----- for x in 0..n -------------------------------------------------

    #[test]
    fn for_range() {
        // for x in 0..n: f()
        let toks = vec![
            t(TokenKind::For),
            name("x"),
            t(TokenKind::In),
            int(0),
            t(TokenKind::DotDot),
            name("n"),
            t(TokenKind::Colon),
            name("f"),
            t(TokenKind::LParen),
            t(TokenKind::RParen),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::For(f) => {
                assert_eq!(f.var, "x");
                match &f.iter.kind {
                    ExprKind::Binary { op, .. } => assert_eq!(*op, BinOp::Range),
                    other => panic!("expected range, got {:?}", other),
                }
                assert_eq!(f.body.stmts.len(), 1);
            }
            other => panic!("expected for, got {:?}", other),
        }
    }

    // ----- fn with params + result type (`->`) ---------------------------

    #[test]
    fn fn_with_params_and_returns() {
        // fn add(a: Int, b: Int) -> Int:
        //     return a
        let toks = vec![
            t(TokenKind::Fn),
            name("add"),
            t(TokenKind::LParen),
            name("a"),
            t(TokenKind::Colon),
            name("Int"),
            t(TokenKind::Comma),
            name("b"),
            t(TokenKind::Colon),
            name("Int"),
            t(TokenKind::RParen),
            t(TokenKind::Arrow),
            name("Int"),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            t(TokenKind::Return),
            name("a"),
            nl(),
            t(TokenKind::Dedent),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => {
                assert_eq!(f.name, "add");
                assert_eq!(f.params.len(), 2);
                assert!(matches!(&f.params[0], Param::Named { name, .. } if name == "a"));
                assert!(f.returns.is_some());
                assert_eq!(f.body.stmts.len(), 1);
                assert!(matches!(&f.body.stmts[0].kind, StmtKind::Return(Some(_))));
            }
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_with_self_param() {
        // fn m(self): self
        let toks = vec![
            t(TokenKind::Fn),
            name("m"),
            t(TokenKind::LParen),
            t(TokenKind::SelfKw),
            t(TokenKind::RParen),
            t(TokenKind::Colon),
            t(TokenKind::SelfKw),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => {
                assert_eq!(f.params.len(), 1);
                assert!(matches!(&f.params[0], Param::SelfRecv));
            }
            other => panic!("expected fn, got {:?}", other),
        }
    }

    // ----- enum with positional & named variants -------------------------

    #[test]
    fn enum_positional_and_named() {
        // enum Shape:
        //     Add(Expr, Expr)
        //     Circle(radius: Float)
        //     Empty
        let toks = vec![
            t(TokenKind::Enum),
            name("Shape"),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            // Add(Expr, Expr)
            name("Add"),
            t(TokenKind::LParen),
            name("Expr"),
            t(TokenKind::Comma),
            name("Expr"),
            t(TokenKind::RParen),
            nl(),
            // Circle(radius: Float)
            name("Circle"),
            t(TokenKind::LParen),
            name("radius"),
            t(TokenKind::Colon),
            name("Float"),
            t(TokenKind::RParen),
            nl(),
            // Empty
            name("Empty"),
            nl(),
            t(TokenKind::Dedent),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Enum(e) => {
                assert_eq!(e.variants.len(), 3);
                assert!(matches!(
                    &e.variants[0].payload,
                    Some(Payload::Positional(v)) if v.len() == 2
                ));
                match &e.variants[1].payload {
                    Some(Payload::Named(fields)) => {
                        assert_eq!(fields.len(), 1);
                        assert_eq!(fields[0].0, "radius");
                    }
                    other => panic!("expected named payload, got {:?}", other),
                }
                assert!(e.variants[2].payload.is_none());
            }
            other => panic!("expected enum, got {:?}", other),
        }
    }

    // ----- struct fields; methods are impl-only --------------------------

    #[test]
    fn struct_with_fields() {
        // struct Point:
        //     x: Float
        let toks = vec![
            t(TokenKind::Struct),
            name("Point"),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            name("x"),
            t(TokenKind::Colon),
            name("Float"),
            nl(),
            t(TokenKind::Dedent),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Struct(s) => {
                assert_eq!(s.name, "Point");
                assert_eq!(s.fields.len(), 1);
                assert_eq!(s.fields[0].name, "x");
            }
            other => panic!("expected struct, got {:?}", other),
        }
    }

    #[test]
    fn struct_body_method_is_rejected() {
        // struct Point:
        //     fn norm(self): self
        // -> methods belong in an `impl` block, not the struct body.
        let toks = vec![
            t(TokenKind::Struct),
            name("Point"),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            t(TokenKind::Fn),
            name("norm"),
            t(TokenKind::LParen),
            t(TokenKind::SelfKw),
            t(TokenKind::RParen),
            t(TokenKind::Colon),
            t(TokenKind::SelfKw),
            nl(),
            t(TokenKind::Dedent),
            eof(),
        ];
        let errs = parse_err(toks);
        assert!(
            errs.iter().any(|d| d.message.contains("impl")),
            "error should point methods at `impl`; got {errs:?}"
        );
    }

    #[test]
    fn impl_block() {
        // impl Point:
        //     fn zero(): 0
        let toks = vec![
            t(TokenKind::Impl),
            name("Point"),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            t(TokenKind::Fn),
            name("zero"),
            t(TokenKind::LParen),
            t(TokenKind::RParen),
            t(TokenKind::Colon),
            int(0),
            nl(),
            t(TokenKind::Dedent),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Impl(i) => {
                assert_eq!(i.type_name, "Point");
                assert_eq!(i.methods.len(), 1);
            }
            other => panic!("expected impl, got {:?}", other),
        }
    }

    // ----- precedence ----------------------------------------------------

    #[test]
    fn neg_pow_binds_tighter_than_unary_minus() {
        // -2 ** 2  ==  -(2 ** 2)
        let e = parse_expr_tokens(vec![
            t(TokenKind::Minus),
            int(2),
            t(TokenKind::StarStar),
            int(2),
        ]);
        match &e.kind {
            ExprKind::Unary { op: UnOp::Neg, operand } => match &operand.kind {
                ExprKind::Binary { op: BinOp::Pow, .. } => {}
                other => panic!("expected `-(2**2)`, inner was {:?}", other),
            },
            other => panic!("expected unary neg at top, got {:?}", other),
        }
    }

    #[test]
    fn pow_is_right_associative() {
        // 2 ** 3 ** 2  ==  2 ** (3 ** 2)
        let e = parse_expr_tokens(vec![
            int(2),
            t(TokenKind::StarStar),
            int(3),
            t(TokenKind::StarStar),
            int(2),
        ]);
        match &e.kind {
            ExprKind::Binary { op: BinOp::Pow, lhs, rhs } => {
                assert_eq!(lhs.kind, ExprKind::Int(BigInt::from(2)));
                match &rhs.kind {
                    ExprKind::Binary { op: BinOp::Pow, .. } => {}
                    other => panic!("expected right-nested pow, got {:?}", other),
                }
            }
            other => panic!("expected pow at top, got {:?}", other),
        }
    }

    #[test]
    fn mul_binds_tighter_than_add() {
        // a + b * c  ==  a + (b * c)
        let e = parse_expr_tokens(vec![
            name("a"),
            t(TokenKind::Plus),
            name("b"),
            t(TokenKind::Star),
            name("c"),
        ]);
        match &e.kind {
            ExprKind::Binary { op: BinOp::Add, lhs, rhs } => {
                assert_eq!(lhs.kind, ExprKind::Name("a".into()));
                assert!(matches!(&rhs.kind, ExprKind::Binary { op: BinOp::Mul, .. }));
            }
            other => panic!("expected add at top, got {:?}", other),
        }
    }

    // ----- ternary -------------------------------------------------------

    #[test]
    fn ternary() {
        // a if c else b
        let e = parse_expr_tokens(vec![
            name("a"),
            t(TokenKind::If),
            name("c"),
            t(TokenKind::Else),
            name("b"),
        ]);
        match &e.kind {
            ExprKind::Ternary { then, cond, otherwise } => {
                assert_eq!(then.kind, ExprKind::Name("a".into()));
                assert_eq!(cond.kind, ExprKind::Name("c".into()));
                assert_eq!(otherwise.kind, ExprKind::Name("b".into()));
            }
            other => panic!("expected ternary, got {:?}", other),
        }
    }

    // ----- lambdas -------------------------------------------------------

    #[test]
    fn lambda_one_arg() {
        // x -> x
        let e = parse_expr_tokens(vec![name("x"), t(TokenKind::Arrow), name("x")]);
        match &e.kind {
            ExprKind::Lambda(l) => {
                assert_eq!(l.params, vec!["x".to_string()]);
                assert_eq!(l.body.kind, ExprKind::Name("x".into()));
            }
            other => panic!("expected lambda, got {:?}", other),
        }
    }

    #[test]
    fn lambda_two_args() {
        // (a, b) -> a
        let e = parse_expr_tokens(vec![
            t(TokenKind::LParen),
            name("a"),
            t(TokenKind::Comma),
            name("b"),
            t(TokenKind::RParen),
            t(TokenKind::Arrow),
            name("a"),
        ]);
        match &e.kind {
            ExprKind::Lambda(l) => {
                assert_eq!(l.params, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("expected lambda, got {:?}", other),
        }
    }

    #[test]
    fn parenthesized_expr_is_not_lambda() {
        // (a + b)  -- a grouped expression, NOT a lambda
        let e = parse_expr_tokens(vec![
            t(TokenKind::LParen),
            name("a"),
            t(TokenKind::Plus),
            name("b"),
            t(TokenKind::RParen),
        ]);
        assert!(matches!(&e.kind, ExprKind::Binary { op: BinOp::Add, .. }));
    }

    // ----- comparison: non-chaining --------------------------------------

    #[test]
    fn comparison_single_ok() {
        // a < b
        let e = parse_expr_tokens(vec![name("a"), t(TokenKind::Lt), name("b")]);
        assert!(matches!(&e.kind, ExprKind::Binary { op: BinOp::Lt, .. }));
    }

    #[test]
    fn comparison_chain_rejected() {
        // a < b < c  -- syntax error
        let toks = vec![
            name("a"),
            t(TokenKind::Lt),
            name("b"),
            t(TokenKind::Lt),
            name("c"),
            nl(),
            eof(),
        ];
        let errs = parse_err(toks);
        assert!(
            errs.iter().any(|d| d.message.contains("do not chain")),
            "expected a non-chaining error, got {:?}",
            errs
        );
    }

    #[test]
    fn is_not_operator() {
        // a is not b
        let e = parse_expr_tokens(vec![
            name("a"),
            t(TokenKind::Is),
            t(TokenKind::Not),
            name("b"),
        ]);
        assert!(matches!(&e.kind, ExprKind::Binary { op: BinOp::IsNot, .. }));
    }

    #[test]
    fn is_operator() {
        // a is b
        let e = parse_expr_tokens(vec![name("a"), t(TokenKind::Is), name("b")]);
        assert!(matches!(&e.kind, ExprKind::Binary { op: BinOp::Is, .. }));
    }

    // ----- match ---------------------------------------------------------

    #[test]
    fn match_with_inline_and_block_arms_and_variant() {
        // match e:
        //     Num(n): n
        //     Add(a, b):
        //         x = a
        //         x
        //     _: 0
        let toks = vec![
            t(TokenKind::Match),
            name("e"),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            // .Num(n): n
            t(TokenKind::Dot),
            name("Num"),
            t(TokenKind::LParen),
            name("n"),
            t(TokenKind::RParen),
            t(TokenKind::Colon),
            name("n"),
            nl(),
            // .Add(a, b): block
            t(TokenKind::Dot),
            name("Add"),
            t(TokenKind::LParen),
            name("a"),
            t(TokenKind::Comma),
            name("b"),
            t(TokenKind::RParen),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            name("x"),
            t(TokenKind::Eq),
            name("a"),
            nl(),
            name("x"),
            nl(),
            t(TokenKind::Dedent),
            // _: 0
            name("_"),
            t(TokenKind::Colon),
            int(0),
            nl(),
            t(TokenKind::Dedent),
            eof(),
        ];
        // Wrap as an expr statement: `match e: ...` standing alone.
        let p = parse_ok(toks);
        let e = match &only_stmt(&p).kind {
            StmtKind::Expr(e) => e,
            other => panic!("expected expr stmt, got {:?}", other),
        };
        match &e.kind {
            ExprKind::Match(m) => {
                assert_eq!(m.arms.len(), 3);
                // arm 0: variant Num(n), inline body (1 stmt)
                assert!(matches!(
                    &m.arms[0].pattern.kind,
                    PatternKind::Variant { enum_name, name, subs }
                        if enum_name.is_none() && name == "Num" && subs.len() == 1
                ));
                assert_eq!(m.arms[0].body.stmts.len(), 1);
                // arm 1: variant .Add(a, b), block body (2 stmts)
                assert!(matches!(
                    &m.arms[1].pattern.kind,
                    PatternKind::Variant { enum_name, name, subs }
                        if enum_name.is_none() && name == "Add" && subs.len() == 2
                ));
                assert_eq!(m.arms[1].body.stmts.len(), 2);
                // arm 2: wildcard
                assert!(matches!(&m.arms[2].pattern.kind, PatternKind::Wildcard));
            }
            other => panic!("expected match, got {:?}", other),
        }
    }

    #[test]
    fn match_as_return_value() {
        // return match e:
        //     _: 0
        let toks = vec![
            t(TokenKind::Return),
            t(TokenKind::Match),
            name("e"),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            name("_"),
            t(TokenKind::Colon),
            int(0),
            nl(),
            t(TokenKind::Dedent),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Return(Some(e)) => assert!(matches!(e.kind, ExprKind::Match(_))),
            other => panic!("expected return match, got {:?}", other),
        }
    }

    #[test]
    fn variant_pattern_with_null_and_literal_subs() {
        // match e:
        //     E.V(_, null, 3): 1
        let toks = vec![
            t(TokenKind::Match),
            name("e"),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            name("E"),
            t(TokenKind::Dot),
            name("V"),
            t(TokenKind::LParen),
            name("_"),
            t(TokenKind::Comma),
            t(TokenKind::Null),
            t(TokenKind::Comma),
            int(3),
            t(TokenKind::RParen),
            t(TokenKind::Colon),
            int(1),
            nl(),
            t(TokenKind::Dedent),
            eof(),
        ];
        let p = parse_ok(toks);
        let e = match &only_stmt(&p).kind {
            StmtKind::Expr(e) => e,
            other => panic!("expected expr stmt, got {:?}", other),
        };
        match &e.kind {
            ExprKind::Match(m) => match &m.arms[0].pattern.kind {
                PatternKind::Variant { enum_name, name, subs } => {
                    assert_eq!(enum_name.as_deref(), Some("E"));
                    assert_eq!(name, "V");
                    assert_eq!(subs.len(), 3);
                    assert!(matches!(subs[0].kind, PatternKind::Wildcard));
                    assert!(matches!(subs[1].kind, PatternKind::Null));
                    assert!(matches!(&subs[2].kind, PatternKind::Literal(LitPattern::Int(_))));
                }
                other => panic!("expected variant pattern, got {:?}", other),
            },
            other => panic!("expected match, got {:?}", other),
        }
    }

    // ----- string interpolation ------------------------------------------

    #[test]
    fn string_with_interpolation() {
        // "x = {a + 1}"
        // Build the inner interpolation token stream: a + 1  (ending in Eof).
        let inner = vec![
            name("a"),
            t(TokenKind::Plus),
            int(1),
            eof(),
        ];
        let str_token = t(TokenKind::Str(vec![
            StrPart::Text("x = ".to_string()),
            StrPart::Interp(inner),
        ]));
        let e = parse_expr_tokens(vec![str_token]);
        match &e.kind {
            ExprKind::Str(lit) => {
                assert_eq!(lit.parts.len(), 2);
                assert!(matches!(&lit.parts[0], StrSeg::Text(t) if t == "x = "));
                match &lit.parts[1] {
                    StrSeg::Expr(inner) => {
                        assert!(matches!(
                            &inner.kind,
                            ExprKind::Binary { op: BinOp::Add, .. }
                        ));
                    }
                    other => panic!("expected interpolation expr, got {:?}", other),
                }
            }
            other => panic!("expected string literal, got {:?}", other),
        }
    }

    // ----- types ---------------------------------------------------------

    #[test]
    fn type_generic_nullable() {
        // val xs: List[Int]? = e   -- exercises base_type generic args + `?`
        let toks = vec![
            t(TokenKind::Val),
            name("xs"),
            t(TokenKind::Colon),
            name("List"),
            t(TokenKind::LBracket),
            name("Int"),
            t(TokenKind::RBracket),
            t(TokenKind::Question),
            t(TokenKind::Eq),
            name("e"),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Binding(b) => {
                let ty = b.ty.as_ref().expect("typed");
                assert!(ty.nullable);
                match &ty.base {
                    BaseType::Named { name, args } => {
                        assert_eq!(name, "List");
                        assert_eq!(args.len(), 1);
                        assert!(matches!(&args[0].base, BaseType::Named { name, .. } if name == "Int"));
                    }
                    other => panic!("expected named type, got {:?}", other),
                }
            }
            other => panic!("expected binding, got {:?}", other),
        }
    }

    #[test]
    fn unit_type() {
        // fn f() -> (): 0   -- the `()` unit type after `->`
        let toks = vec![
            t(TokenKind::Fn),
            name("f"),
            t(TokenKind::LParen),
            t(TokenKind::RParen),
            t(TokenKind::Arrow),
            t(TokenKind::LParen),
            t(TokenKind::RParen),
            t(TokenKind::Colon),
            int(0),
            nl(),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => {
                let ty = f.returns.as_ref().expect("returns");
                assert!(matches!(ty.base, BaseType::Unit));
            }
            other => panic!("expected fn, got {:?}", other),
        }
    }

    // ----- postfix call/index/member chaining ----------------------------

    #[test]
    fn postfix_chain() {
        // a.b(c)[d]  -- member, call, index nested left-to-right
        let e = parse_expr_tokens(vec![
            name("a"),
            t(TokenKind::Dot),
            name("b"),
            t(TokenKind::LParen),
            name("c"),
            t(TokenKind::RParen),
            t(TokenKind::LBracket),
            name("d"),
            t(TokenKind::RBracket),
        ]);
        // Outermost is Index, inside Call, inside Member.
        match &e.kind {
            ExprKind::Index { base, .. } => match &base.kind {
                ExprKind::Call { callee, .. } => {
                    assert!(matches!(&callee.kind, ExprKind::Member { .. }));
                }
                other => panic!("expected call inside index, got {:?}", other),
            },
            other => panic!("expected index at top, got {:?}", other),
        }
    }

    #[test]
    fn named_arg_in_call() {
        // Point(x: 1, y: 2)
        let e = parse_expr_tokens(vec![
            name("Point"),
            t(TokenKind::LParen),
            name("x"),
            t(TokenKind::Colon),
            int(1),
            t(TokenKind::Comma),
            name("y"),
            t(TokenKind::Colon),
            int(2),
            t(TokenKind::RParen),
        ]);
        match &e.kind {
            ExprKind::Call { args, .. } => {
                assert_eq!(args.len(), 2);
                assert!(matches!(&args[0], Arg::Named { name, .. } if name == "x"));
            }
            other => panic!("expected call, got {:?}", other),
        }
    }

    // ----- break / continue / return-none --------------------------------

    #[test]
    fn break_continue_return_none() {
        // while c:
        //     break
        //     continue
        //     return
        let toks = vec![
            t(TokenKind::While),
            name("c"),
            t(TokenKind::Colon),
            nl(),
            t(TokenKind::Indent),
            t(TokenKind::Break),
            nl(),
            t(TokenKind::Continue),
            nl(),
            t(TokenKind::Return),
            nl(),
            t(TokenKind::Dedent),
            eof(),
        ];
        let p = parse_ok(toks);
        match &only_stmt(&p).kind {
            StmtKind::While(w) => {
                assert_eq!(w.body.stmts.len(), 3);
                assert!(matches!(w.body.stmts[0].kind, StmtKind::Break));
                assert!(matches!(w.body.stmts[1].kind, StmtKind::Continue));
                assert!(matches!(w.body.stmts[2].kind, StmtKind::Return(None)));
            }
            other => panic!("expected while, got {:?}", other),
        }
    }

    // ----- `##` doc-comment attachment (§1.1) ----------------------------
    //
    // These go through the real lexer (the doc metadata originates there) and
    // assert it lands on the right AST `doc` fields. The lexer is a sibling
    // module in this crate, so calling it here is fine.

    use crate::lexer::lex;

    /// Lex + parse real source, asserting success.
    fn parse_src(src: &str) -> Program {
        let toks = lex(src).expect("source should lex");
        match parse(&toks) {
            Ok(p) => p,
            Err(e) => panic!("expected parse success, got errors: {:?}", e),
        }
    }

    #[test]
    fn doc_on_fn() {
        let p = parse_src("## adds\nfn add():\n    1\n");
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => assert_eq!(f.doc.as_deref(), Some("adds")),
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn doc_on_struct_and_field() {
        let p = parse_src(
            "## a point\nstruct Point:\n    ## the x coordinate\n    x: Float\n    y: Float\n",
        );
        match &only_stmt(&p).kind {
            StmtKind::Struct(s) => {
                assert_eq!(s.doc.as_deref(), Some("a point"));
                assert_eq!(s.fields[0].doc.as_deref(), Some("the x coordinate"));
                // The undocumented field has no doc.
                assert_eq!(s.fields[1].doc, None);
            }
            other => panic!("expected struct, got {:?}", other),
        }
    }

    #[test]
    fn doc_on_enum_and_variant() {
        let p = parse_src("## a shape\nenum Shape:\n    ## a circle\n    Circle(Float)\n    Square\n");
        match &only_stmt(&p).kind {
            StmtKind::Enum(e) => {
                assert_eq!(e.doc.as_deref(), Some("a shape"));
                assert_eq!(e.variants[0].doc.as_deref(), Some("a circle"));
                assert_eq!(e.variants[1].doc, None);
            }
            other => panic!("expected enum, got {:?}", other),
        }
    }

    #[test]
    fn doc_on_multiline_joins() {
        let p = parse_src("## line one\n## line two\nfn f():\n    1\n");
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => assert_eq!(f.doc.as_deref(), Some("line one\nline two")),
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn doc_on_impl_method() {
        let p = parse_src("impl Point:\n    ## the magnitude\n    fn mag(self):\n        1\n");
        match &only_stmt(&p).kind {
            StmtKind::Impl(i) => {
                assert_eq!(i.methods.len(), 1);
                assert_eq!(i.methods[0].doc.as_deref(), Some("the magnitude"));
            }
            other => panic!("expected impl, got {:?}", other),
        }
    }

    #[test]
    fn blank_line_detaches_doc() {
        let p = parse_src("## orphaned\n\nfn f():\n    1\n");
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => assert_eq!(f.doc, None),
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn plain_hash_is_not_a_doc() {
        let p = parse_src("# just a comment\nfn f():\n    1\n");
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => assert_eq!(f.doc, None),
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn no_docs_parses_with_all_doc_fields_none() {
        // Regression: a program with NO doc comments parses with every `doc`
        // field `None` and nothing else perturbed.
        let bare = parse_src(
            "enum Expr:\n    Num(Float)\n    Add(Expr, Expr)\n\nfn eval(e: Expr) -> Float:\n    1\n",
        );
        assert_eq!(bare.stmts.len(), 2);
        for stmt in &bare.stmts {
            match &stmt.kind {
                StmtKind::Enum(e) => {
                    assert_eq!(e.doc, None);
                    for v in &e.variants {
                        assert_eq!(v.doc, None);
                    }
                }
                StmtKind::Fn(f) => assert_eq!(f.doc, None),
                other => panic!("unexpected stmt {:?}", other),
            }
        }
    }

    #[test]
    fn doc_changes_only_the_doc_field() {
        // Adding a doc to a decl must change *only* its `doc` field — the rest of
        // the AST (down to spans) is unaffected when the decl sits at the same
        // source offset. We arrange identical offsets by putting the doc on the
        // *first* line in one program and a same-length plain `#` comment (which
        // is discarded, not a doc) on the first line of the other, so the `fn`
        // begins at the same byte/line in both.
        let documented = parse_src("## docs!!\nfn f():\n    1\n");
        let undocumented = parse_src("# docs!!!\nfn f():\n    1\n"); // same byte length line

        let f_doc = match &only_stmt(&documented).kind {
            StmtKind::Fn(f) => f.clone(),
            other => panic!("expected fn, got {:?}", other),
        };
        let f_none = match &only_stmt(&undocumented).kind {
            StmtKind::Fn(f) => f.clone(),
            other => panic!("expected fn, got {:?}", other),
        };

        assert_eq!(f_doc.doc.as_deref(), Some("docs!!"));
        assert_eq!(f_none.doc, None);
        // Strip the docs and everything else (name, params, body, span) matches.
        let stripped = FnDecl { doc: None, ..f_doc };
        assert_eq!(stripped, f_none);
    }
}
