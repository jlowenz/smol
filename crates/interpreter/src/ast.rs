use std::sync::Arc;

pub use crate::class::{IndexedSlotKind, MethodPattern};

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    Str(Arc<str>),
    Symbol(Arc<str>),
    LargeInt(Arc<num_bigint::BigInt>),
}

/// A single message send. Three levels of precedence match Smalltalk's grammar.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Unary(Arc<str>),
    Binary {
        op: Arc<str>,
        arg: Box<Expr>,
    },
    /// Keyword message. `parts` is non-empty; the selector is formed by
    /// concatenating all keyword fragments: `[(at:, e1), (put:, e2)]` → `at:put:`.
    Keyword {
        parts: Vec<(Arc<str>, Expr)>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Literal),
    Var(Arc<str>),
    /// A single message send.
    Send {
        receiver: Box<Expr>,
        message: Message,
    },
    Block {
        params: Vec<Arc<str>>,
        /// Local variable declarations (`| x y |`) inside the block.
        locals: Vec<Arc<str>>,
        body: Vec<Stmt>,
    },
    /// Cascade: `receiver msg1; msg2; msg3`.
    /// All messages are sent to the same receiver object; evaluates to the
    /// value of the last message send.
    Cascade {
        receiver: Box<Expr>,
        messages: Vec<Message>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Expr(Expr),
    Assign {
        name: Arc<str>,
        value: Expr,
    },
    /// `^expr` — non-local return from the enclosing method frame.
    Return(Expr),
    /// `| x y z |` — declares temporaries, bound to nil in the current frame.
    LocalDecl(Vec<Arc<str>>),
    /// `ClassName >> pattern [ locals? body ]`
    MethodDef(MethodDef),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodDef {
    pub class_name: Arc<str>,
    pub pattern: MethodPattern,
    pub locals: Vec<Arc<str>>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
    /// Comments collected during parsing, in source order.
    /// Full trivia attachment to individual nodes is deferred.
    pub comments: Vec<Arc<str>>,
}
