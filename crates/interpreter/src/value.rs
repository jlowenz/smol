use std::sync::Arc;

use num_bigint::BigInt;

use crate::ast::Stmt;
use crate::entity::{EntityId, FutureHandle, InstanceHandle};
use crate::env::Env;
use crate::namespace::Namespace;

/// Runtime representation of every object in the language.
///
/// `Int`, `Float`, and `Bool` are immediate variants for bootstrap convenience;
/// conceptually they are objects that respond to messages. NaN-boxing and
/// tagged-pointer schemes are deferred — they are performance optimisations
/// that would obscure bootstrap semantics and require `unsafe`.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    /// Equality uses bit-pattern comparison so `Value` can be used as a
    /// `HashMap` key in environments. See the `Eq` impl below.
    Float(f64),
    Bool(bool),
    Nil,
    /// Identity by content; no global interner yet.
    Symbol(Arc<str>),
    Str(Arc<str>),
    Block(Arc<Block>),
    /// Arbitrary-precision integer. Used when Int arithmetic overflows i64.
    LargeInt(Arc<BigInt>),
    /// Any object instance: actor entity, passive value object, or class object.
    /// Discriminated by `InstanceKind` inside the handle.
    Instance(InstanceHandle),
    /// A lazy future reference. Resolved only when a message is dispatched to it.
    Future(FutureHandle),
    /// A namespace object. Namespace names in the environment are bound to this variant.
    Namespace(Arc<Namespace>),
}

/// A block (closure). Captures its defining environment via shared frames.
#[derive(Debug, Clone)]
pub struct Block {
    pub params: Vec<Arc<str>>,
    pub locals: Vec<Arc<str>>,
    pub body: Vec<Stmt>,
    /// Captured lexical environment. Because `Env` frames are
    /// `Arc<Mutex<Frame>>`, this is a live shared reference, not a snapshot.
    pub env: Env,
    /// Method-stack depth of the frame a `^`-return inside this block should
    /// target. Equal to the method depth at the block's creation site, so that
    /// non-local returns propagate past intermediate method calls and are caught
    /// by the correct `invoke_method` frame.
    pub home_depth: usize,
    /// The `EntityId` of the entity that created this block, or `None` when
    /// created outside any entity context. Used to detect cross-entity block
    /// invocations for write-guard enforcement.
    pub home_entity: Option<EntityId>,
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Nil, Value::Nil) => true,
            (Value::Symbol(a), Value::Symbol(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Block(a), Value::Block(b)) => Arc::ptr_eq(a, b),
            (Value::LargeInt(a), Value::LargeInt(b)) => a == b,
            (Value::Int(a), Value::LargeInt(b)) => BigInt::from(*a) == **b,
            (Value::LargeInt(a), Value::Int(b)) => **a == BigInt::from(*b),
            (Value::Instance(a), Value::Instance(b)) => Arc::ptr_eq(&a.0, &b.0),
            (Value::Future(a), Value::Future(b)) => Arc::ptr_eq(&a.0, &b.0),
            (Value::Namespace(a), Value::Namespace(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for Value {}

impl std::hash::Hash for Value {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Int(n) => n.hash(state),
            Value::Float(f) => f.to_bits().hash(state),
            Value::Bool(b) => b.hash(state),
            Value::Nil => {}
            Value::Symbol(s) | Value::Str(s) => s.hash(state),
            Value::Block(b) => Arc::as_ptr(b).hash(state),
            Value::LargeInt(b) => b.hash(state),
            Value::Instance(h) => Arc::as_ptr(&h.0).hash(state),
            Value::Future(f) => Arc::as_ptr(&f.0).hash(state),
            Value::Namespace(ns) => Arc::as_ptr(ns).hash(state),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::entity::InstanceKind;
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n:?}"),
            Value::Bool(true) => write!(f, "true"),
            Value::Bool(false) => write!(f, "false"),
            Value::Nil => write!(f, "nil"),
            Value::Symbol(s) => write!(f, "#{s}"),
            Value::Str(s) => write!(f, "'{s}'"),
            Value::Block(_) => write!(f, "a Block"),
            Value::LargeInt(n) => write!(f, "{n}"),
            Value::Instance(h) => match &h.0.kind {
                InstanceKind::ClassEntity(_) => write!(f, "{}", h.0.class.name),
                _ => write!(f, "a {}", h.0.class.name),
            },
            Value::Future(_) => write!(f, "a Future"),
            Value::Namespace(ns) => write!(f, "Namespace({})", ns.name),
        }
    }
}

impl Value {
    /// Bootstrap kind tag used in error messages. Not the same as the class name.
    pub fn kind(&self) -> &'static str {
        use crate::entity::InstanceKind;
        match self {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::Nil => "Nil",
            Value::Symbol(_) => "Symbol",
            Value::Str(_) => "String",
            Value::Block(_) => "Block",
            Value::LargeInt(_) => "LargeInt",
            Value::Instance(h) => match &h.0.kind {
                InstanceKind::ClassEntity(_) => "Class",
                _ => "Object",
            },
            Value::Future(_) => "Future",
            Value::Namespace(_) => "Namespace",
        }
    }
}

#[cfg(test)]
#[path = "tests/value.rs"]
mod tests;
