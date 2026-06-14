use super::*;

fn tok(src: &str) -> Vec<Token> {
    Lexer::tokenize(src).expect("lex error")
}

fn ident(s: &str) -> Token {
    Token::Ident(Arc::from(s))
}
fn kw(s: &str) -> Token {
    Token::Keyword(Arc::from(s))
}
fn sym(s: &str) -> Token {
    Token::Symbol(Arc::from(s))
}
fn binop(s: &str) -> Token {
    Token::BinOp(Arc::from(s))
}
fn str_tok(s: &str) -> Token {
    Token::Str(Arc::from(s))
}

#[test]
fn integer_literal() {
    assert_eq!(tok("42"), vec![Token::Int(42), Token::Eof]);
}

#[test]
#[allow(clippy::approx_constant)]
fn float_literal() {
    assert_eq!(tok("3.14"), vec![Token::Float(3.14), Token::Eof]);
}

#[test]
fn float_with_exponent() {
    assert_eq!(tok("1.5e2"), vec![Token::Float(150.0), Token::Eof]);
}

#[test]
fn float_with_signed_exponent() {
    assert_eq!(tok("1.5e+2"), vec![Token::Float(150.0), Token::Eof]);
    assert_eq!(tok("2.0e-1"), vec![Token::Float(0.2), Token::Eof]);
}

#[test]
fn float_exponent_missing_digits_errors() {
    // `1.5e` followed by a non-digit — LexError::InvalidNumber.
    let err = Lexer::tokenize("1.5e!").unwrap_err();
    assert!(matches!(err, LexError::InvalidNumber(_)));
}

#[test]
fn negative_integer_literal() {
    assert_eq!(tok("-42"), vec![Token::Int(-42), Token::Eof]);
}

#[test]
#[allow(clippy::approx_constant)]
fn negative_float_literal() {
    assert_eq!(tok("-3.14"), vec![Token::Float(-3.14), Token::Eof]);
}

#[test]
fn negative_large_integer_literal() {
    use num_bigint::BigInt;
    use std::str::FromStr;
    let big = BigInt::from_str("-99999999999999999999").unwrap();
    assert_eq!(
        tok("-99999999999999999999"),
        vec![Token::LargeInt(Arc::new(big)), Token::Eof]
    );
}

#[test]
fn minus_with_space_stays_binop() {
    // `- 3` (space between) → BinOp, positive literal
    assert_eq!(tok("- 3"), vec![binop("-"), Token::Int(3), Token::Eof]);
}

#[test]
fn binary_minus_in_expression() {
    // `x - 3` → binary subtract; `x -3` → binary subtract (space before -, none after)
    assert_eq!(
        tok("x - 3"),
        vec![ident("x"), binop("-"), Token::Int(3), Token::Eof]
    );
    assert_eq!(tok("x -3"), vec![ident("x"), Token::Int(-3), Token::Eof]);
}

#[test]
fn string_literal() {
    assert_eq!(tok("'hello'"), vec![str_tok("hello"), Token::Eof]);
}

#[test]
fn string_escaped_quote() {
    assert_eq!(tok("'it''s'"), vec![str_tok("it's"), Token::Eof]);
}

#[test]
fn symbol_word() {
    assert_eq!(tok("#foo"), vec![sym("foo"), Token::Eof]);
}

#[test]
fn symbol_keyword_form() {
    assert_eq!(tok("#at:put:"), vec![sym("at:put:"), Token::Eof]);
}

#[test]
fn symbol_quoted() {
    assert_eq!(tok("#'hello world'"), vec![sym("hello world"), Token::Eof]);
}

#[test]
fn keywords_adjacent() {
    // `at: 1 put: 2` should lex as Keyword("at:"), Int(1), Keyword("put:"), Int(2).
    let tokens = tok("at: 1 put: 2");
    assert_eq!(tokens[0], kw("at:"));
    assert_eq!(tokens[1], Token::Int(1));
    assert_eq!(tokens[2], kw("put:"));
    assert_eq!(tokens[3], Token::Int(2));
}

#[test]
fn booleans_and_nil() {
    assert_eq!(tok("true"), vec![Token::True, Token::Eof]);
    assert_eq!(tok("false"), vec![Token::False, Token::Eof]);
    assert_eq!(tok("nil"), vec![Token::Nil, Token::Eof]);
}

#[test]
fn multi_char_binops() {
    assert_eq!(tok("<="), vec![binop("<="), Token::Eof]);
    assert_eq!(tok(">="), vec![binop(">="), Token::Eof]);
    assert_eq!(tok("~="), vec![binop("~="), Token::Eof]);
    assert_eq!(tok("//"), vec![binop("//"), Token::Eof]);
}

#[test]
fn assign_vs_keyword() {
    // `:=` is Assign; `x:` is a keyword.
    let tokens = tok("x := 1");
    assert_eq!(tokens[0], ident("x"));
    assert_eq!(tokens[1], Token::Assign);
    assert_eq!(tokens[2], Token::Int(1));
}

#[test]
fn comment_preserved() {
    let tokens = tok("1 \"add\" + 2");
    assert_eq!(tokens[0], Token::Int(1));
    assert_eq!(tokens[1], Token::Comment(Arc::from("add")));
    assert_eq!(tokens[2], binop("+"));
    assert_eq!(tokens[3], Token::Int(2));
}

#[test]
fn dot_after_integer_is_not_float() {
    // `3.` should lex as Int(3), Dot — not a float.
    let tokens = tok("3.");
    assert_eq!(tokens[0], Token::Int(3));
    assert_eq!(tokens[1], Token::Dot);
}

#[test]
fn cascade_semi() {
    let tokens = tok("a; b");
    assert_eq!(tokens[1], Token::Semi);
}

#[test]
fn block_brackets() {
    let tokens = tok("[x]");
    assert_eq!(tokens[0], Token::LBracket);
    assert_eq!(tokens[1], ident("x"));
    assert_eq!(tokens[2], Token::RBracket);
}

#[test]
fn block_params() {
    let tokens = tok("[:x :y | x]");
    assert_eq!(tokens[0], Token::LBracket);
    assert_eq!(tokens[1], Token::BlockParam(Arc::from("x")));
    assert_eq!(tokens[2], Token::BlockParam(Arc::from("y")));
    assert_eq!(tokens[3], Token::Pipe);
    assert_eq!(tokens[4], ident("x"));
}

#[test]
fn assign_not_block_param() {
    // `:=` must remain Token::Assign, not BlockParam.
    assert_eq!(tok(":="), vec![Token::Assign, Token::Eof]);
}

#[test]
fn large_integer_literal() {
    // A number that overflows i64::MAX
    let big = "99999999999999999999";
    let tokens = tok(big);
    assert!(matches!(&tokens[0], Token::LargeInt(_)));
}

#[test]
fn colon_colon_token() {
    let tokens = tok("::");
    assert_eq!(tokens[0], Token::ColonColon);
    assert_eq!(tokens[1], Token::Eof);
}
