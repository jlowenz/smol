use std::sync::Arc;

use thiserror::Error;

use crate::ast::*;
use crate::lexer::{LexError, Lexer, Token};

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ParseError {
    #[error("lex error: {0}")]
    Lex(#[from] LexError),
    #[error("unexpected token {0:?}, expected {1}")]
    UnexpectedToken(Token, &'static str),
    #[error("unexpected end of input, expected {0}")]
    UnexpectedEof(&'static str),
    #[error("invalid assignment target")]
    InvalidAssignTarget,
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    comments: Vec<Arc<str>>,
}

impl Parser {
    pub fn parse(src: &str) -> Result<Program, ParseError> {
        let tokens = Lexer::tokenize(src)?;
        let mut p = Parser { tokens, pos: 0, comments: Vec::new() };
        p.parse_program()
    }

    // ── Token stream helpers ──────────────────────────────────────────────────

    fn peek(&mut self) -> &Token {
        self.skip_comments();
        &self.tokens[self.pos]
    }

    fn skip_comments(&mut self) {
        while self.pos < self.tokens.len() {
            if let Token::Comment(c) = &self.tokens[self.pos] {
                self.comments.push(c.clone());
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn advance(&mut self) -> Token {
        self.skip_comments();
        let tok = self.tokens[self.pos].clone();
        if tok != Token::Eof {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, expected: &'static str, pred: impl Fn(&Token) -> bool) -> Result<Token, ParseError> {
        let tok = self.peek().clone();
        if pred(&tok) {
            Ok(self.advance())
        } else if tok == Token::Eof {
            Err(ParseError::UnexpectedEof(expected))
        } else {
            Err(ParseError::UnexpectedToken(tok, expected))
        }
    }

    fn eat(&mut self, target: &Token) -> bool {
        if self.peek() == target {
            self.advance();
            true
        } else {
            false
        }
    }

    // ── Grammar ───────────────────────────────────────────────────────────────

    fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut stmts = Vec::new();
        while self.peek() != &Token::Eof {
            // Allow local declarations (`| x y |`) anywhere at the statement level.
            if self.peek() == &Token::Pipe {
                stmts.push(self.parse_local_decl()?);
                self.eat(&Token::Dot);
                continue;
            }
            stmts.push(self.parse_stmt()?);
            self.eat(&Token::Dot);
        }
        Ok(Program { stmts, comments: std::mem::take(&mut self.comments) })
    }

    fn parse_local_decl(&mut self) -> Result<Stmt, ParseError> {
        self.expect("'|'", |t| t == &Token::Pipe)?;
        let mut names = Vec::new();
        while let Token::Ident(name) = self.peek().clone() {
            names.push(name);
            self.advance();
        }
        self.expect("closing '|'", |t| t == &Token::Pipe)?;
        Ok(Stmt::LocalDecl(names))
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        // `^` return
        if self.peek() == &Token::Caret {
            self.advance();
            let expr = self.parse_expr()?;
            return Ok(Stmt::Return(expr));
        }

        // Method def lookahead: IDENT '>>' (instance-side) or IDENT Ident("class") '>>' (class-side)
        {
            let is_method_def = matches!(self.tokens[self.pos], Token::Ident(_)) && {
                let mut i = self.pos + 1;
                while i < self.tokens.len() && matches!(self.tokens[i], Token::Comment(_)) {
                    i += 1;
                }
                // instance-side: IDENT >>
                let direct_arrow = matches!(self.tokens.get(i), Some(Token::BinOp(op)) if op.as_ref() == ">>");
                // class-side: IDENT Ident("class") >>
                let class_side_arrow = matches!(self.tokens.get(i), Some(Token::Ident(s)) if s.as_ref() == "class")
                    && matches!(self.tokens.get(i + 1), Some(Token::BinOp(op)) if op.as_ref() == ">>");
                direct_arrow || class_side_arrow
            };
            if is_method_def {
                return self.parse_method_def();
            }
        }

        // Assignment: lookahead — Ident followed immediately by Assign.
        // We must look at raw tokens (pre-comment-skip) so we don't over-consume.
        {
            let is_assign = matches!(self.tokens[self.pos], Token::Ident(_))
                && {
                    // Skip any comments between ident and next real token.
                    let mut i = self.pos + 1;
                    while i < self.tokens.len() {
                        if matches!(self.tokens[i], Token::Comment(_)) {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    matches!(self.tokens.get(i), Some(Token::Assign))
                };
            if is_assign {
                let name = match self.advance() {
                    Token::Ident(n) => n,
                    _ => unreachable!(),
                };
                self.skip_comments();
                self.advance(); // consume `:=`
                let value = self.parse_expr()?;
                return Ok(Stmt::Assign { name, value });
            }
        }
        Ok(Stmt::Expr(self.parse_expr()?))
    }

    fn parse_method_def(&mut self) -> Result<Stmt, ParseError> {
        // Consume the class name ident
        let class_name = match self.advance() {
            Token::Ident(n) => n,
            _ => unreachable!(),
        };
        // Detect class-side: `IDENT "class" >>`
        let is_class_side = matches!(self.peek(), Token::Ident(s) if s.as_ref() == "class")
            && matches!(self.tokens.get(self.pos + 1), Some(Token::BinOp(op)) if op.as_ref() == ">>");
        if is_class_side {
            self.advance(); // consume "class"
        }
        // Consume '>>'
        self.advance(); // BinOp(">>")

        // Parse the method pattern
        let pattern = match self.peek().clone() {
            Token::Ident(sel) => {
                self.advance();
                MethodPattern::Unary(sel)
            }
            Token::BinOp(op) => {
                self.advance();
                let param = match self.expect("parameter name", |t| matches!(t, Token::Ident(_)))? {
                    Token::Ident(n) => n,
                    _ => unreachable!(),
                };
                MethodPattern::Binary { op, param }
            }
            Token::Keyword(_) => {
                let mut parts = Vec::new();
                while let Token::Keyword(kw) = self.peek().clone() {
                    self.advance();
                    let param = match self.expect("parameter name", |t| matches!(t, Token::Ident(_)))? {
                        Token::Ident(n) => n,
                        _ => unreachable!(),
                    };
                    parts.push((kw, param));
                }
                MethodPattern::Keyword { parts }
            }
            tok => {
                return Err(ParseError::UnexpectedToken(tok, "method pattern"));
            }
        };

        // Expect '['
        self.expect("'[' to open method body", |t| t == &Token::LBracket)?;

        // Optional local declaration inside method body
        let mut locals = Vec::new();
        if self.peek() == &Token::Pipe {
            self.advance(); // consume '|'
            while let Token::Ident(name) = self.peek().clone() {
                locals.push(name);
                self.advance();
            }
            self.expect("closing '|' for method locals", |t| t == &Token::Pipe)?;
        }

        // Statements until ']'
        let mut body = Vec::new();
        while self.peek() != &Token::RBracket && self.peek() != &Token::Eof {
            body.push(self.parse_stmt()?);
            self.eat(&Token::Dot);
        }
        self.expect("']'", |t| t == &Token::RBracket)?;

        Ok(Stmt::MethodDef(MethodDef { class_name, pattern, locals, body, is_class_side }))
    }

    // ── Precedence levels ─────────────────────────────────────────────────────
    //
    // Three levels, lowest to highest: keyword → binary → unary → primary.
    // Each level calls the next unconditionally, then checks for operators of
    // its own level. When none are present the result from the next level is
    // returned unchanged — the "fallthrough."

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_keyword_level()
    }

    fn parse_keyword_level(&mut self) -> Result<Expr, ParseError> {
        let mut receiver = self.parse_binary_level()?;
        // Collect keyword parts greedily.
        if matches!(self.peek(), Token::Keyword(_)) {
            let mut parts = Vec::new();
            while let Token::Keyword(kw) = self.peek().clone() {
                self.advance();
                let arg = self.parse_binary_level()?;
                parts.push((kw, arg));
            }
            receiver = Expr::Send {
                receiver: Box::new(receiver),
                message: Message::Keyword { parts },
            };
        }
        // Cascade: `;` continues the same receiver.
        if self.peek() == &Token::Semi {
            return self.parse_cascade(receiver);
        }
        Ok(receiver)
    }

    fn parse_cascade(&mut self, first_receiver: Expr) -> Result<Expr, ParseError> {
        // The first message has already been folded into `first_receiver` as a
        // `Send`. Unwrap it so we have the bare receiver + first message.
        let (receiver, first_msg) = match first_receiver {
            Expr::Send { receiver, message } => (*receiver, message),
            _ => return Err(ParseError::UnexpectedToken(Token::Semi, "message send before ';'")),
        };
        let mut messages = vec![first_msg];
        while self.eat(&Token::Semi) {
            messages.push(self.parse_cascade_message()?);
        }
        Ok(Expr::Cascade { receiver: Box::new(receiver), messages })
    }

    fn parse_cascade_message(&mut self) -> Result<Message, ParseError> {
        // After `;` we expect one of: unary, binary, or keyword message.
        match self.peek().clone() {
            Token::Ident(_) => {
                // Unary.
                let name = match self.advance() {
                    Token::Ident(n) => n,
                    _ => unreachable!(),
                };
                Ok(Message::Unary(name))
            }
            Token::BinOp(_) => {
                // Binary.
                let op = match self.advance() {
                    Token::BinOp(op) => op,
                    _ => unreachable!(),
                };
                let arg = self.parse_unary_level()?;
                Ok(Message::Binary { op, arg: Box::new(arg) })
            }
            Token::Keyword(_) => {
                // Keyword — collect all parts.
                let mut parts = Vec::new();
                while let Token::Keyword(kw) = self.peek().clone() {
                    self.advance();
                    let arg = self.parse_binary_level()?;
                    parts.push((kw, arg));
                }
                Ok(Message::Keyword { parts })
            }
            tok => Err(ParseError::UnexpectedToken(tok, "cascade message")),
        }
    }

    fn parse_binary_level(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary_level()?;
        while let Token::BinOp(op) = self.peek().clone() {
            self.advance();
            let right = self.parse_unary_level()?;
            left = Expr::Send {
                receiver: Box::new(left),
                message: Message::Binary { op, arg: Box::new(right) },
            };
        }
        Ok(left)
    }

    fn parse_unary_level(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        // Unary messages: consecutive identifiers that are NOT followed by `:=`.
        while let Token::Ident(_) = self.peek() {
            // Peek at the raw token after the ident to avoid treating
            // `x := ...` as a unary send when we're at the ident position.
            let is_assign_lookahead = {
                let mut i = self.pos + 1;
                while i < self.tokens.len() && matches!(self.tokens[i], Token::Comment(_)) {
                    i += 1;
                }
                matches!(self.tokens.get(i), Some(Token::Assign))
            };
            if is_assign_lookahead {
                break;
            }
            let name = match self.advance() {
                Token::Ident(n) => n,
                _ => unreachable!(),
            };
            expr = Expr::Send {
                receiver: Box::new(expr),
                message: Message::Unary(name),
            };
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let tok = self.peek().clone();
        match tok {
            Token::Int(n) => { self.advance(); Ok(Expr::Lit(Literal::Int(n))) }
            Token::Float(f) => { self.advance(); Ok(Expr::Lit(Literal::Float(f))) }
            Token::True => { self.advance(); Ok(Expr::Lit(Literal::Bool(true))) }
            Token::False => { self.advance(); Ok(Expr::Lit(Literal::Bool(false))) }
            Token::Nil => { self.advance(); Ok(Expr::Lit(Literal::Nil)) }
            Token::Super => { self.advance(); Ok(Expr::Super) }
            Token::Str(s) => { self.advance(); Ok(Expr::Lit(Literal::Str(s))) }
            Token::Symbol(s) => { self.advance(); Ok(Expr::Lit(Literal::Symbol(s))) }
            Token::LargeInt(n) => { self.advance(); Ok(Expr::Lit(Literal::LargeInt(n))) }
            Token::Ident(name) => {
                self.advance();
                // Check for `::` namespace resolution chain.
                if self.peek() == &Token::ColonColon {
                    let mut path = vec![name];
                    while self.peek() == &Token::ColonColon {
                        self.advance(); // consume ::
                        match self.peek().clone() {
                            Token::Ident(next) => {
                                self.advance();
                                path.push(next);
                            }
                            tok => return Err(ParseError::UnexpectedToken(tok, "identifier after ::")),
                        }
                    }
                    let name = path.pop().unwrap();
                    return Ok(Expr::NamespaceRef { namespace_path: path, name });
                }
                Ok(Expr::Var(name))
            }
            Token::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect("')'", |t| t == &Token::RParen)?;
                Ok(expr)
            }
            Token::LBracket => self.parse_block(),
            Token::ArrayLiteralOpen => {
                self.advance();
                Ok(Expr::ArrayLiteral(self.parse_array_literal_elements()?))
            }
            Token::Eof => Err(ParseError::UnexpectedEof("expression")),
            tok => Err(ParseError::UnexpectedToken(tok, "expression")),
        }
    }

    /// Parse the element list of an array literal up to (but not consuming) `)`.
    ///
    /// Requires: the opening `#(` has already been consumed.
    /// Ensures: returns all `Literal` elements found before `)`.
    fn parse_array_literal_elements(&mut self) -> Result<Vec<Literal>, ParseError> {
        let mut elements = Vec::new();
        while self.peek() != &Token::RParen && self.peek() != &Token::Eof {
            elements.push(self.parse_literal_element()?);
        }
        self.expect("')' to close array literal", |t| t == &Token::RParen)?;
        Ok(elements)
    }

    fn parse_literal_element(&mut self) -> Result<Literal, ParseError> {
        let tok = self.peek().clone();
        match tok {
            Token::Int(n) =>     { self.advance(); Ok(Literal::Int(n)) }
            Token::Float(f) =>   { self.advance(); Ok(Literal::Float(f)) }
            Token::True =>       { self.advance(); Ok(Literal::Bool(true)) }
            Token::False =>      { self.advance(); Ok(Literal::Bool(false)) }
            Token::Nil =>        { self.advance(); Ok(Literal::Nil) }
            Token::Str(s) =>     { self.advance(); Ok(Literal::Str(s)) }
            Token::Symbol(s) =>  { self.advance(); Ok(Literal::Symbol(s)) }
            Token::LargeInt(n) =>{ self.advance(); Ok(Literal::LargeInt(n)) }
            Token::ArrayLiteralOpen => {
                self.advance();
                Ok(Literal::Array(self.parse_array_literal_elements()?))
            }
            tok => Err(ParseError::UnexpectedToken(tok, "literal element in array literal")),
        }
    }

    fn parse_block(&mut self) -> Result<Expr, ParseError> {
        self.expect("'['", |t| t == &Token::LBracket)?;
        let mut params = Vec::new();
        // Block parameters: `[:x :y | body]`. The lexer emits `BlockParam` for
        // `:name`; a bare `|` at the start means no params, only locals.
        while let Token::BlockParam(p) = self.peek().clone() {
            self.advance();
            params.push(p);
        }
        if !params.is_empty() {
            self.expect("'|' after block parameters", |t| t == &Token::Pipe)?;
        }
        // Optional local decl `| x y |` inside the block body.
        let mut locals = Vec::new();
        if self.peek() == &Token::Pipe {
            self.advance();
            while let Token::Ident(name) = self.peek().clone() {
                locals.push(name);
                self.advance();
            }
            self.expect("closing '|' for locals", |t| t == &Token::Pipe)?;
        }
        let mut body = Vec::new();
        while self.peek() != &Token::RBracket && self.peek() != &Token::Eof {
            body.push(self.parse_stmt()?);
            self.eat(&Token::Dot);
        }
        self.expect("']'", |t| t == &Token::RBracket)?;
        Ok(Expr::Block { params, locals, body })
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(expr("x"), Expr::Var(Arc::from("x")));
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
            Expr::Send { message: Message::Keyword { parts }, .. } => {
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
            Stmt::Assign { name: Arc::from("x"), value: Expr::Lit(Literal::Int(42)) }
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
            Expr::Send { receiver, message: Message::Unary(name) } => {
                assert_eq!(name.as_ref(), "negated");
                match receiver.as_ref() {
                    Expr::Send { message: Message::Unary(inner), .. } => {
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
            Expr::Send { receiver, message: Message::Binary { op, .. } } => {
                assert_eq!(op.as_ref(), "+");
                // receiver should be `3 factorial`
                match *receiver {
                    Expr::Send { message: Message::Unary(name), .. } => {
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
                    MethodPattern::Binary { op: Arc::from("+"), param: Arc::from("other") }
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
}
