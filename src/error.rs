//! Diagnostics shared by every stage of the pipeline.
//!
//! All four stages (`lex`, `parse`, `check`, `run`) report problems as
//! [`Diagnostic`]s pointing at a [`Span`]. [`Diagnostic::render`] turns one into
//! a human-readable, multi-line string with a caret underline against the
//! original source — used by both the CLI ([`crate::main`]) and tests.

use crate::token::Span;

/// Which pipeline phase produced a diagnostic. Used for labeling and lets the
/// CLI/tests group or filter errors by stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Tokenization (`lexer/`).
    Lex,
    /// Parsing (`parser/`).
    Parse,
    /// Static checks — exhaustiveness + null-narrowing (`checks/`).
    Check,
    /// Runtime evaluation (`interp/`).
    Runtime,
}

impl Phase {
    /// Short human label for the phase, e.g. `"parse error"`.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Lex => "lex error",
            Phase::Parse => "parse error",
            Phase::Check => "check error",
            Phase::Runtime => "runtime error",
        }
    }
}

/// A single diagnostic message tied to a source location.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    /// The phase that produced this diagnostic.
    pub phase: Phase,
    /// The human-readable message.
    pub message: String,
    /// Where in the source it points.
    pub span: Span,
}

impl Diagnostic {
    pub fn new(phase: Phase, message: impl Into<String>, span: Span) -> Self {
        Diagnostic { phase, message: message.into(), span }
    }

    /// Convenience constructors per phase.
    pub fn lex(message: impl Into<String>, span: Span) -> Self {
        Diagnostic::new(Phase::Lex, message, span)
    }
    pub fn parse(message: impl Into<String>, span: Span) -> Self {
        Diagnostic::new(Phase::Parse, message, span)
    }
    pub fn check(message: impl Into<String>, span: Span) -> Self {
        Diagnostic::new(Phase::Check, message, span)
    }
    pub fn runtime(message: impl Into<String>, span: Span) -> Self {
        Diagnostic::new(Phase::Runtime, message, span)
    }

    /// Render this diagnostic against the original `src` as a multi-line string:
    ///
    /// ```text
    /// parse error: expected ':' after condition
    ///  --> line 3, col 9
    ///   |
    /// 3 |     if ready
    ///   |         ^^^^^
    /// ```
    ///
    /// The caret line underlines the span on its starting line. Spans that
    /// cross line boundaries are underlined only on the first line (kept simple
    /// on purpose). Falls back gracefully if the span is out of bounds.
    pub fn render(&self, src: &str) -> String {
        let mut out = String::new();
        out.push_str(self.phase.label());
        out.push_str(": ");
        out.push_str(&self.message);
        out.push('\n');

        let line_no = self.span.line.max(1);
        let col_no = self.span.col.max(1);
        out.push_str(&format!(" --> line {}, col {}\n", line_no, col_no));

        // Locate the text of the offending line (1-based).
        let line_text = src.lines().nth(line_no - 1).unwrap_or("");
        let gutter = line_no.to_string();
        let pad = " ".repeat(gutter.len());

        out.push_str(&format!("{} |\n", pad));
        out.push_str(&format!("{} | {}\n", gutter, line_text));

        // Build the caret underline. Column is 1-based in scalar values.
        let caret_col = col_no.saturating_sub(1);
        // Underline length: clamp the span width to what remains on this line.
        let span_width = self.span.end.saturating_sub(self.span.start).max(1);
        let remaining = line_text.chars().count().saturating_sub(caret_col).max(1);
        let underline = span_width.min(remaining);

        out.push_str(&format!(
            "{} | {}{}\n",
            pad,
            " ".repeat(caret_col),
            "^".repeat(underline.max(1))
        ));

        out
    }
}

/// Render a batch of diagnostics against the source, separated by blank lines.
pub fn render_all(diags: &[Diagnostic], src: &str) -> String {
    diags
        .iter()
        .map(|d| d.render(src))
        .collect::<Vec<_>>()
        .join("\n")
}
