    use super::*;
    use num_bigint::BigInt;

    /// Lex and unwrap, returning just the kinds (dropping spans) for easy
    /// assertions.
    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src)
            .expect("expected successful lex")
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    fn int(n: i64) -> TokenKind {
        TokenKind::Int(BigInt::from(n))
    }

    #[test]
    fn keywords_vs_names() {
        let ks = kinds("fn val struct enum impl return if elif else match while for in break continue and or not is true false null self\n");
        use TokenKind::*;
        let expected = vec![
            Fn, Val, Struct, Enum, Impl, Return, If, Elif, Else, Match, While, For, In,
            Break, Continue, And, Or, Not, Is, True, False, Null, SelfKw, Newline, Eof,
        ];
        assert_eq!(ks, expected);
    }

    #[test]
    fn returns_is_now_a_plain_name() {
        // `returns` is not a keyword — it lexes as an identifier.
        use TokenKind::*;
        assert_eq!(kinds("returns\n"), vec![Name("returns".into()), Newline, Eof]);
    }

    #[test]
    fn question_dot_vs_question() {
        // `?.` is one token; a lone `?` (nullable suffix) stays separate from a
        // following `.field` access when whitespace intervenes.
        use TokenKind::*;
        assert_eq!(
            kinds("x?.y\n"),
            vec![Name("x".into()), QuestionDot, Name("y".into()), Newline, Eof]
        );
        // `Int?` then a newline: a bare `?` with no following `.`.
        assert_eq!(kinds("Int?\n"), vec![Name("Int".into()), Question, Newline, Eof]);
    }

    #[test]
    fn print_and_panic_are_names() {
        // `print`/`panic` are prelude bindings, not keywords. `Self`/`import`/
        // `from`/`as`/`to` are still reserved-but-unused → plain names; `trait`
        // and `try` are keywords (spec §7, §9).
        let ks = kinds("print panic Self import from as to trait try\n");
        use TokenKind::*;
        assert_eq!(
            ks,
            vec![
                Name("print".into()),
                Name("panic".into()),
                Name("Self".into()),
                Name("import".into()),
                Name("from".into()),
                Name("as".into()),
                Name("to".into()),
                Trait,
                Try,
                Newline,
                Eof,
            ]
        );
    }

    #[test]
    fn unicode_identifiers() {
        let ks = kinds("café _x x1 λ\n");
        use TokenKind::*;
        assert_eq!(
            ks,
            vec![
                Name("café".into()),
                Name("_x".into()),
                Name("x1".into()),
                Name("λ".into()),
                Newline,
                Eof,
            ]
        );
    }

    #[test]
    fn int_and_float_with_underscores() {
        let ks = kinds("1_000_000 3.14 0 42 1_2.3_4\n");
        use TokenKind::*;
        assert_eq!(
            ks,
            vec![
                int(1_000_000),
                Float(3.14),
                int(0),
                int(42),
                Float(12.34),
                Newline,
                Eof,
            ]
        );
    }

    #[test]
    fn range_does_not_become_float() {
        // `0..n` must be Int DotDot Name — never a float.
        let ks = kinds("0..n\n");
        use TokenKind::*;
        assert_eq!(
            ks,
            vec![int(0), DotDot, Name("n".into()), Newline, Eof]
        );
    }

    #[test]
    fn inclusive_range() {
        let ks = kinds("0..=10\n");
        use TokenKind::*;
        assert_eq!(ks, vec![int(0), DotDotEq, int(10), Newline, Eof]);
    }

    #[test]
    fn member_access_not_float() {
        // `x.field` must be Name Dot Name.
        let ks = kinds("x.field\n");
        use TokenKind::*;
        assert_eq!(
            ks,
            vec![Name("x".into()), Dot, Name("field".into()), Newline, Eof]
        );
    }

    #[test]
    fn float_then_member() {
        // `3.14.foo` -> Float(3.14) Dot Name(foo)
        let ks = kinds("3.14.foo\n");
        use TokenKind::*;
        assert_eq!(
            ks,
            vec![Float(3.14), Dot, Name("foo".into()), Newline, Eof]
        );
    }

    #[test]
    fn trailing_underscore_in_number_is_error() {
        assert!(lex("1_\n").is_err());
        assert!(lex("1_.0\n").is_err());
    }

    #[test]
    fn maximal_munch_operators() {
        let ks = kinds("== != <= >= < > -> ** .. ..= . + - * / % = : , ? ?. ( ) [ ] { }\n");
        use TokenKind::*;
        let expected = vec![
            EqEq, NotEq, LtEq, GtEq, Lt, Gt, Arrow, StarStar, DotDot, DotDotEq, Dot, Plus, Minus,
            Star, Slash, Percent, Eq, Colon, Comma, Question, QuestionDot, LParen, RParen, LBracket,
            RBracket, LBrace, RBrace, Newline, Eof,
        ];
        assert_eq!(ks, expected);
    }

    #[test]
    fn star_vs_starstar() {
        use TokenKind::*;
        assert_eq!(
            kinds("a * b ** c\n"),
            vec![
                Name("a".into()),
                Star,
                Name("b".into()),
                StarStar,
                Name("c".into()),
                Newline,
                Eof,
            ]
        );
    }

    #[test]
    fn is_not_is_two_tokens() {
        use TokenKind::*;
        assert_eq!(
            kinds("x is not null\n"),
            vec![Name("x".into()), Is, Not, Null, Newline, Eof]
        );
    }

    #[test]
    fn string_text_and_interpolation() {
        // `"= {eval(program)}"` -> Str([Text("= "), Interp([eval ( program ) Eof])])
        let toks = lex("\"= {eval(program)}\"\n").unwrap();
        let str_tok = &toks[0];
        match &str_tok.kind {
            TokenKind::Str(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0], StrPart::Text("= ".into()));
                match &parts[1] {
                    StrPart::Interp(inner) => {
                        let inner_kinds: Vec<&TokenKind> = inner.iter().map(|t| &t.kind).collect();
                        use TokenKind::*;
                        let expected = vec![
                            Name("eval".into()),
                            LParen,
                            Name("program".into()),
                            RParen,
                            Eof,
                        ];
                        let expected_refs: Vec<&TokenKind> = expected.iter().collect();
                        assert_eq!(inner_kinds, expected_refs);
                    }
                    other => panic!("expected Interp, got {:?}", other),
                }
            }
            other => panic!("expected Str, got {:?}", other),
        }
        // Stream shape: Str Newline Eof.
        assert_eq!(toks[1].kind, TokenKind::Newline);
        assert_eq!(toks[2].kind, TokenKind::Eof);
    }

    #[test]
    fn nested_brackets_in_interpolation() {
        // `{f(g(x))}` must balance nested parens.
        let toks = lex("\"{f(g(x))}\"\n").unwrap();
        match &toks[0].kind {
            TokenKind::Str(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    StrPart::Interp(inner) => {
                        use TokenKind::*;
                        let got: Vec<TokenKind> = inner.iter().map(|t| t.kind.clone()).collect();
                        assert_eq!(
                            got,
                            vec![
                                Name("f".into()),
                                LParen,
                                Name("g".into()),
                                LParen,
                                Name("x".into()),
                                RParen,
                                RParen,
                                Eof,
                            ]
                        );
                    }
                    other => panic!("expected Interp, got {:?}", other),
                }
            }
            other => panic!("expected Str, got {:?}", other),
        }
    }

    #[test]
    fn brace_escapes() {
        // `"{{x}}"` -> Text("{x}")
        let toks = lex("\"{{x}}\"\n").unwrap();
        match &toks[0].kind {
            TokenKind::Str(parts) => {
                assert_eq!(parts, &vec![StrPart::Text("{x}".into())]);
            }
            other => panic!("expected Str, got {:?}", other),
        }
    }

    #[test]
    fn string_escapes_resolved() {
        let toks = lex("\"a\\nb\\t\\\"\\\\\\{x\\}\"\n").unwrap();
        match &toks[0].kind {
            TokenKind::Str(parts) => {
                assert_eq!(parts, &vec![StrPart::Text("a\nb\t\"\\{x}".into())]);
            }
            other => panic!("expected Str, got {:?}", other),
        }
    }

    #[test]
    fn unterminated_string_is_error() {
        assert!(lex("\"abc\n").is_err());
        assert!(lex("\"abc").is_err());
    }

    #[test]
    fn unbalanced_interpolation_is_error() {
        assert!(lex("\"{abc\"\n").is_err());
    }

    #[test]
    fn offside_rule_simple_block() {
        // if x:
        //     y
        // z
        let src = "if x:\n    y\nz\n";
        use TokenKind::*;
        assert_eq!(
            kinds(src),
            vec![
                If,
                Name("x".into()),
                Colon,
                Newline,
                Indent,
                Name("y".into()),
                Newline,
                Dedent,
                Name("z".into()),
                Newline,
                Eof,
            ]
        );
    }

    #[test]
    fn nested_blocks_dedent_to_eof() {
        // a:
        //     b:
        //         c
        let src = "a:\n    b:\n        c\n";
        use TokenKind::*;
        assert_eq!(
            kinds(src),
            vec![
                Name("a".into()),
                Colon,
                Newline,
                Indent,
                Name("b".into()),
                Colon,
                Newline,
                Indent,
                Name("c".into()),
                Newline,
                Dedent,
                Dedent,
                Eof,
            ]
        );
    }

    #[test]
    fn blank_and_comment_lines_no_layout() {
        // Blank lines and comment-only lines must not affect indentation or
        // produce Newline tokens.
        let src = "a\n\n# comment\n   # indented comment\nb\n";
        use TokenKind::*;
        assert_eq!(
            kinds(src),
            vec![
                Name("a".into()),
                Newline,
                Name("b".into()),
                Newline,
                Eof,
            ]
        );
    }

    #[test]
    fn tab_in_indent_is_error() {
        let src = "if x:\n\ty\n";
        let err = lex(src).unwrap_err();
        assert!(err.message.contains("tab"), "message was: {}", err.message);
    }

    #[test]
    fn bracket_continuation_suppresses_newline() {
        // A newline inside `(...)` does not produce a Newline token.
        let src = "f(a,\n  b)\n";
        use TokenKind::*;
        assert_eq!(
            kinds(src),
            vec![
                Name("f".into()),
                LParen,
                Name("a".into()),
                Comma,
                Name("b".into()),
                RParen,
                Newline,
                Eof,
            ]
        );
    }

    #[test]
    fn bracket_continuation_in_list() {
        let src = "[\n  1,\n  2,\n]\n";
        use TokenKind::*;
        assert_eq!(
            kinds(src),
            vec![
                LBracket,
                int(1),
                Comma,
                int(2),
                Comma,
                RBracket,
                Newline,
                Eof,
            ]
        );
    }

    #[test]
    fn inconsistent_dedent_is_error() {
        // Dedent to a level that was never opened.
        //   a:
        //         b
        //     c       <- 4 spaces, but open levels are {0, 8}
        let src = "a:\n        b\n    c\n";
        assert!(lex(src).is_err());
    }

    #[test]
    fn spans_track_line_and_col() {
        let toks = lex("ab\n  cd\n").unwrap();
        // ab at line 1 col 1
        assert_eq!(toks[0].span.line, 1);
        assert_eq!(toks[0].span.col, 1);
        assert_eq!(toks[0].span.start, 0);
        assert_eq!(toks[0].span.end, 2);
        // find `cd`
        let cd = toks
            .iter()
            .find(|t| matches!(&t.kind, TokenKind::Name(n) if n == "cd"))
            .unwrap();
        assert_eq!(cd.span.line, 2);
        assert_eq!(cd.span.col, 3);
    }

    #[test]
    fn no_trailing_newline_in_source() {
        // Source without a final newline still gets a synthetic Newline + Eof.
        use TokenKind::*;
        assert_eq!(
            kinds("x"),
            vec![Name("x".into()), Newline, Eof]
        );
    }

    #[test]
    fn empty_source() {
        use TokenKind::*;
        assert_eq!(kinds(""), vec![Eof]);
        assert_eq!(kinds("\n\n  \n"), vec![Eof]);
    }

    // ----- `##` doc-comment capture (§1.1) -------------------------------

    /// Find the doc attached to the first token of the given kind.
    fn doc_on(src: &str, want: &TokenKind) -> Option<String> {
        let toks = lex(src).expect("expected successful lex");
        toks.iter()
            .find(|t| &t.kind == want)
            .unwrap_or_else(|| panic!("no token {:?} in {:?}", want, src))
            .doc
            .clone()
    }

    #[test]
    fn doc_attaches_to_following_fn() {
        let doc = doc_on("## adds two numbers\nfn add():\n    1\n", &TokenKind::Fn);
        assert_eq!(doc.as_deref(), Some("adds two numbers"));
    }

    #[test]
    fn doc_strips_leading_hashes_and_one_space() {
        // Exactly one leading space after `##` is stripped; further spaces stay.
        let doc = doc_on("##  two spaces\nstruct S:\n    x: Int\n", &TokenKind::Struct);
        assert_eq!(doc.as_deref(), Some(" two spaces"));
        // No space at all after `##` is fine.
        let doc2 = doc_on("##nospace\nstruct S:\n    x: Int\n", &TokenKind::Struct);
        assert_eq!(doc2.as_deref(), Some("nospace"));
    }

    #[test]
    fn multiple_doc_lines_join_with_newline() {
        let doc = doc_on("## line one\n## line two\nenum E:\n    A\n", &TokenKind::Enum);
        assert_eq!(doc.as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn blank_line_detaches_doc() {
        let doc = doc_on("## orphaned\n\nfn f():\n    1\n", &TokenKind::Fn);
        assert_eq!(doc, None);
    }

    #[test]
    fn plain_comment_line_detaches_doc() {
        // A `##` block followed by a plain `#` line is no longer "immediately
        // above" the declaration, so it detaches.
        let doc = doc_on("## doc\n# plain\nfn f():\n    1\n", &TokenKind::Fn);
        assert_eq!(doc, None);
    }

    #[test]
    fn plain_comment_is_not_a_doc() {
        let doc = doc_on("# just a comment\nfn f():\n    1\n", &TokenKind::Fn);
        assert_eq!(doc, None);
    }

    #[test]
    fn doc_attaches_to_indented_decl_not_indent_token() {
        // Inside an `impl`, the doc must land on the inner `fn`, never on the
        // synthetic `Indent` emitted just before it.
        let src = "impl Foo:\n    ## a method\n    fn bar(self):\n        1\n";
        let toks = lex(src).expect("lex");
        // No layout token carries a doc.
        for t in &toks {
            if matches!(
                t.kind,
                TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent | TokenKind::Eof
            ) {
                assert_eq!(t.doc, None, "layout token must not carry a doc: {:?}", t);
            }
        }
        let fn_doc = toks
            .iter()
            .find(|t| t.kind == TokenKind::Fn)
            .unwrap()
            .doc
            .clone();
        assert_eq!(fn_doc.as_deref(), Some("a method"));
    }

    #[test]
    fn doc_above_non_declaration_lands_on_its_token() {
        // A `##` above a plain statement attaches to that statement's leading
        // token (the parser will simply ignore it). It must not leak to a later
        // declaration on the next line.
        let src = "## not a decl doc\nx = 1\nfn f():\n    1\n";
        let toks = lex(src).expect("lex");
        let x_doc = toks
            .iter()
            .find(|t| matches!(&t.kind, TokenKind::Name(n) if n == "x"))
            .unwrap()
            .doc
            .clone();
        assert_eq!(x_doc.as_deref(), Some("not a decl doc"));
        // The `fn` after a real content line gets nothing.
        let fn_doc = toks.iter().find(|t| t.kind == TokenKind::Fn).unwrap().doc.clone();
        assert_eq!(fn_doc, None);
    }

    #[test]
    fn doc_capture_does_not_perturb_layout() {
        // The token stream (kinds only) with a doc block must be identical to the
        // same program without it — docs are metadata, not structure.
        let with_doc = kinds("## d1\n## d2\nfn f():\n    1\n");
        let without = kinds("fn f():\n    1\n");
        assert_eq!(with_doc, without);
    }

    #[test]
    fn full_showcase_lexes() {
        let src = r#"## A tiny expression evaluator.

enum Expr:
    Num(Float)
    Add(Expr, Expr)
    Mul(Expr, Expr)
    Div(Expr, Expr)

fn eval(e: Expr) -> Float:
    return match e:
        Num(n):    n
        Add(a, b): eval(a) + eval(b)
        Mul(a, b): eval(a) * eval(b)
        Div(a, b):
            divisor = eval(b)
            if divisor == 0.0:
                panic("division by zero")
            eval(a) / divisor

fn main():
    # (1 + 2) * 3
    program = Mul(Add(Num(1.0), Num(2.0)), Num(3.0))
    print("= {eval(program)}")     # = 9.0
"#;
        let toks = lex(src).expect("showcase must lex cleanly");
        assert_eq!(toks.last().unwrap().kind, TokenKind::Eof);
        // Sanity: the interpolated string is present and well-formed.
        let has_interp = toks.iter().any(|t| matches!(&t.kind, TokenKind::Str(parts)
            if parts.iter().any(|p| matches!(p, StrPart::Interp(_)))));
        assert!(has_interp, "expected an interpolated string in showcase");
        // Indent/Dedent balance: equal counts.
        let indents = toks.iter().filter(|t| t.kind == TokenKind::Indent).count();
        let dedents = toks.iter().filter(|t| t.kind == TokenKind::Dedent).count();
        assert_eq!(indents, dedents, "Indent/Dedent must balance");
    }
