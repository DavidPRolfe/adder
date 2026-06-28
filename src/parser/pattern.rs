use super::*;

impl<'a> Parser<'a> {
    // =====================================================================
    // Patterns (§5.8) — recursive as of M2
    // =====================================================================

    /// `pattern = primary_pattern { "or" primary_pattern }`.
    ///
    /// The top level is an **or-pattern** (M2 Wave 2): one or more alternatives
    /// separated by `or`, matching if *any* alternative matches. A single
    /// alternative collapses to that pattern (no `Or` wrapper). Or-patterns
    /// parse here both as a whole-arm pattern and, because variant sub-patterns
    /// recurse through [`Self::parse_pattern`], inside sub-patterns too. Literal
    /// alternatives (`1 or 2`) and variant alternatives (`.A or .B`) both work.
    pub(crate) fn parse_pattern(&mut self) -> PResult<Pattern> {
        let first = self.parse_pattern_primary()?;
        if !matches!(self.peek(), TokenKind::Or) {
            return Ok(first);
        }
        let start = first.span;
        let mut alts = vec![first];
        while self.eat(&TokenKind::Or) {
            alts.push(self.parse_pattern_primary()?);
        }
        let span = start.merge(alts.last().unwrap().span);
        Ok(Pattern { kind: PatternKind::Or(alts), span })
    }

    /// `primary_pattern = "_" | NULL | literal_pattern | NAME | variant_pattern
    /// | tuple_pattern`.
    ///
    /// A single alternative of an or-pattern. A parenthesized form is either a
    /// **tuple pattern** `(p1, p2, …)` (≥2 elements) or grouping `(p)` (which is
    /// transparent — `(p)` is just `p`). Variant sub-patterns are recursive, so
    /// nested destructuring like `.Some(.Pair(a, b))` parses here.
    pub(crate) fn parse_pattern_primary(&mut self) -> PResult<Pattern> {
        let span = self.cur_span();
        match self.peek().clone() {
            // Tuple pattern `(p1, p2, …)`, or grouping `(p)`.
            TokenKind::LParen => self.parse_tuple_pattern(),
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
                } else if matches!(self.peek(), TokenKind::LParen) && (name == "Ok" || name == "Err") {
                    // The prelude `Result` variants (M3; spec §9) may be matched
                    // unqualified — `Ok(v):` / `Err(e):` — mirroring their bare
                    // `Ok(...)` / `Err(...)` constructors. User enum variants
                    // still name their enum (the branch below).
                    let (subs, end) = self.parse_variant_subs(span)?;
                    Ok(Pattern {
                        kind: PatternKind::Variant { enum_name: None, name, subs },
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
    pub(crate) fn parse_variant_subs(&mut self, name_span: Span) -> PResult<(Vec<Pattern>, Span)> {
        if !matches!(self.peek(), TokenKind::LParen) {
            return Ok((Vec::new(), name_span));
        }
        self.advance(); // `(`
        let subs = self.parse_separated(&TokenKind::RParen, Self::parse_pattern)?;
        let close = self.expect(&TokenKind::RParen, "`)` to close a variant pattern")?;
        Ok((subs, close.span))
    }

    /// Parse a parenthesized pattern: a **tuple pattern** `(p1, p2, …)` (≥2
    /// elements, destructured element-wise against a tuple value) or, with no
    /// comma, transparent grouping `(p)` (which is just `p`). Mirrors the
    /// expression-side `(a, b)` tuple / `(e)` grouping distinction.
    pub(crate) fn parse_tuple_pattern(&mut self) -> PResult<Pattern> {
        let start = self.cur_span();
        self.advance(); // `(`
        let first = self.parse_pattern()?;
        if matches!(self.peek(), TokenKind::Comma) {
            let mut elems = vec![first];
            while self.eat(&TokenKind::Comma) {
                if matches!(self.peek(), TokenKind::RParen) {
                    break; // tolerate a trailing comma
                }
                elems.push(self.parse_pattern()?);
            }
            let close = self.expect(&TokenKind::RParen, "`)` to close a tuple pattern")?;
            Ok(Pattern { kind: PatternKind::Tuple(elems), span: start.merge(close.span) })
        } else {
            let close = self.expect(&TokenKind::RParen, "`)` to close a grouped pattern")?;
            // Grouping is transparent; widen the span to include the parens.
            Ok(Pattern { kind: first.kind, span: start.merge(close.span) })
        }
    }
}
