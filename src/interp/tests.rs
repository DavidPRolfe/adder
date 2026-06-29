//! Unit tests for the interpreter (moved verbatim from the original
//! `interp.rs` `#[cfg(test)] mod tests` block).

    use super::*;
    use crate::ast::*;
    // `ast::Binding` collides with the runtime `super::Binding`; in tests we
    // only ever build the AST node, so import it explicitly to disambiguate.
    use crate::ast::Binding;
    use num_bigint::BigInt;

    // ---- tiny AST constructors -------------------------------------------

    fn sp() -> Span {
        Span::dummy()
    }

    fn ex(kind: ExprKind) -> Expr {
        Expr { kind, span: sp() }
    }

    fn int(n: i64) -> Expr {
        ex(ExprKind::Int(BigInt::from(n)))
    }

    fn float(f: f64) -> Expr {
        ex(ExprKind::Float(f))
    }

    fn boolean(b: bool) -> Expr {
        ex(ExprKind::Bool(b))
    }

    fn name(n: &str) -> Expr {
        ex(ExprKind::Name(n.to_string()))
    }

    fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
        ex(ExprKind::Binary { op, lhs: Box::new(l), rhs: Box::new(r) })
    }

    fn st(kind: StmtKind) -> Stmt {
        Stmt { kind, span: sp() }
    }

    fn expr_stmt(e: Expr) -> Stmt {
        st(StmtKind::Expr(e))
    }

    fn block(stmts: Vec<Stmt>) -> Block {
        Block { stmts, span: sp() }
    }

    /// Build a fresh interpreter + root env with the prelude seeded.
    ///
    /// Output is discarded into a leaked `Sink` so the returned `Interp` can be
    /// `'static` and every call site stays `let (mut interp, root) = fresh();`.
    /// These tests evaluate expressions / statements directly and never assert on
    /// `print` output, so a discarding sink is sufficient.
    fn fresh() -> (Interp<'static>, Env) {
        let out: &'static mut std::io::Sink = Box::leak(Box::new(std::io::sink()));
        let interp = Interp { registry: Registry::default(), out, propagating: None };
        let root = Scope::new_root();
        seed_prelude(&root);
        (interp, root)
    }

    /// Evaluate a single expression in a fresh env.
    fn eval_expr(e: &Expr) -> EvalResult {
        let (mut interp, root) = fresh();
        interp.eval(e, &root)
    }

    /// Lex + parse + run a source program, then evaluate its **final**
    /// expression statement, returning that value. Earlier statements run for
    /// their effects (bindings, fn decls). Used by the surface tests, which
    /// are far clearer as source than as hand-built AST.
    fn eval_src(src: &str) -> Value {
        let toks = crate::lexer::lex(src).expect("source should lex");
        let program = crate::parser::parse(&toks).expect("source should parse");
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let (last, init) = program
            .stmts
            .split_last()
            .expect("program should have at least one statement");
        for stmt in init {
            match interp.exec_stmt(stmt, &root) {
                Ok(_) => {}
                Err(d) => panic!("setup statement failed: {}", d.message),
            }
        }
        match &last.kind {
            StmtKind::Expr(e) => interp.eval(e, &root).expect("final expr should evaluate"),
            other => panic!("expected a trailing expr statement, got {:?}", other),
        }
    }

    /// Like [`eval_src`], but expect the **final** expression to fail at runtime;
    /// return the diagnostic. Earlier statements must still succeed.
    fn eval_src_err(src: &str) -> Diagnostic {
        let toks = crate::lexer::lex(src).expect("source should lex");
        let program = crate::parser::parse(&toks).expect("source should parse");
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let (last, init) = program
            .stmts
            .split_last()
            .expect("program should have at least one statement");
        for stmt in init {
            match interp.exec_stmt(stmt, &root) {
                Ok(_) => {}
                Err(d) => panic!("setup statement failed: {}", d.message),
            }
        }
        match &last.kind {
            StmtKind::Expr(e) => interp
                .eval(e, &root)
                .expect_err("final expr should fail at runtime"),
            other => panic!("expected a trailing expr statement, got {:?}", other),
        }
    }

    // ---- arithmetic -------------------------------------------------------

    #[test]
    fn int_arithmetic() {
        // 2 + 3 * 4 — precedence is encoded by the AST shape.
        let e = bin(BinOp::Add, int(2), bin(BinOp::Mul, int(3), int(4)));
        match eval_expr(&e).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(14)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    #[test]
    fn power_right_assoc_eval() {
        // 2 ** (3 ** 2) = 2 ** 9 = 512 — the AST already encodes right-assoc.
        let e = bin(BinOp::Pow, int(2), bin(BinOp::Pow, int(3), int(2)));
        match eval_expr(&e).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(512)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    #[test]
    fn float_true_division() {
        // 9.0 / 2.0 = 4.5
        let e = bin(BinOp::Div, float(9.0), float(2.0));
        match eval_expr(&e).unwrap() {
            Value::Float(f) => assert_eq!(f, 4.5),
            v => panic!("expected Float, got {:?}", v),
        }
    }

    #[test]
    fn int_division_exact_ok_inexact_errs() {
        // 6 / 3 = 2 (exact)
        let ok = bin(BinOp::Div, int(6), int(3));
        match eval_expr(&ok).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(2)),
            v => panic!("expected Int, got {:?}", v),
        }
        // 7 / 2 is not exact -> error.
        let bad = bin(BinOp::Div, int(7), int(2));
        assert!(eval_expr(&bad).is_err());
    }

    #[test]
    fn division_by_zero_errs() {
        let e = bin(BinOp::Div, int(1), int(0));
        assert!(eval_expr(&e).is_err());
    }

    // ---- short-circuit logic ---------------------------------------------

    #[test]
    fn and_short_circuits() {
        // false and <undefined name>  -> false, never touches rhs.
        let e = bin(BinOp::And, boolean(false), name("nope"));
        match eval_expr(&e).unwrap() {
            Value::Bool(b) => assert!(!b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    #[test]
    fn or_short_circuits() {
        // true or <undefined name>  -> true.
        let e = bin(BinOp::Or, boolean(true), name("nope"));
        match eval_expr(&e).unwrap() {
            Value::Bool(b) => assert!(b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    #[test]
    fn and_requires_bool() {
        // 1 and true -> error (non-Bool operand).
        let e = bin(BinOp::And, int(1), boolean(true));
        assert!(eval_expr(&e).is_err());
    }

    // ---- conditions must be Bool -----------------------------------------

    #[test]
    fn non_bool_condition_errs() {
        // if 1: print(...) else: ...  — condition is Int -> runtime error.
        let if_stmt = st(StmtKind::If(IfStmt {
            arms: vec![(int(1), block(vec![expr_stmt(int(0))]))],
            else_body: None,
        }));
        let (mut interp, root) = fresh();
        let r = interp.exec_stmt(&if_stmt, &root);
        assert!(r.is_err());
    }

    #[test]
    fn ternary_non_bool_errs() {
        // (1 if 5 else 2) — cond is Int.
        let e = ex(ExprKind::Ternary {
            then: Box::new(int(1)),
            cond: Box::new(int(5)),
            otherwise: Box::new(int(2)),
        });
        assert!(eval_expr(&e).is_err());
    }

    // ---- structural equality ---------------------------------------------

    #[test]
    fn structural_eq_on_lists() {
        let l1 = ex(ExprKind::List(vec![int(1), int(2), int(3)]));
        let l2 = ex(ExprKind::List(vec![int(1), int(2), int(3)]));
        let e = bin(BinOp::Eq, l1, l2);
        match eval_expr(&e).unwrap() {
            Value::Bool(b) => assert!(b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    #[test]
    fn structural_eq_on_structs() {
        // Build two Point structs by hand and compare with values_equal.
        let mut f1 = HashMap::new();
        f1.insert("x".to_string(), Value::Int(BigInt::from(1)));
        f1.insert("y".to_string(), Value::Int(BigInt::from(2)));
        let s1 = Value::Struct(Rc::new(RefCell::new(StructInstance {
            type_name: "Point".to_string(),
            fields: f1,
            field_order: vec!["x".to_string(), "y".to_string()],
        })));

        let mut f2 = HashMap::new();
        f2.insert("x".to_string(), Value::Int(BigInt::from(1)));
        f2.insert("y".to_string(), Value::Int(BigInt::from(2)));
        let s2 = Value::Struct(Rc::new(RefCell::new(StructInstance {
            type_name: "Point".to_string(),
            fields: f2,
            field_order: vec!["x".to_string(), "y".to_string()],
        })));

        assert!(values_equal(&s1, &s2));

        // Differ a field -> not equal.
        if let Value::Struct(s) = &s2 {
            s.borrow_mut()
                .fields
                .insert("y".to_string(), Value::Int(BigInt::from(99)));
        }
        assert!(!values_equal(&s1, &s2));
    }

    #[test]
    fn is_not_value_inequality() {
        let e = bin(BinOp::IsNot, int(1), int(2));
        match eval_expr(&e).unwrap() {
            Value::Bool(b) => assert!(b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    // ---- val reassignment -------------------------------------------------

    #[test]
    fn val_reassignment_errs() {
        // val x = 1 ; x = 2  -> runtime error on the reassignment.
        let bind = st(StmtKind::Binding(Binding {
            name: "x".to_string(),
            binder: Binder::Name("x".to_string()),
            is_val: true,
            ty: None,
            value: int(1),
        }));
        let reassign = st(StmtKind::Assign(Assign {
            target: Target { base: "x".to_string(), path: vec![], span: sp() },
            value: int(2),
        }));
        let (mut interp, root) = fresh();
        interp.exec_stmt(&bind, &root).unwrap();
        let r = interp.exec_stmt(&reassign, &root);
        assert!(r.is_err());
    }

    #[test]
    fn mutable_reassignment_ok() {
        let bind = st(StmtKind::Binding(Binding {
            name: "x".to_string(),
            binder: Binder::Name("x".to_string()),
            is_val: false,
            ty: None,
            value: int(1),
        }));
        let reassign = st(StmtKind::Assign(Assign {
            target: Target { base: "x".to_string(), path: vec![], span: sp() },
            value: int(2),
        }));
        let (mut interp, root) = fresh();
        interp.exec_stmt(&bind, &root).unwrap();
        interp.exec_stmt(&reassign, &root).unwrap();
        match env_get(&root, "x").unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(2)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    // ---- for loop ---------------------------------------------------------

    #[test]
    fn for_loop_accumulates_over_range() {
        // sum = 0 ; for x in 0..5: sum = sum + x   -> 0+1+2+3+4 = 10
        let init = st(StmtKind::Binding(Binding {
            name: "sum".to_string(),
            binder: Binder::Name("sum".to_string()),
            is_val: false,
            ty: None,
            value: int(0),
        }));
        let for_stmt = st(StmtKind::For(ForStmt {
            var: "x".to_string(),
            binder: Binder::Name("x".to_string()),
            iter: bin(BinOp::Range, int(0), int(5)),
            body: block(vec![st(StmtKind::Assign(Assign {
                target: Target { base: "sum".to_string(), path: vec![], span: sp() },
                value: bin(BinOp::Add, name("sum"), name("x")),
            }))]),
        }));
        let (mut interp, root) = fresh();
        interp.exec_stmt(&init, &root).unwrap();
        interp.exec_stmt(&for_stmt, &root).unwrap();
        match env_get(&root, "sum").unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(10)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    #[test]
    fn for_loop_inclusive_range() {
        // for x in 0..=3 -> [0,1,2,3], sum 6
        let init = st(StmtKind::Binding(Binding {
            name: "sum".to_string(),
            binder: Binder::Name("sum".to_string()),
            is_val: false,
            ty: None,
            value: int(0),
        }));
        let for_stmt = st(StmtKind::For(ForStmt {
            var: "x".to_string(),
            binder: Binder::Name("x".to_string()),
            iter: bin(BinOp::RangeIncl, int(0), int(3)),
            body: block(vec![st(StmtKind::Assign(Assign {
                target: Target { base: "sum".to_string(), path: vec![], span: sp() },
                value: bin(BinOp::Add, name("sum"), name("x")),
            }))]),
        }));
        let (mut interp, root) = fresh();
        interp.exec_stmt(&init, &root).unwrap();
        interp.exec_stmt(&for_stmt, &root).unwrap();
        match env_get(&root, "sum").unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(6)),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    // ---- match over a hand-built enum ------------------------------------

    #[test]
    fn match_enum_variant_returns_arm() {
        // enum E: A  B(Int) ; match B(7): A: 0 ; B(n): n  -> 7
        let scrut = Value::Enum(Rc::new(EnumInstance {
            enum_name: "E".to_string(),
            variant: "B".to_string(),
            payload: vec![Value::Int(BigInt::from(7))],
            payload_names: vec![],
        }));

        let m = MatchExpr {
            scrutinee: Box::new(int(0)), // placeholder; we'll match a value directly
            arms: vec![
                MatchArm {
                    pattern: Pattern {
                        kind: PatternKind::Variant {
                            enum_name: None,
                            name: "A".to_string(),
                            subs: vec![],
                        },
                        span: sp(),
                    },
                    guard: None,
                    body: block(vec![expr_stmt(int(0))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: Pattern {
                        kind: PatternKind::Variant {
                            enum_name: None,
                            name: "B".to_string(),
                            subs: vec![Pattern {
                                kind: PatternKind::Binding("n".to_string()),
                                span: sp(),
                            }],
                        },
                        span: sp(),
                    },
                    guard: None,
                    body: block(vec![expr_stmt(name("n"))]),
                    span: sp(),
                },
            ],
        };

        // Drive eval_match by binding the scrutinee into a name and matching it.
        let (mut interp, root) = fresh();
        env_define(&root, "scrut", scrut, false);
        let m = MatchExpr { scrutinee: Box::new(name("scrut")), ..m };
        let v = interp.eval_match(&m, sp(), &root).unwrap();
        match v {
            Value::Int(n) => assert_eq!(n, BigInt::from(7)),
            v => panic!("expected Int(7), got {:?}", v),
        }
    }

    #[test]
    fn match_wildcard_fallback() {
        let scrut = Value::Int(BigInt::from(42));
        let m = MatchExpr {
            scrutinee: Box::new(name("scrut")),
            arms: vec![
                MatchArm {
                    pattern: Pattern {
                        kind: PatternKind::Literal(LitPattern::Int(BigInt::from(1))),
                        span: sp(),
                    },
                    guard: None,
                    body: block(vec![expr_stmt(int(100))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: Pattern { kind: PatternKind::Wildcard, span: sp() },
                    guard: None,
                    body: block(vec![expr_stmt(int(999))]),
                    span: sp(),
                },
            ],
        };
        let (mut interp, root) = fresh();
        env_define(&root, "scrut", scrut, false);
        match interp.eval_match(&m, sp(), &root).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(999)),
            v => panic!("expected Int(999), got {:?}", v),
        }
    }

    // ---- Show rendering ---------------------------------------------------

    #[test]
    fn show_float_has_decimal_point() {
        assert_eq!(show(&Value::Float(9.0)), "9.0");
        assert_eq!(show(&Value::Float(2.5)), "2.5");
        assert_eq!(show(&Value::Float(-1.0)), "-1.0");
    }

    #[test]
    fn show_int_and_bool_and_null() {
        assert_eq!(show(&Value::Int(BigInt::from(42))), "42");
        assert_eq!(show(&Value::Bool(true)), "true");
        assert_eq!(show(&Value::Bool(false)), "false");
        assert_eq!(show(&Value::Null), "null");
        assert_eq!(show(&Value::Unit), "()");
    }

    #[test]
    fn show_struct() {
        let mut fields = HashMap::new();
        fields.insert("x".to_string(), Value::Int(BigInt::from(1)));
        fields.insert("y".to_string(), Value::Float(2.0));
        let s = Value::Struct(Rc::new(RefCell::new(StructInstance {
            type_name: "Point".to_string(),
            fields,
            field_order: vec!["x".to_string(), "y".to_string()],
        })));
        assert_eq!(show(&s), "Point(x: 1, y: 2.0)");
    }

    #[test]
    fn show_enum() {
        let e = Value::Enum(Rc::new(EnumInstance {
            enum_name: "Expr".to_string(),
            variant: "Num".to_string(),
            payload: vec![Value::Float(1.0)],
            payload_names: vec![],
        }));
        assert_eq!(show(&e), "Num(1.0)");

        let empty = Value::Enum(Rc::new(EnumInstance {
            enum_name: "E".to_string(),
            variant: "Empty".to_string(),
            payload: vec![],
            payload_names: vec![],
        }));
        assert_eq!(show(&empty), "Empty");
    }

    #[test]
    fn show_list() {
        let l = Value::List(Rc::new(RefCell::new(vec![
            Value::Int(BigInt::from(1)),
            Value::Int(BigInt::from(2)),
        ])));
        assert_eq!(show(&l), "[1, 2]");
    }

    // ---- panic ------------------------------------------------------------

    #[test]
    fn panic_produces_err() {
        let (mut interp, root) = fresh();
        let panic_call = ex(ExprKind::Call {
            callee: Box::new(name("panic")),
            args: vec![Arg::Positional(ex(ExprKind::Str(StringLit {
                parts: vec![StrSeg::Text("boom".to_string())],
            })))],
        });
        let r = interp.eval(&panic_call, &root);
        assert!(r.is_err());
        let d = r.unwrap_err();
        assert!(d.message.contains("boom"));
    }

    // ---- calling a hand-built main + functions ---------------------------

    #[test]
    fn run_program_calls_main() {
        // fn main(): x = 1 ; (no assertion on stdout, just that it runs clean)
        let main = FnDecl {
            name: "main".to_string(),
            type_params: vec![],
            params: vec![],
            returns: None,
            body: block(vec![st(StmtKind::Binding(Binding {
                name: "x".to_string(),
                binder: Binder::Name("x".to_string()),
                is_val: false,
                ty: None,
                value: int(1),
            }))]),
            doc: None,
            span: sp(),
        };
        let program = Program { stmts: vec![st(StmtKind::Fn(main))] };
        assert!(run(&program, &mut std::io::sink()).is_ok());
    }

    #[test]
    fn function_call_and_implicit_return() {
        // fn double(n: Int) returns Int: n + n
        // double(21) -> 42
        let double = FnDecl {
            name: "double".to_string(),
            type_params: vec![],
            params: vec![Param::Named {
                name: "n".to_string(),
                ty: Type {
                    base: BaseType::Named { name: "Int".to_string(), args: vec![] },
                    nullable: false,
                    span: sp(),
                },
                default: None,
            }],
            returns: None,
            body: block(vec![expr_stmt(bin(BinOp::Add, name("n"), name("n")))]),
            doc: None,
            span: sp(),
        };
        let (mut interp, root) = fresh();
        interp.exec_stmt(&st(StmtKind::Fn(double)), &root).unwrap();
        let call = ex(ExprKind::Call {
            callee: Box::new(name("double")),
            args: vec![Arg::Positional(int(21))],
        });
        match interp.eval(&call, &root).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(42)),
            v => panic!("expected Int(42), got {:?}", v),
        }
    }

    #[test]
    fn or_else_defaults_null() {
        // null.or_else(5) -> 5 ; 3.or_else(5) -> 3
        let (mut interp, root) = fresh();
        let e1 = ex(ExprKind::Call {
            callee: Box::new(ex(ExprKind::Member {
                base: Box::new(ex(ExprKind::Null)),
                name: "or_else".to_string(),
                safe: false,
            })),
            args: vec![Arg::Positional(int(5))],
        });
        match interp.eval(&e1, &root).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("expected Int(5), got {:?}", v),
        }
        let e2 = ex(ExprKind::Call {
            callee: Box::new(ex(ExprKind::Member {
                base: Box::new(int(3)),
                name: "or_else".to_string(),
                safe: false,
            })),
            args: vec![Arg::Positional(int(5))],
        });
        match interp.eval(&e2, &root).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("expected Int(3), got {:?}", v),
        }
    }

    // ---- the showcase, built by hand ---------------------------------

    /// Reconstructs the showcase evaluator as a hand-built `Program` and
    /// confirms `eval((1+2)*3)` is `9.0` and `Show`s as `"9.0"`.
    ///
    /// Exercises the interpreter end to end: enums with data, recursion, an
    /// exhaustive `match` (with a `panic` guard arm), `fn` signatures,
    /// `val`-style bindings, and float arithmetic together.
    #[test]
    fn showcase_evaluator_yields_9_0() {
        // Helpers to spell out the AST tersely.
        fn ty_float() -> Type {
            Type {
                base: BaseType::Named { name: "Float".to_string(), args: vec![] },
                nullable: false,
                span: sp(),
            }
        }
        fn variant_pat(name: &str, subs: Vec<&str>) -> Pattern {
            Pattern {
                kind: PatternKind::Variant {
                    enum_name: None,
                    name: name.to_string(),
                    subs: subs
                        .into_iter()
                        .map(|s| Pattern {
                            kind: PatternKind::Binding(s.to_string()),
                            span: sp(),
                        })
                        .collect(),
                },
                span: sp(),
            }
        }
        fn call(callee: Expr, args: Vec<Expr>) -> Expr {
            ex(ExprKind::Call {
                callee: Box::new(callee),
                args: args.into_iter().map(Arg::Positional).collect(),
            })
        }

        // enum Expr: Num(Float)  Add(Expr,Expr)  Mul(Expr,Expr)  Div(Expr,Expr)
        let expr_enum = EnumDecl {
            name: "Expr".to_string(),
            type_params: vec![],
            derives: vec![],
            variants: vec![
                VariantDecl {
                    name: "Num".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float()])),
                    doc: None,
                    span: sp(),
                },
                VariantDecl {
                    name: "Add".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float(), ty_float()])),
                    doc: None,
                    span: sp(),
                },
                VariantDecl {
                    name: "Mul".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float(), ty_float()])),
                    doc: None,
                    span: sp(),
                },
                VariantDecl {
                    name: "Div".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float(), ty_float()])),
                    doc: None,
                    span: sp(),
                },
            ],
            doc: None,
            span: sp(),
        };

        // fn eval(e: Expr) returns Float:
        //     return match e:
        //         Num(n):    n
        //         Add(a, b): eval(a) + eval(b)
        //         Mul(a, b): eval(a) * eval(b)
        //         Div(a, b):
        //             divisor = eval(b)
        //             if divisor == 0.0: panic("division by zero")
        //             eval(a) / divisor
        let match_expr = ex(ExprKind::Match(MatchExpr {
            scrutinee: Box::new(name("e")),
            arms: vec![
                MatchArm {
                    pattern: variant_pat("Num", vec!["n"]),
                    guard: None,
                    body: block(vec![expr_stmt(name("n"))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Add", vec!["a", "b"]),
                    guard: None,
                    body: block(vec![expr_stmt(bin(
                        BinOp::Add,
                        call(name("eval"), vec![name("a")]),
                        call(name("eval"), vec![name("b")]),
                    ))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Mul", vec!["a", "b"]),
                    guard: None,
                    body: block(vec![expr_stmt(bin(
                        BinOp::Mul,
                        call(name("eval"), vec![name("a")]),
                        call(name("eval"), vec![name("b")]),
                    ))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Div", vec!["a", "b"]),
                    guard: None,
                    body: block(vec![
                        st(StmtKind::Binding(Binding {
                            name: "divisor".to_string(),
                            binder: Binder::Name("divisor".to_string()),
                            is_val: false,
                            ty: None,
                            value: call(name("eval"), vec![name("b")]),
                        })),
                        st(StmtKind::If(IfStmt {
                            arms: vec![(
                                bin(BinOp::Eq, name("divisor"), float(0.0)),
                                block(vec![expr_stmt(call(
                                    name("panic"),
                                    vec![ex(ExprKind::Str(StringLit {
                                        parts: vec![StrSeg::Text(
                                            "division by zero".to_string(),
                                        )],
                                    }))],
                                ))]),
                            )],
                            else_body: None,
                        })),
                        expr_stmt(bin(
                            BinOp::Div,
                            call(name("eval"), vec![name("a")]),
                            name("divisor"),
                        )),
                    ]),
                    span: sp(),
                },
            ],
        }));

        let eval_fn = FnDecl {
            name: "eval".to_string(),
            type_params: vec![],
            params: vec![Param::Named {
                name: "e".to_string(),
                ty: Type {
                    base: BaseType::Named { name: "Expr".to_string(), args: vec![] },
                    nullable: false,
                    span: sp(),
                },
                default: None,
            }],
            returns: Some(ty_float()),
            body: block(vec![st(StmtKind::Return(Some(match_expr)))]),
            doc: None,
            span: sp(),
        };

        // Build it and the program in a fresh interpreter.
        let program = Program {
            stmts: vec![st(StmtKind::Enum(expr_enum)), st(StmtKind::Fn(eval_fn))],
        };

        let mut sink = std::io::sink();
        let mut interp = Interp { registry: Registry::default(), out: &mut sink, propagating: None };
        let root = Scope::new_root();
        seed_prelude(&root);
        interp.collect_decls(&program);
        for stmt in &program.stmts {
            interp.exec_stmt(stmt, &root).unwrap();
        }

        // program = Expr.Mul(Expr.Add(Expr.Num(1.0), Expr.Num(2.0)), Expr.Num(3.0))
        let vc = |v: &str, args: Vec<Expr>| {
            call(
                ex(ExprKind::Member { base: Box::new(name("Expr")), name: v.to_string(), safe: false }),
                args,
            )
        };
        let num = |f: f64| vc("Num", vec![float(f)]);
        let prog_expr = vc(
            "Mul",
            vec![vc("Add", vec![num(1.0), num(2.0)]), num(3.0)],
        );
        let call_eval = call(name("eval"), vec![prog_expr]);

        let result = interp.eval(&call_eval, &root).unwrap();
        match &result {
            Value::Float(f) => assert_eq!(*f, 9.0),
            v => panic!("expected Float(9.0), got {:?}", v),
        }
        // The critical formatting requirement from the spec.
        assert_eq!(show(&result), "9.0");
    }

    #[test]
    fn showcase_division_by_zero_panics() {
        // Reuse a minimal version: eval(Div(Num(1.0), Num(0.0))) must Err.
        // Build a tiny enum + eval fn covering just Num and Div.
        fn ty_float() -> Type {
            Type {
                base: BaseType::Named { name: "Float".to_string(), args: vec![] },
                nullable: false,
                span: sp(),
            }
        }
        fn call(callee: Expr, args: Vec<Expr>) -> Expr {
            ex(ExprKind::Call {
                callee: Box::new(callee),
                args: args.into_iter().map(Arg::Positional).collect(),
            })
        }
        fn variant_pat(name: &str, subs: Vec<&str>) -> Pattern {
            Pattern {
                kind: PatternKind::Variant {
                    enum_name: None,
                    name: name.to_string(),
                    subs: subs
                        .into_iter()
                        .map(|s| Pattern {
                            kind: PatternKind::Binding(s.to_string()),
                            span: sp(),
                        })
                        .collect(),
                },
                span: sp(),
            }
        }

        let expr_enum = EnumDecl {
            name: "E".to_string(),
            type_params: vec![],
            derives: vec![],
            variants: vec![
                VariantDecl {
                    name: "Num".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float()])),
                    doc: None,
                    span: sp(),
                },
                VariantDecl {
                    name: "Div".to_string(),
                    payload: Some(Payload::Positional(vec![ty_float(), ty_float()])),
                    doc: None,
                    span: sp(),
                },
            ],
            doc: None,
            span: sp(),
        };

        let match_expr = ex(ExprKind::Match(MatchExpr {
            scrutinee: Box::new(name("e")),
            arms: vec![
                MatchArm {
                    pattern: variant_pat("Num", vec!["n"]),
                    guard: None,
                    body: block(vec![expr_stmt(name("n"))]),
                    span: sp(),
                },
                MatchArm {
                    pattern: variant_pat("Div", vec!["a", "b"]),
                    guard: None,
                    body: block(vec![
                        st(StmtKind::Binding(Binding {
                            name: "divisor".to_string(),
                            binder: Binder::Name("divisor".to_string()),
                            is_val: false,
                            ty: None,
                            value: call(name("eval"), vec![name("b")]),
                        })),
                        st(StmtKind::If(IfStmt {
                            arms: vec![(
                                bin(BinOp::Eq, name("divisor"), float(0.0)),
                                block(vec![expr_stmt(call(
                                    name("panic"),
                                    vec![ex(ExprKind::Str(StringLit {
                                        parts: vec![StrSeg::Text("division by zero".to_string())],
                                    }))],
                                ))]),
                            )],
                            else_body: None,
                        })),
                        expr_stmt(bin(
                            BinOp::Div,
                            call(name("eval"), vec![name("a")]),
                            name("divisor"),
                        )),
                    ]),
                    span: sp(),
                },
            ],
        }));

        let eval_fn = FnDecl {
            name: "eval".to_string(),
            type_params: vec![],
            params: vec![Param::Named {
                name: "e".to_string(),
                ty: Type {
                    base: BaseType::Named { name: "E".to_string(), args: vec![] },
                    nullable: false,
                    span: sp(),
                },
                default: None,
            }],
            returns: Some(ty_float()),
            body: block(vec![st(StmtKind::Return(Some(match_expr)))]),
            doc: None,
            span: sp(),
        };

        let program = Program {
            stmts: vec![st(StmtKind::Enum(expr_enum)), st(StmtKind::Fn(eval_fn))],
        };
        let mut sink = std::io::sink();
        let mut interp = Interp { registry: Registry::default(), out: &mut sink, propagating: None };
        let root = Scope::new_root();
        seed_prelude(&root);
        interp.collect_decls(&program);
        for stmt in &program.stmts {
            interp.exec_stmt(stmt, &root).unwrap();
        }

        let vc = |v: &str, args: Vec<Expr>| {
            call(
                ex(ExprKind::Member { base: Box::new(name("E")), name: v.to_string(), safe: false }),
                args,
            )
        };
        let num = |f: f64| vc("Num", vec![float(f)]);
        let div_zero = call(name("eval"), vec![vc("Div", vec![num(1.0), num(0.0)])]);
        let r = interp.eval(&div_zero, &root);
        assert!(r.is_err());
        assert!(r.unwrap_err().message.contains("division by zero"));
    }

    // =======================================================================
    // Built-in method dispatch (List / Map / Set / String)
    // =======================================================================

    /// Build a `Value::List` from raw `Int`s.
    fn int_list(ns: &[i64]) -> Value {
        list_value(ns.iter().map(|&n| Value::Int(BigInt::from(n))).collect())
    }

    /// Extract the `Int`s out of a `Value::List`, panicking on any other shape.
    fn list_ints(v: &Value) -> Vec<i64> {
        match v {
            Value::List(items) => items
                .borrow()
                .iter()
                .map(|x| match x {
                    Value::Int(n) => n.to_i64().unwrap(),
                    other => panic!("expected Int element, got {:?}", other),
                })
                .collect(),
            other => panic!("expected List, got {:?}", other),
        }
    }

    /// A passable callable `Value` built from a single-param lambda `p -> body`.
    fn lambda1(interp: &mut Interp, root: &Env, p: &str, body: Expr) -> Value {
        let lam = ex(ExprKind::Lambda(Lambda {
            params: vec![p.to_string()],
            body: Box::new(body),
        }));
        interp.eval(&lam, root).unwrap()
    }

    /// Call a built-in method on `recv` with already-evaluated argument values
    /// (wrapped as trivial positional arg expressions is unnecessary — we go
    /// straight through the per-type dispatchers).
    fn list_call(interp: &mut Interp, recv: &Value, name: &str, args: Vec<Value>) -> Value {
        let items = match recv {
            Value::List(items) => items.clone(),
            other => panic!("list_call on non-list {:?}", other),
        };
        interp.list_method(&items, name, args, sp()).unwrap()
    }

    #[test]
    fn list_map_filter_sum_pipeline() {
        let (mut interp, root) = fresh();
        let xs = int_list(&[1, 2, 3, 4, 5, 6]);
        // filter(n -> n % 2 == 0)
        let even = lambda1(
            &mut interp,
            &root,
            "n",
            bin(BinOp::Eq, bin(BinOp::Rem, name("n"), int(2)), int(0)),
        );
        let filtered = list_call(&mut interp, &xs, "filter", vec![even]);
        assert_eq!(list_ints(&filtered), vec![2, 4, 6]);
        // map(n -> n * n)
        let square = lambda1(&mut interp, &root, "n", bin(BinOp::Mul, name("n"), name("n")));
        let mapped = list_call(&mut interp, &filtered, "map", vec![square]);
        assert_eq!(list_ints(&mapped), vec![4, 16, 36]);
        // sum() -> 56
        match list_call(&mut interp, &mapped, "sum", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(56)),
            v => panic!("expected Int(56), got {:?}", v),
        }
    }

    #[test]
    fn list_fold_and_reduce() {
        let (mut interp, root) = fresh();
        let xs = int_list(&[1, 2, 3, 4]);
        let add = lambda1_2(&mut interp, &root, "a", "b", bin(BinOp::Add, name("a"), name("b")));
        match list_call(&mut interp, &xs, "fold", vec![Value::Int(BigInt::from(100)), add.clone()]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(110)),
            v => panic!("expected Int(110), got {:?}", v),
        }
        match list_call(&mut interp, &xs, "reduce", vec![add]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(10)),
            v => panic!("expected Int(10), got {:?}", v),
        }
        // reduce on empty is an error.
        let empty = int_list(&[]);
        let add2 = lambda1_2(&mut interp, &root, "a", "b", bin(BinOp::Add, name("a"), name("b")));
        let items = match &empty {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        assert!(interp.list_method(&items, "reduce", vec![add2], sp()).is_err());
    }

    /// A passable callable from a two-param lambda `(a, b) -> body`.
    fn lambda1_2(interp: &mut Interp, root: &Env, a: &str, b: &str, body: Expr) -> Value {
        let lam = ex(ExprKind::Lambda(Lambda {
            params: vec![a.to_string(), b.to_string()],
            body: Box::new(body),
        }));
        interp.eval(&lam, root).unwrap()
    }

    #[test]
    fn list_predicates_and_search() {
        let (mut interp, root) = fresh();
        let xs = int_list(&[1, 2, 3, 4, 5]);
        let gt3 = |interp: &mut Interp, root: &Env| {
            lambda1(interp, root, "n", bin(BinOp::Gt, name("n"), int(3)))
        };
        let p = gt3(&mut interp, &root);
        assert!(matches!(list_call(&mut interp, &xs, "any", vec![p]), Value::Bool(true)));
        let p = gt3(&mut interp, &root);
        assert!(matches!(list_call(&mut interp, &xs, "all", vec![p]), Value::Bool(false)));
        let p = gt3(&mut interp, &root);
        match list_call(&mut interp, &xs, "find", vec![p]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(4)),
            v => panic!("expected Int(4), got {:?}", v),
        }
        // find with no match -> Null
        let none = lambda1(&mut interp, &root, "n", bin(BinOp::Gt, name("n"), int(99)));
        assert!(matches!(list_call(&mut interp, &xs, "find", vec![none]), Value::Null));
        // contains
        assert!(matches!(
            list_call(&mut interp, &xs, "contains", vec![Value::Int(BigInt::from(3))]),
            Value::Bool(true)
        ));
        assert!(matches!(
            list_call(&mut interp, &xs, "contains", vec![Value::Int(BigInt::from(9))]),
            Value::Bool(false)
        ));
        // a non-Bool predicate is a runtime error (no truthiness).
        let bad = lambda1(&mut interp, &root, "n", name("n"));
        let items = match &xs {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        assert!(interp.list_method(&items, "filter", vec![bad], sp()).is_err());
    }

    #[test]
    fn list_size_first_last_min_max() {
        let (mut interp, _root) = fresh();
        let xs = int_list(&[3, 1, 4, 1, 5]);
        match list_call(&mut interp, &xs, "count", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("expected Int, got {:?}", v),
        }
        match list_call(&mut interp, &xs, "len", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("expected Int, got {:?}", v),
        }
        assert!(matches!(list_call(&mut interp, &xs, "is_empty", vec![]), Value::Bool(false)));
        match list_call(&mut interp, &xs, "first", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("got {:?}", v),
        }
        match list_call(&mut interp, &xs, "last", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("got {:?}", v),
        }
        match list_call(&mut interp, &xs, "min", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(1)),
            v => panic!("got {:?}", v),
        }
        match list_call(&mut interp, &xs, "max", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("got {:?}", v),
        }
        // first/last on empty -> Null
        let empty = int_list(&[]);
        assert!(matches!(list_call(&mut interp, &empty, "first", vec![]), Value::Null));
        assert!(matches!(list_call(&mut interp, &empty, "last", vec![]), Value::Null));
        assert!(matches!(list_call(&mut interp, &empty, "is_empty", vec![]), Value::Bool(true)));
    }

    #[test]
    fn list_take_skip_reverse_sorted_collect() {
        let (mut interp, _root) = fresh();
        let xs = int_list(&[1, 2, 3, 4, 5]);
        assert_eq!(
            list_ints(&list_call(&mut interp, &xs, "take", vec![Value::Int(BigInt::from(2))])),
            vec![1, 2]
        );
        assert_eq!(
            list_ints(&list_call(&mut interp, &xs, "skip", vec![Value::Int(BigInt::from(3))])),
            vec![4, 5]
        );
        // take/skip beyond length, and a negative count clamps to 0.
        assert_eq!(
            list_ints(&list_call(&mut interp, &xs, "take", vec![Value::Int(BigInt::from(99))])),
            vec![1, 2, 3, 4, 5]
        );
        assert!(
            list_ints(&list_call(&mut interp, &xs, "skip", vec![Value::Int(BigInt::from(-1))]))
                .len()
                == 5
        );
        assert_eq!(
            list_ints(&list_call(&mut interp, &xs, "reverse", vec![])),
            vec![5, 4, 3, 2, 1]
        );
        let unsorted = int_list(&[3, 1, 4, 1, 5, 9, 2, 6]);
        assert_eq!(
            list_ints(&list_call(&mut interp, &unsorted, "sorted", vec![])),
            vec![1, 1, 2, 3, 4, 5, 6, 9]
        );
        assert_eq!(list_ints(&list_call(&mut interp, &xs, "collect", vec![])), vec![1, 2, 3, 4, 5]);
        // sorted over incomparable (mixed Int/String) is an error.
        let mixed = list_value(vec![Value::Int(BigInt::from(1)), Value::Str("a".to_string())]);
        let items = match &mixed {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        assert!(interp.list_method(&items, "sorted", vec![], sp()).is_err());
    }

    #[test]
    fn list_enumerate_and_zip_make_tuples() {
        let (mut interp, _root) = fresh();
        let xs = list_value(vec![Value::Str("a".to_string()), Value::Str("b".to_string())]);
        let en = list_call(&mut interp, &xs, "enumerate", vec![]);
        match &en {
            Value::List(items) => {
                let items = items.borrow();
                assert_eq!(items.len(), 2);
                match &items[0] {
                    Value::Tuple(t) => {
                        assert!(matches!(&t[0], Value::Int(n) if *n == BigInt::from(0)));
                        assert!(matches!(&t[1], Value::Str(s) if s == "a"));
                    }
                    v => panic!("expected tuple, got {:?}", v),
                }
            }
            v => panic!("expected list, got {:?}", v),
        }
        // zip pairs up to the shorter length.
        let ns = int_list(&[10, 20, 30]);
        let zipped = list_call(&mut interp, &ns, "zip", vec![xs]);
        match &zipped {
            Value::List(items) => assert_eq!(items.borrow().len(), 2),
            v => panic!("expected list, got {:?}", v),
        }
    }

    #[test]
    fn list_append_and_pop_last_mutate() {
        let (mut interp, _root) = fresh();
        let xs = int_list(&[1, 2]);
        list_call(&mut interp, &xs, "append", vec![Value::Int(BigInt::from(3))]);
        assert_eq!(list_ints(&xs), vec![1, 2, 3]); // mutated in place
        match list_call(&mut interp, &xs, "pop_last", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("got {:?}", v),
        }
        assert_eq!(list_ints(&xs), vec![1, 2]);
        // pop_last on empty -> Null
        let empty = int_list(&[]);
        assert!(matches!(list_call(&mut interp, &empty, "pop_last", vec![]), Value::Null));
    }

    #[test]
    fn sum_int_float_and_mixed_error() {
        let (mut interp, _root) = fresh();
        // empty sum is Int(0)
        match list_call(&mut interp, &int_list(&[]), "sum", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(0)),
            v => panic!("got {:?}", v),
        }
        let floats = list_value(vec![Value::Float(1.5), Value::Float(2.0)]);
        match list_call(&mut interp, &floats, "sum", vec![]) {
            Value::Float(f) => assert_eq!(f, 3.5),
            v => panic!("got {:?}", v),
        }
        // mixing Int and Float is a runtime error (no coercion).
        let mixed = list_value(vec![Value::Int(BigInt::from(1)), Value::Float(2.0)]);
        let items = match &mixed {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        assert!(interp.list_method(&items, "sum", vec![], sp()).is_err());
    }

    // ---- Map --------------------------------------------------------------

    fn map_value(pairs: Vec<(Value, Value)>) -> Value {
        Value::Map(Rc::new(RefCell::new(pairs)))
    }

    fn map_call(interp: &mut Interp, recv: &Value, name: &str, args: Vec<Value>) -> Value {
        let pairs = match recv {
            Value::Map(p) => p.clone(),
            other => panic!("map_call on non-map {:?}", other),
        };
        interp.map_method(&pairs, name, args, sp()).unwrap()
    }

    #[test]
    fn map_get_insert_contains_len() {
        let (mut interp, _root) = fresh();
        let m = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
        ]);
        match map_call(&mut interp, &m, "get", vec![Value::Str("a".to_string())]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(1)),
            v => panic!("got {:?}", v),
        }
        // absent key -> Null
        assert!(matches!(
            map_call(&mut interp, &m, "get", vec![Value::Str("z".to_string())]),
            Value::Null
        ));
        // has / contains
        assert!(matches!(
            map_call(&mut interp, &m, "has", vec![Value::Str("b".to_string())]),
            Value::Bool(true)
        ));
        assert!(matches!(
            map_call(&mut interp, &m, "contains", vec![Value::Str("z".to_string())]),
            Value::Bool(false)
        ));
        // insert new key, then overwrite existing (in place; preserves order)
        map_call(&mut interp, &m, "insert", vec![Value::Str("c".to_string()), Value::Int(BigInt::from(3))]);
        map_call(&mut interp, &m, "insert", vec![Value::Str("a".to_string()), Value::Int(BigInt::from(9))]);
        match map_call(&mut interp, &m, "get", vec![Value::Str("a".to_string())]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(9)),
            v => panic!("got {:?}", v),
        }
        match map_call(&mut interp, &m, "len", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("got {:?}", v),
        }
    }

    #[test]
    fn map_keys_values_items() {
        let (mut interp, _root) = fresh();
        let m = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
        ]);
        // keys() preserves insertion order
        match map_call(&mut interp, &m, "keys", vec![]) {
            Value::List(items) => {
                let ks: Vec<String> = items
                    .borrow()
                    .iter()
                    .map(|k| match k {
                        Value::Str(s) => s.clone(),
                        v => panic!("got {:?}", v),
                    })
                    .collect();
                assert_eq!(ks, vec!["a", "b"]);
            }
            v => panic!("got {:?}", v),
        }
        assert_eq!(list_ints(&map_call(&mut interp, &m, "values", vec![])), vec![1, 2]);
        // items() yields 2-tuples
        match map_call(&mut interp, &m, "items", vec![]) {
            Value::List(items) => {
                let items = items.borrow();
                assert_eq!(items.len(), 2);
                match &items[0] {
                    Value::Tuple(t) => {
                        assert_eq!(t.len(), 2);
                        assert!(matches!(&t[0], Value::Str(s) if s == "a"));
                        assert!(matches!(&t[1], Value::Int(n) if *n == BigInt::from(1)));
                    }
                    v => panic!("got {:?}", v),
                }
            }
            v => panic!("got {:?}", v),
        }
    }

    // ---- Set --------------------------------------------------------------

    fn set_call(interp: &mut Interp, recv: &Value, name: &str, args: Vec<Value>) -> Value {
        let items = match recv {
            Value::Set(i) => i.clone(),
            other => panic!("set_call on non-set {:?}", other),
        };
        interp.set_method(&items, name, args, sp()).unwrap()
    }

    fn set_of(ns: &[i64]) -> Value {
        set_value(ns.iter().map(|&n| Value::Int(BigInt::from(n))).collect())
    }

    #[test]
    fn set_insert_dedup_contains_len() {
        let (mut interp, _root) = fresh();
        let s = set_of(&[1, 2]);
        set_call(&mut interp, &s, "insert", vec![Value::Int(BigInt::from(2))]); // dup, no-op
        set_call(&mut interp, &s, "insert", vec![Value::Int(BigInt::from(3))]);
        match set_call(&mut interp, &s, "len", vec![]) {
            Value::Int(n) => assert_eq!(n, BigInt::from(3)),
            v => panic!("got {:?}", v),
        }
        assert!(matches!(
            set_call(&mut interp, &s, "contains", vec![Value::Int(BigInt::from(3))]),
            Value::Bool(true)
        ));
        assert!(matches!(
            set_call(&mut interp, &s, "contains", vec![Value::Int(BigInt::from(9))]),
            Value::Bool(false)
        ));
    }

    #[test]
    fn set_union_and_intersect() {
        let (mut interp, _root) = fresh();
        let a = set_of(&[1, 2, 3]);
        let b = set_of(&[2, 3, 4]);
        let u = set_call(&mut interp, &a, "union", vec![b.clone()]);
        // union keeps insertion order of `a` then new elements of `b`
        match &u {
            Value::Set(items) => {
                let got: Vec<i64> = items
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Int(n) => n.to_i64().unwrap(),
                        x => panic!("got {:?}", x),
                    })
                    .collect();
                assert_eq!(got, vec![1, 2, 3, 4]);
            }
            v => panic!("got {:?}", v),
        }
        let i = set_call(&mut interp, &a, "intersect", vec![b]);
        match &i {
            Value::Set(items) => {
                let got: Vec<i64> = items
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Int(n) => n.to_i64().unwrap(),
                        x => panic!("got {:?}", x),
                    })
                    .collect();
                assert_eq!(got, vec![2, 3]);
            }
            v => panic!("got {:?}", v),
        }
    }

    // ---- String, equality, show, named-arg rejection ----------------------

    #[test]
    fn string_len() {
        // counts Unicode scalar values, not bytes (`é` is one char, two bytes).
        match Interp::str_method("héllo", "len", &[], sp()).unwrap() {
            Value::Int(n) => assert_eq!(n, BigInt::from(5)),
            v => panic!("expected Int(5), got {:?}", v),
        }
    }

    #[test]
    fn map_set_equality_is_order_insensitive() {
        let m1 = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
        ]);
        let m2 = map_value(vec![
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
        ]);
        assert!(values_equal(&m1, &m2));
        // differing value breaks equality
        let m3 = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(9))),
        ]);
        assert!(!values_equal(&m1, &m3));

        let s1 = set_of(&[1, 2, 3]);
        let s2 = set_of(&[3, 2, 1]);
        assert!(values_equal(&s1, &s2));
        assert!(!values_equal(&s1, &set_of(&[1, 2])));
    }

    #[test]
    fn show_map_and_set() {
        let m = map_value(vec![
            (Value::Str("a".to_string()), Value::Int(BigInt::from(1))),
            (Value::Str("b".to_string()), Value::Int(BigInt::from(2))),
        ]);
        assert_eq!(show(&m), "{a: 1, b: 2}");
        // empty map is `{}`
        assert_eq!(show(&map_value(vec![])), "{}");
        // set renders `{a, b}`; empty set renders `Set()`
        assert_eq!(show(&set_of(&[1, 2])), "{1, 2}");
        assert_eq!(show(&set_value(vec![])), "Set()");
    }

    #[test]
    fn builtin_method_rejects_named_args() {
        let (mut interp, root) = fresh();
        // xs.contains(x: 1) — named arg on a built-in method is rejected.
        let call_expr = ex(ExprKind::Call {
            callee: Box::new(ex(ExprKind::Member {
                base: Box::new(ex(ExprKind::List(vec![int(1), int(2)]))),
                name: "contains".to_string(),
                safe: false,
            })),
            args: vec![Arg::Named { name: "x".to_string(), value: int(1) }],
        });
        let r = interp.eval(&call_expr, &root);
        assert!(r.is_err());
        assert!(r.unwrap_err().message.contains("positional"));
    }

    #[test]
    fn unknown_builtin_method_errors() {
        let (mut interp, _root) = fresh();
        let xs = int_list(&[1, 2, 3]);
        let items = match &xs {
            Value::List(i) => i.clone(),
            _ => unreachable!(),
        };
        let r = interp.list_method(&items, "no_such_method", vec![], sp());
        assert!(r.is_err());
        assert!(r.unwrap_err().message.contains("no method"));
    }
    // =====================================================================
    // Collections, comprehensions, tuples, default/named args.
    // Asserted on structure / scalars (not Map/Set `Show`, covered above).
    // =====================================================================

    /// Collect a `Value::List`'s elements, or panic.
    fn list_vals(v: Value) -> Vec<Value> {
        match v {
            Value::List(items) => items.borrow().clone(),
            other => panic!("expected List, got {:?}", other),
        }
    }

    fn as_int(v: &Value) -> i64 {
        match v {
            Value::Int(n) => n.to_i64().expect("fits i64"),
            other => panic!("expected Int, got {:?}", other),
        }
    }

    #[test]
    fn tuple_literal_and_eq() {
        // (1, 2) == (1, 2)
        match eval_src("val t = (1, 2)\nt == (1, 2)\n") {
            Value::Bool(b) => assert!(b),
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    #[test]
    fn set_literal_dedups() {
        // A set literal drops structural duplicates; collect its len via a list.
        match eval_src("val s = {1, 2, 2, 3, 1}\ns\n") {
            Value::Set(items) => assert_eq!(items.borrow().len(), 3),
            v => panic!("expected Set, got {:?}", v),
        }
    }

    #[test]
    fn map_literal_dedups_last_wins() {
        // A repeated key keeps first-seen position but the latest value.
        match eval_src("val m = {1: 10, 1: 20}\nm\n") {
            Value::Map(pairs) => {
                let p = pairs.borrow();
                assert_eq!(p.len(), 1);
                assert_eq!(as_int(&p[0].1), 20);
            }
            v => panic!("expected Map, got {:?}", v),
        }
    }

    #[test]
    fn list_comprehension_runs() {
        // [x * x for x in 1..=5 if x != 3] == [1, 4, 16, 25]
        let xs = list_vals(eval_src("[x * x for x in 1..=5 if x != 3]\n"));
        let got: Vec<i64> = xs.iter().map(as_int).collect();
        assert_eq!(got, vec![1, 4, 16, 25]);
    }

    #[test]
    fn set_comprehension_dedups() {
        match eval_src("{x % 3 for x in 0..9}\n") {
            Value::Set(items) => assert_eq!(items.borrow().len(), 3), // {0, 1, 2}
            v => panic!("expected Set, got {:?}", v),
        }
    }

    #[test]
    fn comprehension_over_tuple_list_destructures() {
        // Iterate a list of tuples, destructuring each into (a, b); sum a + b.
        let xs = list_vals(eval_src("[a + b for (a, b) in [(1, 2), (3, 4)]]\n"));
        let got: Vec<i64> = xs.iter().map(as_int).collect();
        assert_eq!(got, vec![3, 7]);
    }

    #[test]
    fn comprehension_filter_must_be_bool() {
        // A non-Bool filter is a runtime error (no truthiness).
        let toks = crate::lexer::lex("[x for x in 0..3 if x]\n").unwrap();
        let program = crate::parser::parse(&toks).unwrap();
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let last = &program.stmts[program.stmts.len() - 1];
        if let StmtKind::Expr(e) = &last.kind {
            assert!(interp.eval(e, &root).is_err());
        } else {
            panic!("expected expr statement");
        }
    }

    #[test]
    fn val_tuple_destructure_binds_names() {
        // val (a, b) = (7, 9) ; a + b == 16
        assert_eq!(as_int(&eval_src("val (a, b) = (7, 9)\na + b\n")), 16);
    }

    #[test]
    fn for_tuple_destructure_sums() {
        // for (a, b) in [(1,2),(3,4)]: total = total + a + b  -> 10
        let src = "total = 0\nfor (a, b) in [(1, 2), (3, 4)]:\n    total = total + a + b\ntotal\n";
        assert_eq!(as_int(&eval_src(src)), 10);
    }

    #[test]
    fn default_arg_used_when_omitted() {
        // fn add(a: Int, b: Int = 10) -> Int: a + b ; add(5) == 15
        let src = "fn add(a: Int, b: Int = 10) -> Int:\n    a + b\nadd(5)\n";
        assert_eq!(as_int(&eval_src(src)), 15);
    }

    #[test]
    fn default_arg_overridden_positionally() {
        let src = "fn add(a: Int, b: Int = 10) -> Int:\n    a + b\nadd(5, 100)\n";
        assert_eq!(as_int(&eval_src(src)), 105);
    }

    #[test]
    fn named_call_arg_binds_by_name() {
        // greeting passed by name; result is its concatenation.
        let src = "fn greet(name: String, greeting: String = \"Hi\") -> String:\n    greeting + name\ngreet(\"Ada\", greeting: \"Hello \")\n";
        match eval_src(src) {
            Value::Str(s) => assert_eq!(s, "Hello Ada"),
            v => panic!("expected String, got {:?}", v),
        }
    }

    #[test]
    fn lambda_passed_to_function_typed_param() {
        // A lambda flows into a function-typed param and is called.
        let src = "fn apply(f: (Int) -> Int, x: Int) -> Int:\n    f(x)\napply(n -> n * n, 6)\n";
        assert_eq!(as_int(&eval_src(src)), 36);
    }

    #[test]
    fn missing_required_arg_errors() {
        // fn f(a: Int): a ; f() -> arity error.
        let toks = crate::lexer::lex("fn f(a: Int) -> Int:\n    a\nf()\n").unwrap();
        let program = crate::parser::parse(&toks).unwrap();
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let mut err = None;
        for stmt in &program.stmts {
            if let Err(d) = interp.exec_stmt(stmt, &root) {
                err = Some(d);
                break;
            }
        }
        assert!(err.is_some(), "expected an arity error");
    }

    #[test]
    fn tuple_destructure_arity_mismatch_errors() {
        // val (a, b) = (1, 2, 3) -> runtime error.
        let toks = crate::lexer::lex("val (a, b) = (1, 2, 3)\n").unwrap();
        let program = crate::parser::parse(&toks).unwrap();
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        let r = interp.exec_stmt(&program.stmts[0], &root);
        assert!(r.is_err());
    }

    // ----- match guards / or-patterns / tuple+nested patterns ----

    #[test]
    fn match_guard_true_takes_arm() {
        // The guard holds, so the guarded arm fires (and sees the binding `x`).
        let src = "n = 7\nmatch n:\n    x if x > 5: x * 2\n    _: 0\n";
        assert_eq!(as_int(&eval_src(src)), 14);
    }

    #[test]
    fn match_guard_false_falls_through() {
        // The guard fails, so control falls to the next (catch-all) arm.
        let src = "n = 3\nmatch n:\n    x if x > 5: x * 2\n    _: 99\n";
        assert_eq!(as_int(&eval_src(src)), 99);
    }

    #[test]
    fn match_guard_non_bool_errors() {
        // A non-Bool guard is a runtime error (no truthiness).
        let toks = crate::lexer::lex("match 1:\n    x if x: 1\n    _: 0\n").unwrap();
        let program = crate::parser::parse(&toks).unwrap();
        let (mut interp, root) = fresh();
        interp.collect_decls(&program);
        if let StmtKind::Expr(e) = &program.stmts[0].kind {
            assert!(interp.eval(e, &root).is_err());
        } else {
            panic!("expected expr statement");
        }
    }

    #[test]
    fn or_pattern_literal_alternatives() {
        // Any of 1/2/3 takes the arm; 4 falls through.
        let hit = "n = 2\nmatch n:\n    1 or 2 or 3: 1\n    _: 0\n";
        let miss = "n = 4\nmatch n:\n    1 or 2 or 3: 1\n    _: 0\n";
        assert_eq!(as_int(&eval_src(hit)), 1);
        assert_eq!(as_int(&eval_src(miss)), 0);
    }

    #[test]
    fn or_pattern_variant_alternatives() {
        // `.Red or .Green` matches either variant of the scrutinee's enum.
        let src = "enum Color:\n    Red\n    Green\n    Blue\nc = Color.Green\nmatch c:\n    .Red or .Green: 1\n    .Blue: 0\n";
        assert_eq!(as_int(&eval_src(src)), 1);
    }

    #[test]
    fn or_pattern_binds_from_matching_alternative() {
        // The binding from the alternative that matched is visible in the body.
        let src = "n = 5\nmatch n:\n    0: 0\n    x or y: x\n";
        assert_eq!(as_int(&eval_src(src)), 5);
    }

    #[test]
    fn tuple_pattern_destructures() {
        // A 2-tuple binds element-wise.
        let src = "p = (3, 4)\nmatch p:\n    (a, b): a + b\n";
        assert_eq!(as_int(&eval_src(src)), 7);
    }

    #[test]
    fn tuple_pattern_wrong_arity_skips() {
        // A 3-arity tuple pattern does not match a 2-tuple; fall through.
        let src = "p = (1, 2)\nmatch p:\n    (a, b, c): 0\n    _: 99\n";
        assert_eq!(as_int(&eval_src(src)), 99);
    }

    #[test]
    fn nested_variant_subpattern_binds_inner() {
        // A variant nested inside a variant binds the inner payload.
        let src = "enum Inner:\n    Pair(Int, Int)\nenum Outer:\n    Wrap(Inner)\n    None\nv = Outer.Wrap(Inner.Pair(2, 5))\nmatch v:\n    .Wrap(.Pair(a, b)): a + b\n    .None: 0\n";
        assert_eq!(as_int(&eval_src(src)), 7);
    }

    #[test]
    fn nested_literal_subpattern_filters() {
        // A literal sub-pattern only matches a specific payload value.
        let src = "enum Tag:\n    N(Int)\nv = Tag.N(2)\nmatch v:\n    .N(1): 10\n    .N(2): 20\n    .N(x): x\n";
        assert_eq!(as_int(&eval_src(src)), 20);
    }
    // ---- `?.` safe-call and `.expect` -----------------------

    #[test]
    fn safe_member_on_null_yields_null() {
        // x?.field on a null receiver short-circuits to null.
        let src = "struct P:\n    name: String\nx: P? = null\nx?.name\n";
        assert!(matches!(eval_src(src), Value::Null));
    }

    #[test]
    fn safe_member_on_present_reads_field() {
        // x?.field on a present struct reads the field like `.`.
        let src = "struct P:\n    name: String\nx: P? = P(name: \"Ada\")\nx?.name\n";
        match eval_src(src) {
            Value::Str(s) => assert_eq!(s, "Ada"),
            v => panic!("expected String, got {:?}", v),
        }
    }

    #[test]
    fn safe_method_call_on_null_yields_null_without_evaluating_args() {
        // x?.m(panic(...)) on a null receiver must NOT evaluate the args — if it
        // did, the `panic` would surface as an error.
        let src =
            "struct P:\n    name: String\nx: P? = null\nx?.greet(panic(\"boom\"))\n";
        assert!(matches!(eval_src(src), Value::Null));
    }

    #[test]
    fn safe_call_chain_short_circuits() {
        // a?.b?.c yields null when an inner link is null.
        let src = "struct Inner:\n    n: Int\nstruct Outer:\n    inner: Inner?\nval o = Outer(inner: null)\no?.inner?.n\n";
        assert!(matches!(eval_src(src), Value::Null));
    }

    #[test]
    fn safe_call_chain_reaches_value_when_present() {
        let src = "struct Inner:\n    n: Int\nstruct Outer:\n    inner: Inner?\nval o = Outer(inner: Inner(n: 42))\no?.inner?.n\n";
        assert_eq!(as_int(&eval_src(src)), 42);
    }

    #[test]
    fn expect_present_returns_value() {
        // x.expect(msg) on a present value returns the value unchanged.
        let src = "val x: Int? = 7\nx.expect(\"required\")\n";
        assert_eq!(as_int(&eval_src(src)), 7);
    }

    #[test]
    fn expect_null_panics_with_message() {
        // x.expect(msg) on null is a runtime error carrying `msg`.
        let d = eval_src_err("val x: Int? = null\nx.expect(\"name was required\")\n");
        assert!(
            d.message.contains("name was required"),
            "expect panic should carry the message: {}",
            d.message
        );
    }
