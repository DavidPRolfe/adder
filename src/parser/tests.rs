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
    fn safe_member_access() {
        // a?.b  -- a safe member access (`safe: true`)
        let e = parse_expr_tokens(vec![
            name("a"),
            t(TokenKind::QuestionDot),
            name("b"),
        ]);
        match &e.kind {
            ExprKind::Member { name, safe, base } => {
                assert_eq!(name, "b");
                assert!(*safe, "`?.` should set safe: true");
                assert!(matches!(&base.kind, ExprKind::Name(n) if n == "a"));
            }
            other => panic!("expected a safe member, got {:?}", other),
        }
    }

    #[test]
    fn safe_method_call() {
        // a?.b(c)  -- a Call whose callee is a safe Member
        let e = parse_expr_tokens(vec![
            name("a"),
            t(TokenKind::QuestionDot),
            name("b"),
            t(TokenKind::LParen),
            name("c"),
            t(TokenKind::RParen),
        ]);
        match &e.kind {
            ExprKind::Call { callee, args } => {
                assert_eq!(args.len(), 1);
                match &callee.kind {
                    ExprKind::Member { name, safe, .. } => {
                        assert_eq!(name, "b");
                        assert!(*safe, "`?.m(...)` callee should be a safe Member");
                    }
                    other => panic!("expected safe member callee, got {:?}", other),
                }
            }
            other => panic!("expected a call, got {:?}", other),
        }
    }

    #[test]
    fn safe_chain_nests_left_to_right() {
        // a?.b?.c  -- the inner `a?.b` is the base of the outer `?.c`
        let e = parse_expr_tokens(vec![
            name("a"),
            t(TokenKind::QuestionDot),
            name("b"),
            t(TokenKind::QuestionDot),
            name("c"),
        ]);
        match &e.kind {
            ExprKind::Member { name, safe, base } => {
                assert_eq!(name, "c");
                assert!(*safe);
                assert!(matches!(
                    &base.kind,
                    ExprKind::Member { name, safe: true, .. } if name == "b"
                ));
            }
            other => panic!("expected an outer safe member, got {:?}", other),
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

    // =====================================================================
    // Collections, comprehensions, tuple binders, function/tuple types,
    // default + named args. These go through the real lexer.
    // =====================================================================

    /// Lex + parse real source, returning the parse diagnostics (asserting it
    /// lexed and the parse *failed*).
    fn parse_src_err(src: &str) -> Vec<Diagnostic> {
        let toks = lex(src).expect("source should lex");
        match parse(&toks) {
            Ok(p) => panic!("expected parse error, got program: {:?}", p),
            Err(e) => e,
        }
    }

    /// Pull the single expr-statement expression out of a one-line program.
    fn expr_of(src: &str) -> Expr {
        let p = parse_src(src);
        match &only_stmt(&p).kind {
            StmtKind::Expr(e) => e.clone(),
            // A bare binding/expr: unwrap a `val x = <expr>` initializer.
            StmtKind::Binding(b) => b.value.clone(),
            other => panic!("expected an expr/binding statement, got {:?}", other),
        }
    }

    // ----- brace literals: map / set / empty -----------------------------

    #[test]
    fn empty_braces_is_empty_map() {
        match expr_of("val m = {}\n").kind {
            ExprKind::Map(pairs) => assert!(pairs.is_empty()),
            other => panic!("expected empty Map, got {:?}", other),
        }
    }

    #[test]
    fn map_literal_pairs() {
        match expr_of("val m = {1: 2, 3: 4}\n").kind {
            ExprKind::Map(pairs) => assert_eq!(pairs.len(), 2),
            other => panic!("expected Map, got {:?}", other),
        }
    }

    #[test]
    fn set_literal_elements() {
        match expr_of("val s = {1, 2, 3}\n").kind {
            ExprKind::Set(items) => assert_eq!(items.len(), 3),
            other => panic!("expected Set, got {:?}", other),
        }
    }

    // ----- tuples vs grouping --------------------------------------------

    #[test]
    fn tuple_literal_two_or_more() {
        match expr_of("val t = (1, 2, 3)\n").kind {
            ExprKind::Tuple(items) => assert_eq!(items.len(), 3),
            other => panic!("expected Tuple, got {:?}", other),
        }
    }

    #[test]
    fn single_paren_is_grouping_not_tuple() {
        // `(1 + 2)` is grouping: the inner Binary survives, not a 1-tuple.
        match expr_of("val g = (1 + 2)\n").kind {
            ExprKind::Binary { .. } => {}
            other => panic!("expected grouped Binary, got {:?}", other),
        }
    }

    #[test]
    fn trailing_comma_tuple() {
        match expr_of("val t = (1, 2,)\n").kind {
            ExprKind::Tuple(items) => assert_eq!(items.len(), 2),
            other => panic!("expected Tuple, got {:?}", other),
        }
    }

    // ----- comprehensions ------------------------------------------------

    #[test]
    fn list_comprehension_with_filter() {
        let c = match expr_of("val xs = [x * x for x in 1..=5 if x != 3]\n").kind {
            ExprKind::Comprehension(c) => c,
            other => panic!("expected Comprehension, got {:?}", other),
        };
        assert!(matches!(c.output, ComprehensionOutput::List(_)));
        assert!(matches!(c.binder, ComprehensionBinder::Name(ref n) if n == "x"));
        assert!(c.cond.is_some());
    }

    #[test]
    fn set_comprehension() {
        match expr_of("val s = {x % 3 for x in xs}\n").kind {
            ExprKind::Comprehension(c) => {
                assert!(matches!(c.output, ComprehensionOutput::Set(_)));
                assert!(c.cond.is_none());
            }
            other => panic!("expected Comprehension, got {:?}", other),
        }
    }

    #[test]
    fn map_comprehension() {
        match expr_of("val m = {k: k for k in ks}\n").kind {
            ExprKind::Comprehension(c) => {
                assert!(matches!(c.output, ComprehensionOutput::Map { .. }));
            }
            other => panic!("expected Comprehension, got {:?}", other),
        }
    }

    #[test]
    fn comprehension_tuple_binder() {
        match expr_of("val xs = [a for (a, b) in pairs]\n").kind {
            ExprKind::Comprehension(c) => {
                assert!(matches!(c.binder, ComprehensionBinder::Tuple(ref ns) if ns == &["a", "b"]));
            }
            other => panic!("expected Comprehension, got {:?}", other),
        }
    }

    // ----- function & tuple types ----------------------------------------

    #[test]
    fn function_type_param() {
        // fn apply(f: (Int) -> Int, x: Int) -> Int: f(x)
        let p = parse_src("fn apply(f: (Int) -> Int, x: Int) -> Int:\n    f(x)\n");
        let f = match &only_stmt(&p).kind {
            StmtKind::Fn(f) => f.clone(),
            other => panic!("expected fn, got {:?}", other),
        };
        match &f.params[0] {
            Param::Named { ty, .. } => match &ty.base {
                BaseType::Fn { params, ret } => {
                    assert_eq!(params.len(), 1);
                    assert!(matches!(&ret.base, BaseType::Named { name, .. } if name == "Int"));
                }
                other => panic!("expected Fn type, got {:?}", other),
            },
            other => panic!("expected named param, got {:?}", other),
        }
    }

    #[test]
    fn zero_arg_function_type() {
        let p = parse_src("fn run(f: () -> Int):\n    f()\n");
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => match &f.params[0] {
                Param::Named { ty, .. } => {
                    assert!(matches!(&ty.base, BaseType::Fn { params, .. } if params.is_empty()));
                }
                other => panic!("expected named param, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn tuple_type_annotation() {
        let p = parse_src("fn f(p: (Int, String)):\n    p\n");
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => match &f.params[0] {
                Param::Named { ty, .. } => {
                    assert!(matches!(&ty.base, BaseType::Tuple(c) if c.len() == 2));
                }
                other => panic!("expected named param, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    // ----- default + named call args -------------------------------------

    #[test]
    fn param_default_value() {
        let p = parse_src("fn greet(name: String, greeting: String = \"Hi\"):\n    name\n");
        match &only_stmt(&p).kind {
            StmtKind::Fn(f) => match &f.params[1] {
                Param::Named { default, .. } => assert!(default.is_some()),
                other => panic!("expected named param, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn named_call_arg_parses() {
        // A named arg in an ordinary call site parses to Arg::Named.
        match expr_of("val r = greet(\"Ada\", greeting: \"Hi\")\n").kind {
            ExprKind::Call { args, .. } => {
                assert!(matches!(&args[1], Arg::Named { name, .. } if name == "greeting"));
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    // ----- tuple binders in val / for ------------------------------------

    #[test]
    fn val_tuple_binder() {
        let p = parse_src("val (a, b) = pair\n");
        match &only_stmt(&p).kind {
            StmtKind::Binding(b) => {
                assert!(matches!(&b.binder, Binder::Tuple(ns) if ns == &["a", "b"]));
                assert!(b.is_val);
            }
            other => panic!("expected binding, got {:?}", other),
        }
    }

    #[test]
    fn for_tuple_binder() {
        let p = parse_src("for (k, v) in items:\n    k\n");
        match &only_stmt(&p).kind {
            StmtKind::For(f) => {
                assert!(matches!(&f.binder, Binder::Tuple(ns) if ns == &["k", "v"]));
                assert_eq!(f.var, "k"); // label mirrors the first name
            }
            other => panic!("expected for, got {:?}", other),
        }
    }

    #[test]
    fn one_name_tuple_binder_rejected() {
        // `val (a) = …` is not a tuple binder (needs ≥2 names) — a parse error.
        let errs = parse_src_err("val (a) = x\n");
        assert!(!errs.is_empty());
    }

    // =====================================================================
    // Match guards, or-patterns, tuple / nested patterns.
    // =====================================================================

    /// Pull the arms of a `match` out of a one-binding program
    /// (`val r = match …`).
    fn match_arms_of(src: &str) -> Vec<MatchArm> {
        match expr_of(src).kind {
            ExprKind::Match(m) => m.arms,
            other => panic!("expected a match expr, got {:?}", other),
        }
    }

    #[test]
    fn match_guard_parses() {
        // A guarded arm records its `if cond` between the pattern and the `:`.
        let arms = match_arms_of("val r = match n:\n    x if x > 0: 1\n    _: 0\n");
        assert!(matches!(&arms[0].pattern.kind, PatternKind::Binding(n) if n == "x"));
        assert!(arms[0].guard.is_some(), "first arm should be guarded");
        assert!(arms[1].guard.is_none(), "wildcard arm is unguarded");
    }

    #[test]
    fn or_pattern_of_literals() {
        // `1 or 2 or 3` is a single or-pattern with three alternatives.
        let arms = match_arms_of("val r = match n:\n    1 or 2 or 3: 1\n    _: 0\n");
        match &arms[0].pattern.kind {
            PatternKind::Or(alts) => {
                assert_eq!(alts.len(), 3);
                assert!(alts
                    .iter()
                    .all(|a| matches!(a.kind, PatternKind::Literal(_))));
            }
            other => panic!("expected an or-pattern, got {:?}", other),
        }
    }

    #[test]
    fn or_pattern_of_variants() {
        // `.A or .B` covers two leading-dot variant alternatives.
        let arms = match_arms_of("val r = match c:\n    .A or .B: 1\n    _: 0\n");
        match &arms[0].pattern.kind {
            PatternKind::Or(alts) => {
                assert_eq!(alts.len(), 2);
                assert!(matches!(
                    &alts[0].kind,
                    PatternKind::Variant { name, .. } if name == "A"
                ));
                assert!(matches!(
                    &alts[1].kind,
                    PatternKind::Variant { name, .. } if name == "B"
                ));
            }
            other => panic!("expected an or-pattern, got {:?}", other),
        }
    }

    #[test]
    fn tuple_pattern_parses() {
        // `(a, b)` destructures a pair element-wise.
        let arms = match_arms_of("val r = match p:\n    (a, b): 1\n    _: 0\n");
        match &arms[0].pattern.kind {
            PatternKind::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(&elems[0].kind, PatternKind::Binding(n) if n == "a"));
            }
            other => panic!("expected a tuple pattern, got {:?}", other),
        }
    }

    #[test]
    fn grouped_pattern_is_transparent() {
        // `(p)` with no comma is grouping, not a 1-tuple.
        let arms = match_arms_of("val r = match p:\n    (a): 1\n    _: 0\n");
        assert!(matches!(&arms[0].pattern.kind, PatternKind::Binding(n) if n == "a"));
    }

    #[test]
    fn nested_variant_subpattern() {
        // A variant sub-pattern may itself be a variant pattern.
        let arms = match_arms_of("val r = match v:\n    .Some(.Pair(a, b)): 1\n    _: 0\n");
        match &arms[0].pattern.kind {
            PatternKind::Variant { name, subs, .. } => {
                assert_eq!(name, "Some");
                assert_eq!(subs.len(), 1);
                assert!(matches!(
                    &subs[0].kind,
                    PatternKind::Variant { name, .. } if name == "Pair"
                ));
            }
            other => panic!("expected a nested variant pattern, got {:?}", other),
        }
    }

    #[test]
    fn or_pattern_inside_variant_sub() {
        // An or-pattern nests inside a variant sub-pattern: `.Tag(1 or 2)`.
        let arms = match_arms_of("val r = match v:\n    .Tag(1 or 2): 1\n    _: 0\n");
        match &arms[0].pattern.kind {
            PatternKind::Variant { subs, .. } => {
                assert_eq!(subs.len(), 1);
                assert!(matches!(&subs[0].kind, PatternKind::Or(alts) if alts.len() == 2));
            }
            other => panic!("expected a variant pattern, got {:?}", other),
        }
    }
