use super::*;

fn parse(src: &str) -> Program {
    Parser::parse(src).expect("parse error")
}

fn expr(src: &str) -> Expr {
    match parse(src).stmts.into_iter().next().unwrap() {
        Stmt::Expr(e) => e,
        other => panic!("expected Expr stmt, got {other:?}"),
    }
}

#[test]
fn integer_literal() {
    assert_eq!(expr("42"), Expr::Lit(Literal::Int(42)));
}

#[test]
fn string_literal() {
    assert_eq!(expr("'hi'"), Expr::Lit(Literal::Str(Arc::from("hi"))));
}

#[test]
fn variable() {
    assert_eq!(
        expr("x"),
        Expr::Var {
            name: Arc::from("x"),
            res: VarRef::Unresolved,
        }
    );
}

#[test]
fn unary_send() {
    assert_eq!(
        expr("3 negated"),
        Expr::Send {
            receiver: Box::new(Expr::Lit(Literal::Int(3))),
            message: Message::Unary(Arc::from("negated")),
        }
    );
}

#[test]
fn pipe_parsed_as_binary_send() {
    // `|` in expression position is a binary message, not a local-decl delimiter.
    assert_eq!(
        expr("true | false"),
        Expr::Send {
            receiver: Box::new(Expr::Lit(Literal::Bool(true))),
            message: Message::Binary {
                op: Arc::from("|"),
                arg: Box::new(Expr::Lit(Literal::Bool(false))),
            },
        }
    );
}

#[test]
fn binary_send() {
    assert_eq!(
        expr("3 + 4"),
        Expr::Send {
            receiver: Box::new(Expr::Lit(Literal::Int(3))),
            message: Message::Binary {
                op: Arc::from("+"),
                arg: Box::new(Expr::Lit(Literal::Int(4))),
            },
        }
    );
}

#[test]
fn keyword_send_two_part() {
    // `obj at: 1 put: 2` → two-part keyword send, selector `at:put:`
    let e = expr("obj at: 1 put: 2");
    match e {
        Expr::Send {
            message: Message::Keyword { parts },
            ..
        } => {
            assert_eq!(parts.len(), 2);
            assert_eq!(parts[0].0, Arc::from("at:"));
            assert_eq!(parts[1].0, Arc::from("put:"));
        }
        _ => panic!("expected keyword send"),
    }
}

#[test]
fn assignment() {
    let prog = parse("x := 42");
    assert_eq!(
        prog.stmts[0],
        Stmt::Assign {
            name: Arc::from("x"),
            res: VarRef::Unresolved,
            value: Expr::Lit(Literal::Int(42))
        }
    );
}

#[test]
fn local_decl() {
    let prog = parse("| x y |");
    assert_eq!(
        prog.stmts[0],
        Stmt::LocalDecl(vec![Arc::from("x"), Arc::from("y")])
    );
}

#[test]
fn caret_return() {
    let prog = parse("^ 1");
    assert_eq!(prog.stmts[0], Stmt::Return(Expr::Lit(Literal::Int(1))));
}

#[test]
fn cascade() {
    // `a foo; bar` → Cascade with two messages
    let e = expr("a foo; bar");
    match e {
        Expr::Cascade { messages, .. } => {
            assert_eq!(messages.len(), 2);
        }
        _ => panic!("expected cascade"),
    }
}

#[test]
fn block_no_params() {
    let e = expr("[42]");
    match e {
        Expr::Block { params, body, .. } => {
            assert!(params.is_empty());
            assert_eq!(body.len(), 1);
        }
        _ => panic!("expected block"),
    }
}

#[test]
fn block_with_params() {
    let e = expr("[:x | x + 1]");
    match e {
        Expr::Block { params, .. } => {
            assert_eq!(params, vec![Arc::from("x")]);
        }
        _ => panic!("expected block"),
    }
}

#[test]
fn comments_collected() {
    let prog = parse("1 \"a comment\" + 2");
    assert_eq!(prog.comments, vec![Arc::from("a comment")]);
}

#[test]
fn unary_chain() {
    // `3 factorial negated` → two unary sends, left-to-right
    let e = expr("3 factorial negated");
    match &e {
        Expr::Send {
            receiver,
            message: Message::Unary(name),
        } => {
            assert_eq!(name.as_ref(), "negated");
            match receiver.as_ref() {
                Expr::Send {
                    message: Message::Unary(inner),
                    ..
                } => {
                    assert_eq!(inner.as_ref(), "factorial");
                }
                _ => panic!("expected inner unary send"),
            }
        }
        _ => panic!("expected unary send"),
    }
}

