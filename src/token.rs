//! The **lexer ↔ parser contract**: tokens and source spans.
//!
//! This module is the authority for the *lexical* surface of Adder M1 (grammar
//! §1). The lexer ([`crate::lexer::lex`]) produces a `Vec<Token>`; the parser
//! ([`crate::parser::parse`]) consumes it. Both agents build against the types
//! here, so this file is intended to be **complete** — downstream agents should
//! not need to modify it.
//!
//! Every [`Token`] carries a [`Span`] for error reporting. Synthetic layout
//! tokens (`Newline`, `Indent`, `Dedent`, `Eof`) are produced by the lexer's
//! off-side-rule pass (grammar §1.2) and appear in the stream like any other
//! terminal.

use num_bigint::BigInt;

/// A byte-offset range into the original source string, plus precomputed
/// line/column of the start position so diagnostics can be rendered without
/// re-scanning.
///
/// `start`/`end` are **byte** offsets (half-open: `start..end`) into the UTF-8
/// source. `line` and `col` are **1-based** and refer to the `start` position;
/// they are a convenience for error rendering. Keep them in sync with `start`
/// when constructing spans in the lexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the first character (inclusive).
    pub start: usize,
    /// Byte offset one past the last character (exclusive).
    pub end: usize,
    /// 1-based line number of `start`.
    pub line: usize,
    /// 1-based column number of `start` (in Unicode scalar values, counting
    /// from the start of the line).
    pub col: usize,
}

impl Span {
    /// Construct a span from byte offsets and a 1-based line/column.
    pub fn new(start: usize, end: usize, line: usize, col: usize) -> Self {
        Span { start, end, line, col }
    }

    /// A zero-width placeholder span at offset 0 (line 1, col 1). Useful for
    /// synthetic nodes the parser invents that have no real source location.
    pub fn dummy() -> Self {
        Span { start: 0, end: 0, line: 1, col: 1 }
    }

    /// Merge two spans into the smallest span covering both. Line/col are taken
    /// from whichever span starts earlier.
    pub fn merge(self, other: Span) -> Span {
        let (lo, _hi) = if self.start <= other.start {
            (self, other)
        } else {
            (other, self)
        };
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            line: lo.line,
            col: lo.col,
        }
    }
}

/// One piece of a string literal's value (grammar §1.5).
///
/// A `STRING` token does **not** carry a single decoded `String`; instead it
/// carries an ordered list of `StrPart`s so that interpolations can be parsed
/// as expressions later. Escapes in literal text are **already resolved** by
/// the lexer (e.g. `\n` → newline, `{{` → `{`).
///
/// For an [`StrPart::Interp`], the lexer re-lexes the source between the
/// braces into its own `Vec<Token>` (terminated by an [`Token::Eof`]). The
/// **parser** then recursively parses that nested stream as an `expr`
/// (grammar §5) — see [`crate::ast::StringLit`]. This keeps interpolation a
/// purely lexical/structural concern at this layer; no expression parsing
/// happens in the lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    /// Literal text with all escapes already resolved.
    Text(String),
    /// An interpolation `{ expr }`: the nested token stream lexed from between
    /// the braces. Includes a trailing [`Token::Eof`]. The parser turns this
    /// into an `expr` AST node.
    Interp(Vec<Token>),
}

/// A lexical token: a terminal of grammar §1 paired with its source [`Span`].
///
/// Keywords (§1.3), literals (§1.4–§1.5), operators/punctuation (§1.6), and the
/// synthetic layout tokens (§1.2) are all represented here.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Token { kind, span }
    }
}

/// The kind of a [`Token`] — every terminal in grammar §1.
///
/// Notes for downstream agents:
/// - `INT` is stored pre-parsed as a [`BigInt`] (digit separators `_` already
///   stripped by the lexer). `FLOAT` is an `f64`. `STRING` is a list of
///   [`StrPart`]s (see that type for interpolation handling).
/// - `is not` lexes as **two** tokens: [`TokenKind::Is`] then
///   [`TokenKind::Not`]. There is no combined `IsNot` token; the parser
///   recognizes the pair (grammar §5.3).
/// - `true`/`false`/`null` are their own keyword-kinds ([`TokenKind::True`],
///   [`TokenKind::False`], [`TokenKind::Null`]) rather than a generic literal,
///   matching the grammar's `BOOL`/`NULL` terminals.
/// - `print`/`panic` are **not** keywords — they lex as [`TokenKind::Name`]
///   (they are prelude bindings, grammar §1.3/§5.6).
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ----- Identifiers & literals (§1.3–§1.5) -----
    /// An identifier (not a reserved keyword). Holds the source text.
    Name(String),
    /// Arbitrary-precision integer literal, with `_` separators already removed.
    Int(BigInt),
    /// 64-bit floating-point literal.
    Float(f64),
    /// String literal as a sequence of parts (text + interpolations).
    Str(Vec<StrPart>),

    // ----- Keywords (§1.3) -----
    Fn,
    Val,
    Struct,
    Enum,
    Impl,
    Return,
    Returns,
    If,
    Elif,
    Else,
    Match,
    While,
    For,
    In,
    Break,
    Continue,
    And,
    Or,
    Not,
    Is,
    /// The `true` literal (grammar `BOOL`).
    True,
    /// The `false` literal (grammar `BOOL`).
    False,
    /// The `null` literal (grammar `NULL`).
    Null,
    /// The `self` method receiver.
    SelfKw,

    // ----- Comparison operators (§1.6) -----
    EqEq,    // ==
    NotEq,   // !=
    Lt,      // <
    LtEq,    // <=
    Gt,      // >
    GtEq,    // >=

    // ----- Arithmetic operators (§1.6) -----
    Plus,    // +
    Minus,   // -
    Star,    // *
    Slash,   // /
    Percent, // %
    StarStar,// **

    // ----- Assignment / lambda / ranges (§1.6) -----
    Eq,        // =
    Arrow,     // ->
    DotDot,    // ..
    DotDotEq,  // ..=

    // ----- Punctuation (§1.6) -----
    Colon,       // :
    Comma,       // ,
    Dot,         // .
    LParen,      // (
    RParen,      // )
    LBracket,    // [
    RBracket,    // ]
    LBrace,      // {
    RBrace,      // }
    Question,    // ?

    // ----- Synthetic layout tokens (§1.2) -----
    /// End of a logical line.
    Newline,
    /// Start of a deeper-indented block.
    Indent,
    /// End of an indented block (one per level closed).
    Dedent,
    /// End of the token stream.
    Eof,
}
