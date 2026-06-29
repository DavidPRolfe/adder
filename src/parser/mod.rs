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

mod control;
mod expr;
mod item;
mod pattern;
mod stmt;
mod types;
#[cfg(test)]
mod tests;

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

    /// Parse zero-or-more `parse_item`, comma-separated, tolerating a single
    /// trailing comma, stopping at (but **not** consuming) `close`. The caller
    /// performs the closing `expect`/bracket consumption.
    ///
    /// Semantics match the hand-rolled comma loops elsewhere in this file: if
    /// the current token is already `close`, no items are parsed; otherwise an
    /// item is parsed, then for each `,` consumed a following `close` ends the
    /// list (trailing comma) and anything else is parsed as the next item.
    fn parse_separated<T>(
        &mut self,
        close: &TokenKind,
        mut parse_item: impl FnMut(&mut Self) -> PResult<T>,
    ) -> PResult<Vec<T>> {
        let mut items = Vec::new();
        if self.peek() == close {
            return Ok(items);
        }
        loop {
            items.push(parse_item(self)?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            if self.peek() == close {
                break;
            }
        }
        Ok(items)
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
        TokenKind::Trait => "`trait`".into(),
        TokenKind::Try => "`try`".into(),
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
