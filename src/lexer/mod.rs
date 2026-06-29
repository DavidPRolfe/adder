//! Stage 1 — **lexing**: source text → token stream.
//!
//! Owned by the *lexer agent*. Turns UTF-8 source into a `Vec<Token>` following
//! grammar §1, including the off-side-rule layout tokens (`Newline`, `Indent`,
//! `Dedent`) and a terminating `Eof`. String interpolations are re-lexed into
//! nested token streams (see [`crate::token::StrPart`]).
//!
//! Contract: returns the full token stream on success, or the first
//! [`Diagnostic`] on a lexical error (indentation/tab error, unterminated
//! string, unbalanced interpolation brace, invalid escape, stray character).

use num_bigint::BigInt;

use crate::error::Diagnostic;
use crate::token::{Span, StrPart, Token, TokenKind};

/// Lex `src` into a token stream terminated by [`crate::token::TokenKind::Eof`].
///
/// The returned vector includes synthetic `Newline`/`Indent`/`Dedent` tokens
/// per grammar §1.2.
pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut lexer = Lexer::new(src, 0, 1, 1);
    lexer.run_top_level()
}

/// A `(char, byte_offset)` cursor over the source. We keep our own index so we
/// can do multi-character lookahead and slice the source by byte offsets for
/// spans and for re-lexing interpolations.
struct Lexer<'a> {
    src: &'a str,
    /// The byte offset (into `src`) where this (possibly nested) lexer's view
    /// begins. Spans are reported relative to the *original* source, so when a
    /// nested lexer is run over an interpolation substring we pass it the
    /// absolute base offset and starting line/col of that substring.
    base: usize,
    /// Absolute byte offset of the next unconsumed char.
    pos: usize,
    /// 1-based line of the next unconsumed char.
    line: usize,
    /// 1-based column (in Unicode scalar values) of the next unconsumed char.
    col: usize,
    /// Indentation stack (number of leading spaces per open block). Always
    /// starts with `0`.
    indents: Vec<usize>,
    /// Count of currently-open `(`/`[`/`{` brackets. While `> 0`, physical
    /// newlines do not produce `Newline` tokens (bracket-continuation §1.2).
    bracket_depth: usize,
    /// When `true`, this lexer is re-lexing the inner source of a string
    /// interpolation. Interpolations are a single inline `expr`, so we suppress
    /// all layout tokens (`Newline`/`Indent`/`Dedent`) and emit only the
    /// expression's tokens followed by `Eof` (matching the `StrPart::Interp`
    /// contract in `token.rs`).
    inline: bool,
    /// Accumulated `##` doc-comment text (grammar §1.1) for a run of consecutive
    /// `##` comment-only lines. Lines are joined with `\n` (leading `##` and a
    /// single following space already stripped). It is attached to the **first
    /// real token** the lexer next emits (see [`Lexer::push`]) and is reset by a
    /// blank line or a plain `#` comment line, which detach the doc from any
    /// following declaration.
    pending_doc: Option<String>,
    /// Output token stream.
    out: Vec<Token>,
}

impl<'a> Lexer<'a> {
    /// Build a lexer over `src`, where `src` begins at absolute byte offset
    /// `base`, line `line`, column `col` in the original program. For the
    /// top-level lexer these are `(0, 1, 1)`.
    fn new(src: &'a str, base: usize, line: usize, col: usize) -> Self {
        Lexer {
            src,
            base,
            pos: base,
            line,
            col,
            indents: vec![0],
            bracket_depth: 0,
            inline: false,
            pending_doc: None,
            out: Vec::new(),
        }
    }

    /// Build an interpolation sub-lexer (inline mode: no layout tokens).
    fn new_inline(src: &'a str, base: usize, line: usize, col: usize) -> Self {
        let mut l = Lexer::new(src, base, line, col);
        l.inline = true;
        l
    }

    // ------------------------------------------------------------------
    // Cursor primitives
    // ------------------------------------------------------------------

    /// Byte offset relative to the start of our `src` slice.
    fn local(&self) -> usize {
        self.pos - self.base
    }

    /// Peek the char at the current position without consuming.
    fn peek(&self) -> Option<char> {
        self.src[self.local()..].chars().next()
    }

