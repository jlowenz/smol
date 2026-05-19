use std::sync::Arc;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals
    Int(i64),
    Float(f64),
    /// Single-quoted string: `'hello'`.
    Str(Arc<str>),
    /// `#word` or `#'arbitrary string'`.
    Symbol(Arc<str>),
    True,
    False,
    Nil,
    // super
    Super,
    /// Integer literal that overflows `i64`.
    LargeInt(Arc<num_bigint::BigInt>),
    // Identifiers
    /// Starts with a letter; may contain letters, digits, `_`.
    Ident(Arc<str>),
    /// Identifier immediately followed by `:` (but not `:=`).
    Keyword(Arc<str>),
    // Operators / punctuation
    /// Binary operator characters: `+ - * / < > <= >= = ~= , @ \ // >>`
    BinOp(Arc<str>),
    Assign,      // :=
    ColonColon,  // ::
    Caret,       // ^
    Dot,         // .
    Semi,        // ;
    Pipe,        // |
    LParen,      // (
    RParen,      // )
    LBracket,    // [
    RBracket,    // ]
    /// Block parameter: `:name` inside `[...]`. The leading `:` is consumed by
    /// the lexer when it sees `:` followed by an alphabetic character and no `=`.
    BlockParam(Arc<str>),
    /// `#(` — opening delimiter of an array literal.
    ArrayLiteralOpen,
    // Trivia — preserved for future AST attachment; skipped by the parser.
    /// Contents of a `"..."` comment, without delimiters.
    Comment(Arc<str>),
    Eof,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum LexError {
    #[error("unexpected character {0:?} at position {1}")]
    UnexpectedChar(char, usize),
    #[error("unterminated string starting at position {0}")]
    UnterminatedString(usize),
    #[error("unterminated comment starting at position {0}")]
    UnterminatedComment(usize),
    #[error("invalid number literal at position {0}")]
    InvalidNumber(usize),
}

