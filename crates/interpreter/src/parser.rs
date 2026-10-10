use std::collections::HashSet;
use std::sync::Arc;

use thiserror::Error;

use crate::ast::*;
use crate::lexer::{LexError, Lexer, SpannedToken, Token};

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
    /// `^` inside a block that has no enclosing method. Non-local returns
    /// require a method frame to return to; without one the return has nowhere
    /// to go and is always a programming error.
    #[error("`^` (non-local return) inside a block requires an enclosing method")]
    NonLocalReturnOutsideMethod,
    /// Assignment inside a block to a variable not declared in that block's
    /// own params or locals. Only block-local names may be assigned from within
    /// a block; assigning to method temps or ivars from a block creates
    /// cross-entity mutation hazards.
    #[error("block assigns to `{0}`, which is not declared in this block's params or locals")]
    NonLocalBlockAssignment(Arc<str>),
}

pub struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
    comments: Vec<Arc<str>>,
    /// True while parsing a method body. Lets `^` inside nested blocks pass.
    in_method: bool,
    /// True while parsing any block body. Combined with `in_method` to detect
    /// orphaned non-local returns at parse time.
    in_block: bool,
    /// Stack of block-local name sets (params ∪ locals). One entry per active
    /// block nesting level. Assignment to a name not in the top entry is a
    /// `NonLocalBlockAssignment` error.
    block_scope_stack: Vec<HashSet<Arc<str>>>,
}

impl Parser {
    pub fn parse(src: &str) -> Result<Program, ParseError> {
        let tokens = Lexer::tokenize(src)?;
        let mut p = Parser {
            tokens,
            pos: 0,
            comments: Vec::new(),
            in_method: false,
            in_block: false,
            block_scope_stack: Vec::new(),
        };
        p.parse_program()
    }

    // ── Token stream helpers ──────────────────────────────────────────────────

    fn peek(&mut self) -> &Token {
        self.skip_comments();
        &self.tokens[self.pos].node
    }

