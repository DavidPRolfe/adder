use super::*;

impl<'a> Parser<'a> {
    // =====================================================================
    // Types (§6)
    // =====================================================================

    /// `type = base_type [ "?" ]`.
    ///
    /// A parenthesized type is one of (M2): the unit type `()`; a grouped type
    /// `(T)`; a tuple type `(A, B, …)` (≥2 components); or — when a `->` follows
    /// the closing `)` — a **function type** `(T1, …) -> R` (zero params is
    /// `() -> R`). Function/tuple types are parsed for documentation and
    /// higher-order signatures; they are not statically checked in M2.
    pub(crate) fn parse_type(&mut self) -> PResult<Type> {
        let start = self.cur_span();
        let base = match self.peek().clone() {
            TokenKind::LParen => {
                // Parse the parenthesized component list, then decide the shape.
                self.advance(); // `(`
                let comps = self.parse_separated(&TokenKind::RParen, Self::parse_type)?;
                self.expect(&TokenKind::RParen, "`)` to close a parenthesized type")?;

                // `(…) -> R` is a function type (any param count, including 0).
                if matches!(self.peek(), TokenKind::Arrow) {
                    self.advance(); // `->`
                    let ret = self.parse_type()?;
                    BaseType::Fn { params: comps, ret: Box::new(ret) }
                } else {
                    match comps.len() {
                        0 => BaseType::Unit,
                        // `(T)` is grouping: return the inner type, applying any
                        // trailing `?` to it (so `(T)?` is `T?`).
                        1 => {
                            let mut inner = comps.into_iter().next().unwrap();
                            if matches!(self.peek(), TokenKind::Question) {
                                let q = self.cur_span();
                                self.advance();
                                inner.nullable = true;
                                inner.span = start.merge(q);
                            } else {
                                inner.span = start.merge(inner.span);
                            }
                            return Ok(inner);
                        }
                        _ => BaseType::Tuple(comps),
                    }
                }
            }
            TokenKind::Name(name) => {
                self.advance();
                let mut args = Vec::new();
                if matches!(self.peek(), TokenKind::LBracket) {
                    self.advance();
                    args = self.parse_separated(&TokenKind::RBracket, Self::parse_type)?;
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
    pub(crate) fn prev_span_or(&self, fallback: Span) -> Span {
        if self.pos == 0 {
            fallback
        } else {
            self.tokens
                .get(self.pos - 1)
                .map(|t| t.span)
                .unwrap_or(fallback)
        }
    }
}