    /// Peek the char `n` chars ahead (0 == current).
    fn peek_n(&self, n: usize) -> Option<char> {
        self.src[self.local()..].chars().nth(n)
    }

    /// Consume and return the current char, advancing line/col bookkeeping.
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    /// A span covering byte range `[start, end)` whose start is at the recorded
    /// `(line, col)`.
    fn span_at(&self, start: usize, end: usize, line: usize, col: usize) -> Span {
        Span::new(start, end, line, col)
    }

    /// A zero-width span at the current cursor position (for synthetic tokens).
    fn here(&self) -> Span {
        Span::new(self.pos, self.pos, self.line, self.col)
    }

    fn push(&mut self, kind: TokenKind, span: Span) {
        // Attach any pending `##` doc comment to the first *real* token after the
        // doc block. Synthetic layout tokens (`Newline`/`Indent`/`Dedent`/`Eof`)
        // never carry a doc — the doc belongs to the declaration's leading token
        // (e.g. the `fn`/`struct`/`enum` keyword, or a field/variant name), which
        // may be preceded by an `Indent` for an indented declaration.
        let doc = if is_layout(&kind) {
            None
        } else {
            self.pending_doc.take()
        };
        self.out.push(Token::with_doc(kind, span, doc));
    }

    // ------------------------------------------------------------------
    // Top-level driver (off-side rule)
    // ------------------------------------------------------------------

    /// Run the full off-side-rule lexer over the whole source, returning the
    /// token stream ending in `Eof`. Used by the top-level `lex` entry point.
    fn run_top_level(&mut self) -> Result<Vec<Token>, Diagnostic> {
        // Whether the previous logical line emitted any "real" content (and thus
        // a trailing Newline is owed at EOF / before a dedent block).
        let mut produced_line = false;

        loop {
            // --- Start of a physical line: measure indentation. ---
            // Only meaningful when not inside brackets, and never in inline
            // (interpolation) mode where layout is irrelevant.
            if self.bracket_depth == 0 && !self.inline {
                let indent = self.measure_indent()?;
                match indent {
                    IndentLine::Blank => {
                        // Blank or comment-only line: no tokens, no layout.
                        if self.peek().is_none() {
                            break;
                        }
                        continue;
                    }
                    IndentLine::Content(width) => {
                        self.apply_indentation(width)?;
                    }
                }
            }

            // --- Lex tokens until the logical line ends. ---
            let line_had_content = self.lex_logical_line()?;
            if line_had_content {
                produced_line = true;
            }

            if self.peek().is_none() {
                break;
            }
        }

        // EOF: emit a final Newline (if the last logical line produced content
        // and we haven't already), then a Dedent per open block, then Eof.
        // Inline (interpolation) mode emits no layout tokens — just the
        // expression's tokens followed by Eof.
        if !self.inline {
            if produced_line {
                // Avoid a duplicate Newline if the source ended right after one.
                let needs_nl =
                    !matches!(self.out.last().map(|t| &t.kind), Some(TokenKind::Newline));
                if needs_nl {
                    self.push(TokenKind::Newline, self.here());
                }
            }
            while self.indents.len() > 1 {
                self.indents.pop();
                self.push(TokenKind::Dedent, self.here());
            }
        }
        self.push(TokenKind::Eof, self.here());
        Ok(std::mem::take(&mut self.out))
    }