pub struct Lexer<'src> {
    src: &'src str,
    chars: std::iter::Peekable<std::str::CharIndices<'src>>,
    pos: usize,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Self {
        Self {
            src,
            chars: src.char_indices().peekable(),
            pos: 0,
        }
    }

    /// Tokenize the entire source into a `Vec<Token>`.
    ///
    /// Ensures: the last token is always `Token::Eof`.
    pub fn tokenize(src: &'src str) -> Result<Vec<Token>, LexError> {
        let mut lexer = Self::new(src);
        let mut tokens = Vec::new();
        loop {
            let tok = lexer.next_token()?;
            let done = tok == Token::Eof;
            tokens.push(tok);
            if done {
                break;
            }
        }
        Ok(tokens)
    }

    fn peek_char(&mut self) -> Option<char> {
        self.chars.peek().map(|(_, c)| *c)
    }

    fn advance(&mut self) -> Option<char> {
        let (pos, c) = self.chars.next()?;
        self.pos = pos + c.len_utf8();
        Some(c)
    }

    fn skip_whitespace(&mut self) {
        while self.peek_char().map(|c| c.is_ascii_whitespace()).unwrap_or(false) {
            self.advance();
        }
    }

    fn read_comment(&mut self, start: usize) -> Result<Token, LexError> {
        // Opening `"` already consumed.
        let mut s = String::new();
        loop {
            match self.advance() {
                None => return Err(LexError::UnterminatedComment(start)),
                Some('"') => return Ok(Token::Comment(Arc::from(s.as_str()))),
                Some(c) => s.push(c),
            }
        }
    }

    fn read_string(&mut self, start: usize) -> Result<Token, LexError> {
        // Opening `'` already consumed.
        let mut s = String::new();
        loop {
            match self.advance() {
                None => return Err(LexError::UnterminatedString(start)),
                Some('\'') => {
                    // Smalltalk escapes a literal quote by doubling it: `'it''s'`.
                    if self.peek_char() == Some('\'') {
                        self.advance();
                        s.push('\'');
                    } else {
                        return Ok(Token::Str(Arc::from(s.as_str())));
                    }
                }
                Some(c) => s.push(c),
            }
        }
    }

    fn read_symbol_quoted(&mut self, start: usize) -> Result<Token, LexError> {
        // `#'` — the `#` and `'` already consumed.
        let mut s = String::new();
        loop {
            match self.advance() {
                None => return Err(LexError::UnterminatedString(start)),
                Some('\'') => {
                    if self.peek_char() == Some('\'') {
                        self.advance();
                        s.push('\'');
                    } else {
                        return Ok(Token::Symbol(Arc::from(s.as_str())));
                    }
                }
                Some(c) => s.push(c),
            }
        }
    }

    fn read_number(&mut self, first: char, start: usize) -> Result<Token, LexError> {
        let mut s = String::new();
        s.push(first);
        while self.peek_char().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            s.push(self.advance().unwrap());
        }
        // Check for decimal point — only if followed by a digit (i.e. not statement terminator `.`).
        if self.peek_char() == Some('.') {
            // Look two chars ahead: if the char after `.` is a digit, it's a float.
            let rest = &self.src[self.pos..];
            let mut it = rest.chars();
            it.next(); // the dot
            if it.next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                s.push(self.advance().unwrap()); // consume `.`
                while self.peek_char().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    s.push(self.advance().unwrap());
                }
                // Optional exponent.
                if self.peek_char() == Some('e') || self.peek_char() == Some('E') {
                    s.push(self.advance().unwrap());
                    if self.peek_char() == Some('+') || self.peek_char() == Some('-') {
                        s.push(self.advance().unwrap());
                    }
                    if !self.peek_char().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                        return Err(LexError::InvalidNumber(start));
                    }
                    while self.peek_char().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                        s.push(self.advance().unwrap());
                    }
                }
                return s.parse::<f64>().map(Token::Float).map_err(|_| LexError::InvalidNumber(start));
            }
        }
        match s.parse::<i64>() {
            Ok(n) => Ok(Token::Int(n)),
            Err(_) => {
                // Only overflows; the string contains only digits so BigInt parse can't fail.
                let big: num_bigint::BigInt = s.parse().map_err(|_| LexError::InvalidNumber(start))?;
                Ok(Token::LargeInt(Arc::new(big)))
            }
        }
    }

    fn read_ident_or_keyword(&mut self, first: char) -> Token {
        let mut s = String::new();
        s.push(first);
        while self
            .peek_char()
            .map(|c| c.is_alphanumeric() || c == '_')
            .unwrap_or(false)
        {
            s.push(self.advance().unwrap());
        }
        // Keyword: identifier immediately followed by `:` but not `:=` or `::`.
        if self.peek_char() == Some(':') {
            let rest = &self.src[self.pos..];
            let second = rest.chars().nth(1);
            if second != Some('=') && second != Some(':') {
                self.advance(); // consume `:`
                s.push(':');
                return Token::Keyword(Arc::from(s.as_str()));
            }
        }
        match s.as_str() {
            "true" => Token::True,
            "false" => Token::False,
            "nil" => Token::Nil,
            "super" => Token::Super,
            _ => Token::Ident(Arc::from(s.as_str())),
        }
    }

    fn read_binop(&mut self, first: char) -> Token {
        let mut s = String::new();
        s.push(first);
        // Multi-char operators: `<=`, `>=`, `>>`, `~=`, `//`, `\\`, `~~`, `->`.
        if let Some(next) = self.peek_char() {
            let two: bool = match (first, next) {
                ('<', '=') | ('>', '=') | ('>', '>') | ('~', '=') | ('/', '/') | ('\\', '\\')
                | ('-', '>') | ('~', '~') => true,
                _ => false,
            };
            if two {
                s.push(self.advance().unwrap());
            }
        }
        Token::BinOp(Arc::from(s.as_str()))
    }

    fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_whitespace();
        let start = self.pos;
        let c = match self.advance() {
            None => return Ok(Token::Eof),
            Some(c) => c,
        };

        match c {
            '"' => self.read_comment(start),
            '\'' => self.read_string(start),
            '#' => {
                match self.peek_char() {
                    Some('\'') => {
                        self.advance();
                        self.read_symbol_quoted(start)
                    }
                    Some(ch) if ch.is_alphabetic() || ch == '_' => {
                        // #word
                        let first = self.advance().unwrap();
                        let mut s = String::new();
                        s.push(first);
                        while self
                            .peek_char()
                            .map(|c| c.is_alphanumeric() || c == '_' || c == ':')
                            .unwrap_or(false)
                        {
                            // Allow colons inside symbol names (e.g. #at:put:)
                            // but stop before `:`-only sequences that look like
                            // keyword starts.
                            s.push(self.advance().unwrap());
                        }
                        Ok(Token::Symbol(Arc::from(s.as_str())))
                    }
                    Some('(') => {
                        self.advance();
                        Ok(Token::ArrayLiteralOpen)
                    }
                    _ => Ok(self.read_binop('#')),
                }
            }
            ':' => {
                if self.peek_char() == Some('=') {
                    self.advance();
                    Ok(Token::Assign)
                } else if self.peek_char() == Some(':') {
                    self.advance();
                    Ok(Token::ColonColon)
                } else if self.peek_char().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false) {
                    // Block parameter: `:name` inside `[...]`.
                    let first = self.advance().unwrap();
                    let mut s = String::new();
                    s.push(first);
                    while self.peek_char().map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false) {
                        s.push(self.advance().unwrap());
                    }
                    Ok(Token::BlockParam(Arc::from(s.as_str())))
                } else {
                    Err(LexError::UnexpectedChar(':', start))
                }
            }
            '^' => Ok(Token::Caret),
            '.' => Ok(Token::Dot),
            ';' => Ok(Token::Semi),
            '|' => Ok(Token::Pipe),
            '(' => Ok(Token::LParen),
            ')' => Ok(Token::RParen),
            '[' => Ok(Token::LBracket),
            ']' => Ok(Token::RBracket),
            c if c.is_ascii_digit() => self.read_number(c, start),
            c if c.is_alphabetic() || c == '_' => Ok(self.read_ident_or_keyword(c)),
            '-' => {
                // A `-` immediately followed by a digit (no whitespace) is a negative
                // number literal, per ANSI Smalltalk. The sign is folded into the token
                // so `Int(-3)`, `Float(-1.5)`, etc. are produced directly.
                if self.peek_char().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    self.read_number('-', start)
                } else {
                    Ok(self.read_binop('-'))
                }
            }
            '+' | '*' | '/' | '\\' | '<' | '>' | '=' | '~' | '@' | ',' | '&' => {
                Ok(self.read_binop(c))
            }
            c => Err(LexError::UnexpectedChar(c, start)),
        }
    }
}

#[cfg(test)]
mod tests {
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
    fn float_literal() {
        assert_eq!(tok("3.14"), vec![Token::Float(3.14), Token::Eof]);
    }

    #[test]
    fn float_with_exponent() {
        assert_eq!(tok("1.5e2"), vec![Token::Float(150.0), Token::Eof]);
    }

    #[test]
    fn negative_integer_literal() {
        assert_eq!(tok("-42"), vec![Token::Int(-42), Token::Eof]);
    }

    #[test]
    fn negative_float_literal() {
        assert_eq!(tok("-3.14"), vec![Token::Float(-3.14), Token::Eof]);
    }

    #[test]
    fn negative_large_integer_literal() {
        use num_bigint::BigInt;
        use std::str::FromStr;
        let big = BigInt::from_str("-99999999999999999999").unwrap();
        assert_eq!(tok("-99999999999999999999"), vec![Token::LargeInt(Arc::new(big)), Token::Eof]);
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
        assert_eq!(
            tok("x -3"),
            vec![ident("x"), Token::Int(-3), Token::Eof]
        );
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
}