#[test]
fn precedence_unary_over_binary() {
    // `3 factorial + 1` — unary binds tighter than binary
    let e = expr("3 factorial + 1");
    match e {
        Expr::Send {
            receiver,
            message: Message::Binary { op, .. },
        } => {
            assert_eq!(op.as_ref(), "+");
            // receiver should be `3 factorial`
            match *receiver {
                Expr::Send {
                    message: Message::Unary(name),
                    ..
                } => {
                    assert_eq!(name.as_ref(), "factorial");
                }
                _ => panic!("expected unary receiver"),
            }
        }
        _ => panic!("expected binary send"),
    }
}

#[test]
fn method_def_unary() {
    let prog = parse("Counter >> increment [ count := count + 1 ]");
    match &prog.stmts[0] {
        Stmt::MethodDef(def) => {
            assert_eq!(def.class_name.as_ref(), "Counter");
            assert_eq!(def.pattern, MethodPattern::Unary(Arc::from("increment")));
            assert!(def.locals.is_empty());
        }
        _ => panic!("expected MethodDef"),
    }
}

#[test]
fn method_def_binary() {
    let prog = parse("Vec >> + other [ ^self ]");
    match &prog.stmts[0] {
        Stmt::MethodDef(def) => {
            assert_eq!(def.class_name.as_ref(), "Vec");
            assert_eq!(
                def.pattern,
                MethodPattern::Binary {
                    op: Arc::from("+"),
                    param: Arc::from("other")
                }
            );
            assert_eq!(def.pattern.selector().as_ref(), "+");
            assert_eq!(def.pattern.params(), vec![Arc::from("other")]);
        }
        _ => panic!("expected MethodDef"),
    }
}

#[test]
fn method_def_keyword_single() {
    let prog = parse("Counter >> incrementBy: n [ count := count + n ]");
    match &prog.stmts[0] {
        Stmt::MethodDef(def) => {
            assert_eq!(def.class_name.as_ref(), "Counter");
            assert_eq!(def.pattern.selector().as_ref(), "incrementBy:");
            assert_eq!(def.pattern.params(), vec![Arc::from("n")]);
        }
        _ => panic!("expected MethodDef"),
    }
}

#[test]
fn method_def_keyword_multi() {
    let prog = parse("Coll >> at: i put: v [ ]");
    match &prog.stmts[0] {
        Stmt::MethodDef(def) => {
            assert_eq!(def.pattern.selector().as_ref(), "at:put:");
            assert_eq!(def.pattern.params(), vec![Arc::from("i"), Arc::from("v")]);
        }
        _ => panic!("expected MethodDef"),
    }
}

#[test]
fn method_def_with_locals() {
    let prog = parse("Foo >> bar [ | tmp | tmp := 1. ^tmp ]");
    match &prog.stmts[0] {
        Stmt::MethodDef(def) => {
            assert_eq!(def.locals, vec![Arc::from("tmp")]);
            assert_eq!(def.body.len(), 2); // Assign + Return
        }
        _ => panic!("expected MethodDef"),
    }
}

#[test]
fn method_def_with_return() {
    let prog = parse("Counter >> count [ ^count ]");
    match &prog.stmts[0] {
        Stmt::MethodDef(def) => {
            assert_eq!(def.class_name.as_ref(), "Counter");
            assert_eq!(def.body.len(), 1);
            assert!(matches!(def.body[0], Stmt::Return(_)));
        }
        _ => panic!("expected MethodDef"),
    }
}

// ── Non-local return validation ───────────────────────────────────────────

#[test]
fn caret_in_toplevel_block_is_parse_error() {
    let err = Parser::parse("[^42] value").unwrap_err();
    assert_eq!(err, ParseError::NonLocalReturnOutsideMethod);
}

#[test]
fn caret_in_nested_toplevel_block_is_parse_error() {
    let err = Parser::parse("[[^42]] value value").unwrap_err();
    assert_eq!(err, ParseError::NonLocalReturnOutsideMethod);
}

#[test]
fn caret_inside_method_block_is_valid() {
    // `^` inside a block that's inside a method body is a legitimate
    // non-local return targeting the method frame.
    let prog = parse("Foo >> run [ [^42] value ]");
    assert!(matches!(prog.stmts[0], Stmt::MethodDef(_)));
}

#[test]
fn caret_directly_in_method_body_is_valid() {
    let prog = parse("Foo >> count [ ^count ]");
    assert!(matches!(prog.stmts[0], Stmt::MethodDef(_)));
}