    /// Measure the leading whitespace of the current physical line. Returns
    /// `Blank` for blank / comment-only lines (consuming through their newline),
    /// or `Content(width)` for a line with real tokens, leaving the cursor at the
    /// first non-whitespace char.
    fn measure_indent(&mut self) -> Result<IndentLine, Diagnostic> {
        let mut width = 0usize;
        loop {
            match self.peek() {
                Some(' ') => {
                    self.bump();
                    width += 1;
                }
                Some('\t') => {
                    let sp = self.here();
                    return Err(Diagnostic::lex(
                        "tab in leading whitespace is not allowed (use 4 spaces per indent level)",
                        sp,
                    ));
                }
                Some('\r') => {
                    // Tolerate CR (CRLF line endings); don't count toward indent.
                    self.bump();
                }
                Some('\n') => {
                    // Blank line: detaches any pending doc from a following decl.
                    self.bump();
                    self.pending_doc = None;
                    return Ok(IndentLine::Blank);
                }
                Some('#') => {
                    // Comment-only line. A `##` line accumulates into the pending
                    // doc comment; a plain `#` line is an ordinary comment that
                    // detaches any pending doc (it is no longer immediately above
                    // the next declaration).
                    self.consume_doc_or_comment();
                    // Consume the trailing newline (if any) so the next call
                    // starts on a fresh physical line.
                    if let Some('\n') = self.peek() {
                        self.bump();
                    }
                    return Ok(IndentLine::Blank);
                }
                None => {
                    // EOF with only whitespace on the line: detaches pending doc.
                    self.pending_doc = None;
                    return Ok(IndentLine::Blank);
                }
                Some(_) => {
                    return Ok(IndentLine::Content(width));
                }
            }
        }
    }

    /// Compare `width` against the indentation stack, emitting Indent/Dedent.
    fn apply_indentation(&mut self, width: usize) -> Result<(), Diagnostic> {
        let current = *self.indents.last().unwrap();
        if width > current {
            self.indents.push(width);
            self.push(TokenKind::Indent, self.here());
        } else if width < current {
            while *self.indents.last().unwrap() > width {
                self.indents.pop();
                self.push(TokenKind::Dedent, self.here());
            }
            if *self.indents.last().unwrap() != width {
                return Err(Diagnostic::lex(
                    "inconsistent indentation: does not match any enclosing block level",
                    self.here(),
                ));
            }
        }
        Ok(())
    }

    /// Lex a single logical line's worth of tokens (which may span physical
    /// lines while brackets are open). Stops after emitting the line-terminating
    /// `Newline` (when at bracket depth 0) or at EOF. Returns whether any real
    /// token was produced on this logical line.
    fn lex_logical_line(&mut self) -> Result<bool, Diagnostic> {
        let mut produced = false;
        loop {
            match self.peek() {
                None => {
                    // EOF mid-line: stop; the caller handles trailing Newline.
                    return Ok(produced);
                }
                Some('\n') => {
                    self.bump();
                    if self.bracket_depth == 0 {
                        if produced {
                            self.push(TokenKind::Newline, self.here());
                        }
                        return Ok(produced);
                    }
                    // Inside brackets: newline suppressed. Skip any leading
                    // whitespace of the continuation line and keep going.
                    self.skip_inline_ws_and_comments_across_lines()?;
                }
                Some('\r') => {
                    self.bump();
                }
                Some(' ') | Some('\t') => {
                    // Inline (non-leading) whitespace separates tokens. A tab
                    // here is fine — the tab restriction is leading-only.
                    self.bump();
                }
                Some('#') => {
                    self.consume_comment();
                    // Comment runs to end of line; the loop will see the '\n'
                    // (or EOF) next and terminate the logical line.
                }
                Some(_) => {
                    self.lex_token()?;
                    produced = true;
                }
            }
        }
    }

    /// While inside brackets, after consuming a newline, skip the whitespace and
    /// any comment lines of continuation physical lines. (Indentation is
    /// irrelevant inside brackets.)
    fn skip_inline_ws_and_comments_across_lines(&mut self) -> Result<(), Diagnostic> {
        loop {
            match self.peek() {
                Some(' ') | Some('\t') | Some('\r') => {
                    self.bump();
                }
                Some('#') => {
                    self.consume_comment();
                }
                Some('\n') => {
                    self.bump();
                }
                _ => return Ok(()),
            }
        }
    }