    fn skip_comments(&mut self) {
        while self.pos < self.tokens.len() {
            if let Token::Comment(c) = &self.tokens[self.pos].node {
                self.comments.push(c.clone());
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn advance(&mut self) -> Token {
        self.skip_comments();
        let tok = self.tokens[self.pos].node.clone();
        if tok != Token::Eof {
            self.pos += 1;
        }
        tok
    }

    fn expect(
        &mut self,
        expected: &'static str,
        pred: impl Fn(&Token) -> bool,
    ) -> Result<Token, ParseError> {
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

    /// Wrap `node` with the source span covering tokens `start..=self.pos - 1`
    /// (the statement's first token through its last consumed token).
    ///
    /// Requires: `start < self.pos` (at least one token was consumed for `node`).
    fn tag_from(&self, start: usize, node: Stmt) -> TaggedStmt {
        let span = self.tokens[start].span.to(self.tokens[self.pos - 1].span);
        TaggedStmt::new(span, node)
    }

    // ── Grammar ───────────────────────────────────────────────────────────────

    fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut stmts = Vec::new();
        while self.peek() != &Token::Eof {
            let start = self.pos;
            // Allow local declarations (`| x y |`) anywhere at the statement level.
            let node = if self.peek() == &Token::Pipe {
                self.parse_local_decl()?
            } else {
                self.parse_stmt()?
            };
            stmts.push(self.tag_from(start, node));
            self.eat(&Token::Dot);
        }
        Ok(Program {
            stmts,
            comments: std::mem::take(&mut self.comments),
        })
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
        // `^` non-local return
        if self.peek() == &Token::Caret {
            self.advance();
            if self.in_block && !self.in_method {
                return Err(ParseError::NonLocalReturnOutsideMethod);
            }
            let expr = self.parse_expr()?;
            return Ok(Stmt::Return(expr));
        }

        // Method def lookahead: IDENT '>>' (instance-side) or IDENT Ident("class") '>>' (class-side)
        {
            let is_method_def = matches!(self.tokens[self.pos].node, Token::Ident(_))
                && {
                    let mut i = self.pos + 1;
                    while i < self.tokens.len() && matches!(self.tokens[i].node, Token::Comment(_))
                    {
                        i += 1;
                    }
                    // instance-side: IDENT >>
                    let direct_arrow = matches!(self.tokens.get(i).map(|t| &t.node), Some(Token::BinOp(op)) if op.as_ref() == ">>");
                    // class-side: IDENT Ident("class") >>
                    let class_side_arrow = matches!(self.tokens.get(i).map(|t| &t.node), Some(Token::Ident(s)) if s.as_ref() == "class")
                        && matches!(self.tokens.get(i + 1).map(|t| &t.node), Some(Token::BinOp(op)) if op.as_ref() == ">>");
                    direct_arrow || class_side_arrow
                };
            if is_method_def {
                return self.parse_method_def();
            }
        }

        // Assignment: lookahead — Ident followed immediately by Assign.
        // We must look at raw tokens (pre-comment-skip) so we don't over-consume.
        {
            let is_assign = matches!(self.tokens[self.pos].node, Token::Ident(_)) && {
                // Skip any comments between ident and next real token.
                let mut i = self.pos + 1;
                while i < self.tokens.len() {
                    if matches!(self.tokens[i].node, Token::Comment(_)) {
                        i += 1;
                    } else {
                        break;
                    }
                }
                matches!(self.tokens.get(i).map(|t| &t.node), Some(Token::Assign))
            };
            if is_assign {
                let name = match self.advance() {
                    Token::Ident(n) => n,
                    _ => unreachable!(),
                };
                if self
                    .block_scope_stack
                    .last()
                    .is_some_and(|scope| !scope.contains(&name))
                {
                    return Err(ParseError::NonLocalBlockAssignment(name));
                }
                self.skip_comments();
                self.advance(); // consume `:=`
                let value = self.parse_expr()?;
                return Ok(Stmt::Assign {
                    name,
                    res: VarRef::Unresolved,
                    value,
                });
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
            && matches!(self.tokens.get(self.pos + 1).map(|t| &t.node), Some(Token::BinOp(op)) if op.as_ref() == ">>");
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
            Token::Pipe => {
                self.advance();
                let param = match self.expect("parameter name", |t| matches!(t, Token::Ident(_)))? {
                    Token::Ident(n) => n,
                    _ => unreachable!(),
                };
                MethodPattern::Binary {
                    op: Arc::from("|"),
                    param,
                }
            }
            Token::Keyword(_) => {
                let mut parts = Vec::new();
                while let Token::Keyword(kw) = self.peek().clone() {
                    self.advance();
                    let param =
                        match self.expect("parameter name", |t| matches!(t, Token::Ident(_)))? {
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

        // Statements until ']' — `^` is valid here and in any nested blocks.
        // A method body is a new scope root: block assignments inside the method
        // are validated against the method's own block nesting, not any enclosing
        // block scope (e.g., a method defined inside a `Namespace do:` block).
        let prev_in_method = self.in_method;
        let prev_in_block = self.in_block;
        let prev_scope_stack = std::mem::take(&mut self.block_scope_stack);
        self.in_method = true;
        self.in_block = false;
        let mut body = Vec::new();
        while self.peek() != &Token::RBracket && self.peek() != &Token::Eof {
            let start = self.pos;
            let node = self.parse_stmt()?;
            body.push(self.tag_from(start, node));
            self.eat(&Token::Dot);
        }
        self.in_method = prev_in_method;
        self.in_block = prev_in_block;
        self.block_scope_stack = prev_scope_stack;
        self.expect("']'", |t| t == &Token::RBracket)?;

        Ok(Stmt::MethodDef(MethodDef {
            class_name,
            pattern,
            locals,
            body,
            is_class_side,
        }))
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
            _ => {
                return Err(ParseError::UnexpectedToken(
                    Token::Semi,
                    "message send before ';'",
                ));
            }
        };
        let mut messages = vec![first_msg];
        while self.eat(&Token::Semi) {
            messages.push(self.parse_cascade_message()?);
        }
        Ok(Expr::Cascade {
            receiver: Box::new(receiver),
            messages,
        })
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
                Ok(Message::Binary {
                    op,
                    arg: Box::new(arg),
                })
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
        loop {
            // `Token::Pipe` is also valid as the binary `|` operator in expression
            // position; the local-decl uses of `|` are consumed before this level
            // is reached (in `parse_program` and `parse_block`).
            let op: Arc<str> = match self.peek().clone() {
                Token::BinOp(op) => {
                    self.advance();
                    op
                }
                Token::Pipe => {
                    self.advance();
                    Arc::from("|")
                }
                _ => break,
            };
            let right = self.parse_unary_level()?;
            left = Expr::Send {
                receiver: Box::new(left),
                message: Message::Binary {
                    op,
                    arg: Box::new(right),
                },
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
                while i < self.tokens.len() && matches!(self.tokens[i].node, Token::Comment(_)) {
                    i += 1;
                }
                matches!(self.tokens.get(i).map(|t| &t.node), Some(Token::Assign))
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
            Token::Int(n) => {
                self.advance();
                Ok(Expr::Lit(Literal::Int(n)))
            }
            Token::Float(f) => {
                self.advance();
                Ok(Expr::Lit(Literal::Float(f)))
            }
            Token::True => {
                self.advance();
                Ok(Expr::Lit(Literal::Bool(true)))
            }
            Token::False => {
                self.advance();
                Ok(Expr::Lit(Literal::Bool(false)))
            }
            Token::Nil => {
                self.advance();
                Ok(Expr::Lit(Literal::Nil))
            }
            Token::Super => {
                self.advance();
                Ok(Expr::Super)
            }
            Token::Str(s) => {
                self.advance();
                Ok(Expr::Lit(Literal::Str(s)))
            }
            Token::Symbol(s) => {
                self.advance();
                Ok(Expr::Lit(Literal::Symbol(s)))
            }
            Token::LargeInt(n) => {
                self.advance();
                Ok(Expr::Lit(Literal::LargeInt(n)))
            }
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
                            tok => {
                                return Err(ParseError::UnexpectedToken(
                                    tok,
                                    "identifier after ::",
                                ));
                            }
                        }
                    }
                    let name = path.pop().unwrap();
                    return Ok(Expr::NamespaceRef {
                        namespace_path: path,
                        name,
                    });
                }
                Ok(Expr::Var {
                    name,
                    res: VarRef::Unresolved,
                })
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
            Token::LBrace => self.parse_dynamic_array(),
            Token::Eof => Err(ParseError::UnexpectedEof("expression")),
            tok => Err(ParseError::UnexpectedToken(tok, "expression")),
        }
    }

    /// Parse a dynamic array literal `{ expr. expr. ... }`.
    ///
    /// Requires: the opening `{` has already been consumed.
    /// Ensures: each element expression is fully parsed; a trailing `.` before `}` is allowed;
    ///   returns `Expr::DynamicArray` with zero or more element expressions.
    fn parse_dynamic_array(&mut self) -> Result<Expr, ParseError> {
        self.advance(); // consume `{`
        let mut elements = Vec::new();
        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            elements.push(self.parse_expr()?);
            if self.peek() == &Token::Dot {
                self.advance(); // consume optional `.` separator
            } else {
                break;
            }
        }
        self.expect("'}' to close dynamic array", |t| t == &Token::RBrace)?;
        Ok(Expr::DynamicArray(elements))
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
            Token::Int(n) => {
                self.advance();
                Ok(Literal::Int(n))
            }
            Token::Float(f) => {
                self.advance();
                Ok(Literal::Float(f))
            }
            Token::True => {
                self.advance();
                Ok(Literal::Bool(true))
            }
            Token::False => {
                self.advance();
                Ok(Literal::Bool(false))
            }
            Token::Nil => {
                self.advance();
                Ok(Literal::Nil)
            }
            Token::Str(s) => {
                self.advance();
                Ok(Literal::Str(s))
            }
            Token::Symbol(s) => {
                self.advance();
                Ok(Literal::Symbol(s))
            }
            Token::LargeInt(n) => {
                self.advance();
                Ok(Literal::LargeInt(n))
            }
            Token::ArrayLiteralOpen => {
                self.advance();
                Ok(Literal::Array(self.parse_array_literal_elements()?))
            }
            tok => Err(ParseError::UnexpectedToken(
                tok,
                "literal element in array literal",
            )),
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
        let prev_in_block = self.in_block;
        self.in_block = true;
        let mut scope: HashSet<Arc<str>> = params.iter().cloned().collect();
        scope.extend(locals.iter().cloned());
        self.block_scope_stack.push(scope);
        let mut body = Vec::new();
        while self.peek() != &Token::RBracket && self.peek() != &Token::Eof {
            let start = self.pos;
            let node = self.parse_stmt()?;
            body.push(self.tag_from(start, node));
            self.eat(&Token::Dot);
        }
        self.block_scope_stack.pop();
        self.in_block = prev_in_block;
        self.expect("']'", |t| t == &Token::RBracket)?;
        Ok(Expr::Block {
            params,
            locals,
            body,
            // Filled by the resolve pass; a block that is never resolved
            // (top-level) captures nothing.
            captures: Vec::new(),
        })
    }
}

#[cfg(test)]
#[path = "tests/parser.rs"]
mod tests;