// ── Primary literal variants ──────────────────────────────────────────────

#[test]
#[allow(clippy::approx_constant)]
fn float_literal() {
    assert_eq!(expr("3.14"), Expr::Lit(Literal::Float(3.14)));
}

#[test]
fn bool_and_nil_literals() {
    assert_eq!(expr("true"), Expr::Lit(Literal::Bool(true)));
    assert_eq!(expr("false"), Expr::Lit(Literal::Bool(false)));
    assert_eq!(expr("nil"), Expr::Lit(Literal::Nil));
}

#[test]
fn super_keyword_parsed() {
    assert_eq!(expr("super"), Expr::Super);
}

#[test]
fn parenthesized_expression() {
    assert_eq!(expr("(42)"), Expr::Lit(Literal::Int(42)));
}

// ── Namespace resolution ──────────────────────────────────────────────────

#[test]
fn namespace_ref_single_level() {
    let e = expr("A::B");
    assert_eq!(
        e,
        Expr::NamespaceRef {
            namespace_path: vec![Arc::from("A")],
            name: Arc::from("B")
        }
    );
}

#[test]
fn namespace_ref_deep_path() {
    let e = expr("A::B::C");
    assert_eq!(
        e,
        Expr::NamespaceRef {
            namespace_path: vec![Arc::from("A"), Arc::from("B")],
            name: Arc::from("C"),
        }
    );
}

#[test]
fn unexpected_token_after_colons_is_error() {
    let err = Parser::parse("A:: 123").unwrap_err();
    assert!(matches!(
        err,
        ParseError::UnexpectedToken(_, "identifier after ::")
    ));
}

// ── Array literals ────────────────────────────────────────────────────────

#[test]
fn array_literal_empty_in_parser() {
    let e = expr("#()");
    assert_eq!(e, Expr::ArrayLiteral(vec![]));
}

#[test]
fn array_literal_bool_and_nil_elements() {
    let e = expr("#(true false nil)");
    assert_eq!(
        e,
        Expr::ArrayLiteral(vec![
            Literal::Bool(true),
            Literal::Bool(false),
            Literal::Nil,
        ])
    );
}

#[test]
fn array_literal_nested() {
    let e = expr("#(#(1 2) 3)");
    assert_eq!(
        e,
        Expr::ArrayLiteral(vec![
            Literal::Array(vec![Literal::Int(1), Literal::Int(2)]),
            Literal::Int(3),
        ])
    );
}

#[test]
fn array_literal_invalid_element_is_error() {
    // `[` is not a valid literal element inside an array literal.
    let err = Parser::parse("#([])").unwrap_err();
    assert!(matches!(
        err,
        ParseError::UnexpectedToken(_, "literal element in array literal")
    ));
}

// ── Error cases ───────────────────────────────────────────────────────────

#[test]
fn unexpected_eof_as_primary_is_error() {
    let err = Parser::parse("(").unwrap_err();
    assert!(matches!(err, ParseError::UnexpectedEof(_)));
}

#[test]
fn unexpected_token_as_primary_is_error() {
    // `]` is not a valid start of a primary expression.
    let err = Parser::parse("]").unwrap_err();
    assert!(matches!(err, ParseError::UnexpectedToken(_, "expression")));
}

#[test]
fn method_def_with_comment_between_class_and_arrow() {
    let prog = parse("Counter \"note\" >> inc [ ]");
    assert!(matches!(prog.stmts[0], Stmt::MethodDef(_)));
}

// ── expect() EOF and UnexpectedToken paths ────────────────────────────────

#[test]
fn unclosed_array_literal_gives_unexpected_eof() {
    // `expect("')' to close array literal")` receives EOF.
    let err = Parser::parse("#(1 2").unwrap_err();
    assert!(matches!(err, ParseError::UnexpectedEof(_)));
}

#[test]
fn unclosed_paren_with_extra_token_gives_unexpected_token() {
    // `expect("')'")` receives `3` instead.
    let err = Parser::parse("(1 + 2 3)").unwrap_err();
    assert!(matches!(err, ParseError::UnexpectedToken(_, _)));
}

// ── Assignment comment lookahead (lines 143-145) ──────────────────────────

#[test]
fn assignment_with_comment_between_ident_and_assign() {
    // Comment token between variable and `:=` is skipped during lookahead.
    let prog = parse("x \"a note\" := 5");
    assert!(matches!(prog.stmts[0], Stmt::Assign { .. }));
}

// ── Method def invalid pattern (lines 206-207) ────────────────────────────