    /// Consume a `#`-comment up to (but not including) the end-of-line newline.
    fn consume_comment(&mut self) {
        debug_assert_eq!(self.peek(), Some('#'));
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.bump();
        }
    }

    /// Consume a comment-only line, distinguishing a `##` doc comment from a
    /// plain `#` comment (grammar §1.1). The cursor is on the leading `#`.
    ///
    /// - `## text` (two-or-more leading hashes): append `text` (with the leading
    ///   `##` and a single following space stripped) to [`Lexer::pending_doc`],
    ///   joining consecutive doc lines with `\n`. This doc is later attached to
    ///   the first real token of the next logical line (see [`Lexer::push`]).
    /// - `# text` (a single `#`): an ordinary comment. It is discarded *and*
    ///   detaches any pending doc — a plain comment between a `##` block and a
    ///   declaration breaks the "immediately above" relationship.
    ///
    /// Either way the comment runs to (but does not consume) the newline. Doc
    /// capture is suppressed in `inline` (interpolation) mode, where layout and
    /// declarations are irrelevant; there it behaves like [`Self::consume_comment`].
    fn consume_doc_or_comment(&mut self) {
        debug_assert_eq!(self.peek(), Some('#'));
        // A doc comment requires `##` and is meaningless inside an interpolation.
        let is_doc = !self.inline && self.peek_n(1) == Some('#');
        if !is_doc {
            // Plain comment: discard and detach any pending doc.
            self.consume_comment();
            self.pending_doc = None;
            return;
        }

        // Skip the two leading `#`s, then a single following space, then capture
        // the rest of the line verbatim (other leading spaces are preserved).
        self.bump(); // first '#'
        self.bump(); // second '#'
        if self.peek() == Some(' ') {
            self.bump();
        }
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            // Tolerate CRLF: don't fold a trailing '\r' into the captured text.
            if c == '\r' && self.peek_n(1) == Some('\n') {
                break;
            }
            text.push(c);
            self.bump();
        }

        match &mut self.pending_doc {
            Some(existing) => {
                existing.push('\n');
                existing.push_str(&text);
            }
            None => self.pending_doc = Some(text),
        }
    }

    // ------------------------------------------------------------------
    // Token scanners
    // ------------------------------------------------------------------

    /// Lex one non-whitespace, non-newline token starting at the current cursor.
    fn lex_token(&mut self) -> Result<(), Diagnostic> {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        let c = self.peek().expect("lex_token called at EOF");

        if is_id_start(c) {
            return self.lex_name_or_keyword(start, line, col);
        }
        if c.is_ascii_digit() {
            return self.lex_number(start, line, col);
        }
        if c == '"' {
            return self.lex_string(start, line, col);
        }
        self.lex_operator(start, line, col)
    }

    fn lex_name_or_keyword(
        &mut self,
        start: usize,
        line: usize,
        col: usize,
    ) -> Result<(), Diagnostic> {
        while let Some(c) = self.peek() {
            if is_id_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        let text = &self.src[start - self.base..self.pos - self.base];
        let kind = keyword_kind(text).unwrap_or_else(|| TokenKind::Name(text.to_string()));
        let span = self.span_at(start, self.pos, line, col);
        self.push(kind, span);
        Ok(())
    }

    /// Lex an `INT` or `FLOAT` (§1.4). `_` separators are allowed between digits
    /// but not leading/trailing nor adjacent to `.`. A `.` is a float point only
    /// when a digit follows it (so `0..n` and `x.field` don't become floats).
    fn lex_number(&mut self, start: usize, line: usize, col: usize) -> Result<(), Diagnostic> {
        // Integer part.
        self.consume_digit_run(start, line, col)?;

        // Optional fractional part: only if '.' is followed by a digit.
        let is_float = matches!(self.peek(), Some('.'))
            && matches!(self.peek_n(1), Some(d) if d.is_ascii_digit());

        if is_float {
            self.bump(); // consume '.'
            // The char before '.' must not be '_' (already guaranteed: a run
            // never ends on '_', see consume_digit_run). Consume fractional run.
            self.consume_digit_run(start, line, col)?;
        }

        let raw = &self.src[start - self.base..self.pos - self.base];
        let cleaned: String = raw.chars().filter(|&c| c != '_').collect();
        let span = self.span_at(start, self.pos, line, col);

        if is_float {
            match cleaned.parse::<f64>() {
                Ok(f) => self.push(TokenKind::Float(f), span),
                Err(_) => {
                    return Err(Diagnostic::lex(
                        format!("invalid float literal `{}`", raw),
                        span,
                    ))
                }
            }
        } else {
            match cleaned.parse::<BigInt>() {
                Ok(i) => self.push(TokenKind::Int(i), span),
                Err(_) => {
                    return Err(Diagnostic::lex(
                        format!("invalid integer literal `{}`", raw),
                        span,
                    ))
                }
            }
        }
        Ok(())
    }

    /// Consume a run of `digit (digit | '_')*`, rejecting a trailing `_` (which
    /// would be either a trailing separator or one adjacent to a following `.`).
    /// Assumes the current char is a digit.
    fn consume_digit_run(
        &mut self,
        start: usize,
        line: usize,
        col: usize,
    ) -> Result<(), Diagnostic> {
        debug_assert!(self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false));
        let mut last_was_underscore = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.bump();
                last_was_underscore = false;
            } else if c == '_' {
                self.bump();
                last_was_underscore = true;
            } else {
                break;
            }
        }
        if last_was_underscore {
            let span = self.span_at(start, self.pos, line, col);
            return Err(Diagnostic::lex(
                "digit separator `_` may not be trailing or adjacent to `.`",
                span,
            ));
        }
        Ok(())
    }

    /// Lex a `"`-delimited string literal into a `Str(Vec<StrPart>)` (§1.5).
    fn lex_string(&mut self, start: usize, line: usize, col: usize) -> Result<(), Diagnostic> {
        self.bump(); // opening '"'
        let mut parts: Vec<StrPart> = Vec::new();
        let mut text = String::new();

        loop {
            let c = match self.peek() {
                None => {
                    let span = self.span_at(start, self.pos, line, col);
                    return Err(Diagnostic::lex("unterminated string literal", span));
                }
                Some('\n') => {
                    let span = self.span_at(start, self.pos, line, col);
                    return Err(Diagnostic::lex(
                        "unterminated string literal (Adder strings are single-line)",
                        span,
                    ));
                }
                Some(c) => c,
            };

            match c {
                '"' => {
                    self.bump();
                    break;
                }
                '\\' => {
                    let esc_line = self.line;
                    let esc_col = self.col;
                    let esc_start = self.pos;
                    self.bump(); // backslash
                    let e = match self.peek() {
                        None => {
                            let span = self.span_at(start, self.pos, line, col);
                            return Err(Diagnostic::lex(
                                "unterminated string literal (trailing backslash)",
                                span,
                            ));
                        }
                        Some(e) => e,
                    };
                    let resolved = match e {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        '{' => '{',
                        '}' => '}',
                        other => {
                            let span =
                                self.span_at(esc_start, self.pos + other.len_utf8(), esc_line, esc_col);
                            return Err(Diagnostic::lex(
                                format!("invalid string escape `\\{}`", other),
                                span,
                            ));
                        }
                    };
                    self.bump(); // the escape char
                    text.push(resolved);
                }
                '{' => {
                    // `{{` is a literal `{`; a single `{` opens an interpolation.
                    if let Some('{') = self.peek_n(1) {
                        self.bump();
                        self.bump();
                        text.push('{');
                    } else {
                        // Flush pending text.
                        if !text.is_empty() {
                            parts.push(StrPart::Text(std::mem::take(&mut text)));
                        }
                        let interp = self.lex_interpolation(start, line, col)?;
                        parts.push(StrPart::Interp(interp));
                    }
                }
                '}' => {
                    // `}}` is a literal `}`; a lone `}` outside an interpolation
                    // is an error (unbalanced).
                    if let Some('}') = self.peek_n(1) {
                        self.bump();
                        self.bump();
                        text.push('}');
                    } else {
                        let span = self.here();
                        return Err(Diagnostic::lex(
                            "unexpected `}` in string (use `}}` for a literal brace)",
                            span,
                        ));
                    }
                }
                _ => {
                    self.bump();
                    text.push(c);
                }
            }
        }

        if !text.is_empty() {
            parts.push(StrPart::Text(text));
        }

        let span = self.span_at(start, self.pos, line, col);
        self.push(TokenKind::Str(parts), span);
        Ok(())
    }

    /// At a single `{` inside a string, find the matching `}` (tracking nested
    /// brackets and inner strings), re-lex the inner expression source, and
    /// return its tokens (already ending in `Eof`).
    fn lex_interpolation(
        &mut self,
        str_start: usize,
        str_line: usize,
        str_col: usize,
    ) -> Result<Vec<Token>, Diagnostic> {
        debug_assert_eq!(self.peek(), Some('{'));
        self.bump(); // opening '{'
        let inner_start = self.pos;
        let inner_line = self.line;
        let inner_col = self.col;

        // Scan to the matching '}', honoring nested () [] {} and string literals
        // so that `{f(g(x))}` and `{"a}b"}`-style nesting are balanced correctly.
        let mut depth: usize = 0;
        loop {
            match self.peek() {
                None => {
                    let span = self.span_at(str_start, self.pos, str_line, str_col);
                    return Err(Diagnostic::lex(
                        "unterminated interpolation: missing `}` before end of string",
                        span,
                    ));
                }
                Some('\n') => {
                    let span = self.span_at(str_start, self.pos, str_line, str_col);
                    return Err(Diagnostic::lex(
                        "unterminated interpolation (Adder strings are single-line)",
                        span,
                    ));
                }
                Some('}') if depth == 0 => {
                    break;
                }
                Some('}') => {
                    depth -= 1;
                    self.bump();
                }
                Some('{') | Some('(') | Some('[') => {
                    depth += 1;
                    self.bump();
                }
                Some(')') | Some(']') => {
                    // Closing for ( or [; we don't strictly validate the kind
                    // here (the nested lexer/parser will), just track depth.
                    depth = depth.saturating_sub(1);
                    self.bump();
                }
                Some('"') => {
                    // Skip an entire nested string so its braces/quotes don't
                    // confuse brace matching.
                    self.skip_nested_string(str_start, str_line, str_col)?;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }

        let inner_end = self.pos;
        self.bump(); // closing '}'

        let inner_src = &self.src[inner_start - self.base..inner_end - self.base];
        let mut sub = Lexer::new_inline(inner_src, inner_start, inner_line, inner_col);
        let tokens = sub.run_top_level()?;
        Ok(tokens)
    }

    /// Consume a nested `"`-string while scanning an interpolation, handling
    /// escapes so an escaped quote doesn't end the string early. Leaves the
    /// cursor just past the closing quote.
    fn skip_nested_string(
        &mut self,
        str_start: usize,
        str_line: usize,
        str_col: usize,
    ) -> Result<(), Diagnostic> {
        debug_assert_eq!(self.peek(), Some('"'));
        self.bump(); // opening quote
        loop {
            match self.peek() {
                None | Some('\n') => {
                    let span = self.span_at(str_start, self.pos, str_line, str_col);
                    return Err(Diagnostic::lex(
                        "unterminated string inside interpolation",
                        span,
                    ));
                }
                Some('\\') => {
                    self.bump();
                    // Skip the escaped char (if any).
                    if self.peek().is_some() {
                        self.bump();
                    }
                }
                Some('"') => {
                    self.bump();
                    return Ok(());
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    /// Lex an operator or punctuation token using maximal munch (§1.6).
    fn lex_operator(&mut self, start: usize, line: usize, col: usize) -> Result<(), Diagnostic> {
        let c = self.peek().unwrap();
        let c1 = self.peek_n(1);
        let c2 = self.peek_n(2);

        // Helper macro: advance by N chars then push a token.
        macro_rules! emit {
            ($n:expr, $kind:expr) => {{
                for _ in 0..$n {
                    self.bump();
                }
                let span = self.span_at(start, self.pos, line, col);
                self.push($kind, span);
                return Ok(());
            }};
        }

        match c {
            '=' => match c1 {
                Some('=') => emit!(2, TokenKind::EqEq),
                _ => emit!(1, TokenKind::Eq),
            },
            '!' => match c1 {
                Some('=') => emit!(2, TokenKind::NotEq),
                _ => {
                    let span = self.span_at(start, self.pos + 1, line, col);
                    Err(Diagnostic::lex(
                        "unexpected `!` (did you mean `!=` or `not`?)",
                        span,
                    ))
                }
            },
            '<' => match c1 {
                Some('=') => emit!(2, TokenKind::LtEq),
                _ => emit!(1, TokenKind::Lt),
            },
            '>' => match c1 {
                Some('=') => emit!(2, TokenKind::GtEq),
                _ => emit!(1, TokenKind::Gt),
            },
            '+' => emit!(1, TokenKind::Plus),
            '-' => match c1 {
                Some('>') => emit!(2, TokenKind::Arrow),
                _ => emit!(1, TokenKind::Minus),
            },
            '*' => match c1 {
                Some('*') => emit!(2, TokenKind::StarStar),
                _ => emit!(1, TokenKind::Star),
            },
            '/' => emit!(1, TokenKind::Slash),
            '%' => emit!(1, TokenKind::Percent),
            '.' => match (c1, c2) {
                (Some('.'), Some('=')) => emit!(3, TokenKind::DotDotEq),
                (Some('.'), _) => emit!(2, TokenKind::DotDot),
                _ => emit!(1, TokenKind::Dot),
            },
            ':' => emit!(1, TokenKind::Colon),
            ',' => emit!(1, TokenKind::Comma),
            // `?.` is the safe-call operator; a lone `?` is the nullable
            // type suffix. Maximal munch: only fold `.` in when it immediately
            // follows the `?`.
            '?' => match c1 {
                Some('.') => emit!(2, TokenKind::QuestionDot),
                _ => emit!(1, TokenKind::Question),
            },
            '(' => {
                self.bracket_depth += 1;
                emit!(1, TokenKind::LParen)
            }
            ')' => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                emit!(1, TokenKind::RParen)
            }
            '[' => {
                self.bracket_depth += 1;
                emit!(1, TokenKind::LBracket)
            }
            ']' => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                emit!(1, TokenKind::RBracket)
            }
            '{' => {
                self.bracket_depth += 1;
                emit!(1, TokenKind::LBrace)
            }
            '}' => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                emit!(1, TokenKind::RBrace)
            }
            other => {
                let span = self.span_at(start, self.pos + other.len_utf8(), line, col);
                Err(Diagnostic::lex(
                    format!("unexpected character `{}`", other),
                    span,
                ))
            }
        }
    }
}

/// Result of measuring a physical line's leading whitespace.
enum IndentLine {
    /// Blank or comment-only line (already consumed through its newline).
    Blank,
    /// A line bearing real tokens, with the given indentation width in spaces.
    Content(usize),
}

/// Whether `kind` is a synthetic layout/structural token (§1.2) rather than a
/// "real" terminal. A `##` doc comment is never attached to a layout token — it
/// belongs to the declaration's leading terminal, even when an `Indent` is
/// emitted first for an indented declaration.
fn is_layout(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent | TokenKind::Eof
    )
}

/// `id_start = unicode_letter | "_"`.
fn is_id_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

/// `id_continue = unicode_letter | unicode_digit | "_"`.
fn is_id_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Map a reserved keyword to its `TokenKind`, or `None` for a plain `Name`.
///
/// `trait` and `try` are keywords (spec §7, §9). The
/// remaining reserved-but-unused words (`import from as to`) and the contextual
/// keywords `Self`/`derive` still lex as plain `Name` — `Self` is recognized in
/// type position and `derive` at the head of a struct/enum declaration by the
/// parser (grammar §1). `print` and `panic` are intentionally
/// not keywords. `returns` is not a keyword (function results use `->`), so it
/// too lexes as a plain `Name`.
fn keyword_kind(text: &str) -> Option<TokenKind> {
    Some(match text {
        "fn" => TokenKind::Fn,
        "val" => TokenKind::Val,
        "struct" => TokenKind::Struct,
        "enum" => TokenKind::Enum,
        "impl" => TokenKind::Impl,
        "return" => TokenKind::Return,
        "if" => TokenKind::If,
        "elif" => TokenKind::Elif,
        "else" => TokenKind::Else,
        "match" => TokenKind::Match,
        "while" => TokenKind::While,
        "for" => TokenKind::For,
        "in" => TokenKind::In,
        "break" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "is" => TokenKind::Is,
        "trait" => TokenKind::Trait,
        "try" => TokenKind::Try,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "null" => TokenKind::Null,
        "self" => TokenKind::SelfKw,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
