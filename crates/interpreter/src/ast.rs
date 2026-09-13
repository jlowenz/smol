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
    /// Nested `#(...)` array literal. Evaluated by `eval_expr` (not `eval_literal`)
    /// because constructing an Array object requires the evaluator and environment.
    Array(Vec<Literal>),
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

impl Message {
    /// The selector string of this send: the unary/binary name, or the keyword
    /// parts concatenated (`[(at:, _), (put:, _)]` → `at:put:`). Mirrors
    /// [`crate::class::MethodPattern::selector`] on the definition side.
    pub fn selector(&self) -> Arc<str> {
        match self {
            Message::Unary(s) => s.clone(),
            Message::Binary { op, .. } => op.clone(),
            Message::Keyword { parts } => parts
                .iter()
                .map(|(k, _)| k.as_ref())
                .collect::<String>()
                .into(),
        }
    }
}

/// How a variable reference resolves. Filled by the resolve pass
/// (`docs/perf_env_frame_redesign.md` §5); the parser emits `Unresolved`.
#[derive(Debug, Clone, PartialEq)]
pub enum VarRef {
    /// Not yet resolved — the parser's output, before the resolve pass runs.
    Unresolved,
    /// Slot `i` in the current scope's frame (a parameter or local).
    Local(u16),
    /// Index `i` into the current block's capture snapshot (an enclosing binding).
    Upvalue(u16),
    /// The receiver, `self`.
    SelfRef,
    /// An instance variable, class variable, or global — resolved at runtime.
    Dynamic(Arc<str>),
}

/// Where a block capture slot is filled from at block-creation time. Parallel to a
/// block's `Upvalue` indices: `captures[i]` says where snapshot slot `i` comes from
/// in the *immediately-enclosing* (creating) scope. Filled by the resolve pass.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureSource {
    /// Slot `i` in the enclosing scope's frame (`Frame::slots`).
    ParentLocal(u16),
    /// Index `i` in the enclosing scope's own capture snapshot (`Frame::snapshot`).
    ParentUpvalue(u16),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Literal),
    Var {
        name: Arc<str>,
        res: VarRef,
    },
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
        /// Capture plan, parallel to the block's `Upvalue(i)` indices: `captures[i]`
        /// says where snapshot slot `i` is filled from in the enclosing scope at
        /// block-creation time. Empty from the parser; filled by the resolve pass
        /// (`Vec::new()` for an unresolved/top-level block, which captures nothing).
        captures: Vec<CaptureSource>,
    },
    /// Cascade: `receiver msg1; msg2; msg3`.
    /// All messages are sent to the same receiver object; evaluates to the
    /// value of the last message send.
    Cascade {
        receiver: Box<Expr>,
        messages: Vec<Message>,
    },
    /// Receiver for super sends. Only valid as the receiver of a message send.
    Super,
    /// `A::B::C` — namespace_path is the chain of namespace names, name is the final class.
    /// Resolved at eval time.
    NamespaceRef {
        namespace_path: Vec<Arc<str>>,
        name: Arc<str>,
    },
    /// `#(elem0 elem1 ...)` — array literal with recursively-evaluated elements.
    ArrayLiteral(Vec<Literal>),
    /// `{ expr. expr. ... }` — dynamic array: elements evaluated at runtime.
    DynamicArray(Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Expr(Expr),
    Assign {
        name: Arc<str>,
        /// How the assignment target resolves. Filled by the resolve pass (P2.1b);
        /// the parser emits `Unresolved`. The strict block rule (P1) guarantees a
        /// target is a scope-local (`Local`) or a dynamic name (`Dynamic`), never a
        /// capture — a block may not assign a non-local.
        res: VarRef,
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
    /// True for `Foo class >> selector` definitions.
    pub is_class_side: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
    /// Comments collected during parsing, in source order.
    /// Full trivia attachment to individual nodes is deferred.
    pub comments: Vec<Arc<str>>,
}