#[test]
fn method_def_invalid_pattern_errors() {
    let err = Parser::parse("Foo >> 123 [ ]").unwrap_err();
    assert!(matches!(
        err,
        ParseError::UnexpectedToken(_, "method pattern")
    ));
}

// ── Cascade binary and keyword messages (lines 300-311) ───────────────────

#[test]
fn cascade_binary_message() {
    let e = expr("3 yourself; + 1");
    assert!(matches!(e, Expr::Cascade { .. }));
}

#[test]
fn cascade_keyword_message() {
    let e = expr("3 yourself; max: 5");
    assert!(matches!(e, Expr::Cascade { .. }));
}

#[test]
fn cascade_invalid_message_after_semi_errors() {
    // `123` after `;` is not a valid cascade message.
    let err = Parser::parse("3 asString; 123").unwrap_err();
    assert!(matches!(
        err,
        ParseError::UnexpectedToken(_, "cascade message")
    ));
}

#[test]
fn cascade_non_send_before_semi_errors() {
    // Bare literal before `;` is not a message send.
    let err = Parser::parse("3; + 1").unwrap_err();
    assert!(matches!(err, ParseError::UnexpectedToken(Token::Semi, _)));
}

// ── Unary level: comment in assign lookahead (lines 339-340, 344) ─────────

#[test]
fn unary_level_skips_comment_in_assign_lookahead() {
    // `3 factorial` is parsed as a unary send; `x "note" := 5` is a separate
    // assignment statement.  The comment between `x` and `:=` exercises the
    // comment-skip loop inside the assign-lookahead check (lines 339-340, 344).
    let prog = parse("3 factorial x \"a note\" := 5");
    assert_eq!(prog.stmts.len(), 2);
    assert!(matches!(prog.stmts[0], Stmt::Expr(_)));
    assert!(matches!(prog.stmts[1], Stmt::Assign { .. }));
}

// ── Array literal: Float and LargeInt elements (lines 423, 429) ───────────

#[test]
#[allow(clippy::approx_constant)]
fn array_literal_float_elements() {
    let e = expr("#(1.5 3.14)");
    assert_eq!(
        e,
        Expr::ArrayLiteral(vec![Literal::Float(1.5), Literal::Float(3.14)])
    );
}

#[test]
fn array_literal_largeint_element() {
    use num_bigint::BigInt;
    use std::str::FromStr;
    use std::sync::Arc;
    let e = expr("#(99999999999999999999)");
    assert_eq!(
        e,
        Expr::ArrayLiteral(vec![Literal::LargeInt(Arc::new(
            BigInt::from_str("99999999999999999999").unwrap()
        ))])
    );
}

// ── NonLocalBlockAssignment ───────────────────────────────────────────────

#[test]
fn block_local_assignment_is_legal() {
    // Assigning to a block param or local is always legal.
    assert!(Parser::parse("[:x | x := 42. x]").is_ok());
    assert!(Parser::parse("[| n | n := 1. n]").is_ok());
    assert!(Parser::parse("[:x | | t | t := x + 1. t]").is_ok());
}

#[test]
fn block_assignment_to_method_temp_is_error() {
    // Assigning to a method temp from inside a block is rejected.
    let err = Parser::parse("Object >> m [ | n | [n := 1] value ]");
    assert!(
        matches!(err, Err(ParseError::NonLocalBlockAssignment(ref name)) if name.as_ref() == "n"),
        "expected NonLocalBlockAssignment(\"n\"), got {err:?}"
    );
}

#[test]
fn block_assignment_to_outer_block_temp_is_error() {
    // Assigning to an outer block's local from an inner block is rejected.
    let err = Parser::parse("Object >> m [ [| n | [n := 1] value] value ]");
    assert!(
        matches!(err, Err(ParseError::NonLocalBlockAssignment(ref name)) if name.as_ref() == "n"),
        "expected NonLocalBlockAssignment(\"n\"), got {err:?}"
    );
}

#[test]
fn nested_block_each_may_assign_own_locals() {
    // Each nesting level may assign only to its own scope — both legal here.
    assert!(Parser::parse("Object >> m [ [:x | | t | t := x. t] value: 1 ]").is_ok());
}

#[test]
fn method_body_inside_namespace_do_may_assign_ivars() {
    // A method defined inside a Namespace do: block must be able to assign ivars.
    assert!(Parser::parse(
        "MyNS do: [ Value subclass: #Foo instanceVariableNames: 'val'. Foo >> set: v [ val := v ] ]"
    )
    .is_ok());
}
