use std::sync::Arc;

use num_bigint::BigInt;

use crate::ast::Stmt;
use crate::class::Class;
use crate::env::Env;
use crate::namespace::Namespace;
use crate::object::Object;

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
    /// A user-defined object instance.
    Object(Arc<Object>),
    /// A class object. Class names in the environment are bound to this variant.
    Class(Arc<Class>),
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
    /// `Arc<RefCell<Frame>>`, this is a live shared reference, not a snapshot.
    pub env: Env,
    /// Method-stack depth of the frame a `^`-return inside this block should
    /// target. Equal to the method depth at the block's creation site, so that
    /// non-local returns propagate past intermediate method calls and are caught
    /// by the correct `invoke_method` frame.
    pub home_depth: usize,
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
            (Value::Object(a), Value::Object(b)) => Arc::ptr_eq(a, b),
            (Value::Class(a), Value::Class(b)) => Arc::ptr_eq(a, b),
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
            Value::Object(o) => Arc::as_ptr(o).hash(state),
            Value::Class(c) => Arc::as_ptr(c).hash(state),
            Value::Namespace(ns) => Arc::as_ptr(ns).hash(state),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
            Value::Object(o) => write!(f, "a {}", o.class.name),
            Value::Class(c) => write!(f, "{}", c.name),
            Value::Namespace(ns) => write!(f, "Namespace({})", ns.name),
        }
    }
}

impl Value {
    /// Bootstrap kind tag used in error messages. Not the same as the class name.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::Nil => "Nil",
            Value::Symbol(_) => "Symbol",
            Value::Str(_) => "String",
            Value::Block(_) => "Block",
            Value::LargeInt(_) => "LargeInt",
            Value::Object(_) => "Object",
            Value::Class(_) => "Class",
            Value::Namespace(_) => "Namespace",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::class::Class;
    use crate::env::Env;

    fn make_class(name: &str) -> Arc<Class> {
        Arc::new(Class {
            name: Arc::from(name),
            superclass: None,
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: std::cell::RefCell::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: std::cell::RefCell::new(HashMap::new()),
            class_methods: std::cell::RefCell::new(HashMap::new()),
        })
    }

    #[test]
    fn float_eq_bit_pattern() {
        // Bit-pattern comparison: same NaN bits are equal (unlike IEEE 754).
        // This is intentional — it gives reflexivity so NaN works as a HashMap key.
        let nan = Value::Float(f64::NAN);
        assert_eq!(nan, nan);
        assert_eq!(Value::Float(1.5), Value::Float(1.5));
        assert_ne!(Value::Float(1.5), Value::Float(2.0));
        // +0.0 and -0.0 have different bit patterns → unequal under this scheme.
        assert_ne!(Value::Float(0.0_f64), Value::Float(-0.0_f64));
    }

    #[test]
    fn value_kind_names() {
        assert_eq!(Value::Int(0).kind(), "Int");
        assert_eq!(Value::Nil.kind(), "Nil");
        assert_eq!(Value::LargeInt(Arc::new(BigInt::from(0))).kind(), "LargeInt");
    }

    #[test]
    fn largeint_eq_int() {
        assert_eq!(Value::Int(42), Value::LargeInt(Arc::new(BigInt::from(42))));
        assert_eq!(Value::LargeInt(Arc::new(BigInt::from(42))), Value::Int(42));
    }

    #[test]
    fn kind_remaining_variants() {
        let class = make_class("X");
        let block = Value::Block(Arc::new(Block {
            params: vec![],
            locals: vec![],
            body: vec![],
            env: Env::new(),
            home_depth: 0,
        }));
        assert_eq!(Value::Float(1.0).kind(), "Float");
        assert_eq!(Value::Bool(true).kind(), "Bool");
        assert_eq!(Value::Symbol(Arc::from("s")).kind(), "Symbol");
        assert_eq!(Value::Str(Arc::from("s")).kind(), "String");
        assert_eq!(block.kind(), "Block");
        assert_eq!(Value::Object(Arc::new(Object::new(class.clone()))).kind(), "Object");
        assert_eq!(Value::Class(class).kind(), "Class");
        assert_eq!(Value::Namespace(Arc::new(Namespace::new(Arc::from("N"), None))).kind(), "Namespace");
    }

    #[test]
    fn display_scalars() {
        assert_eq!(format!("{}", Value::Int(7)), "7");
        assert_eq!(format!("{}", Value::Float(2.5)), "2.5");
        assert_eq!(format!("{}", Value::Bool(true)), "true");
        assert_eq!(format!("{}", Value::Bool(false)), "false");
        assert_eq!(format!("{}", Value::Nil), "nil");
        assert_eq!(format!("{}", Value::Symbol(Arc::from("ok"))), "#ok");
        assert_eq!(format!("{}", Value::Str(Arc::from("hi"))), "'hi'");
        assert_eq!(format!("{}", Value::LargeInt(Arc::new(BigInt::from(99)))), "99");
    }

    #[test]
    fn display_composite_variants() {
        let class = make_class("Dog");
        assert_eq!(format!("{}", Value::Class(class.clone())), "Dog");
        assert_eq!(format!("{}", Value::Object(Arc::new(Object::new(class)))), "a Dog");
        assert_eq!(
            format!("{}", Value::Namespace(Arc::new(Namespace::new(Arc::from("Lib"), None)))),
            "Namespace(Lib)"
        );
        let block = Value::Block(Arc::new(Block {
            params: vec![],
            locals: vec![],
            body: vec![],
            env: Env::new(),
            home_depth: 0,
        }));
        assert_eq!(format!("{block}"), "a Block");
    }

    #[test]
    fn eq_primitive_variants() {
        assert_eq!(Value::Bool(true), Value::Bool(true));
        assert_ne!(Value::Bool(true), Value::Bool(false));
        assert_eq!(Value::Nil, Value::Nil);
        assert_ne!(Value::Nil, Value::Int(0));
        assert_eq!(Value::Symbol(Arc::from("x")), Value::Symbol(Arc::from("x")));
        assert_ne!(Value::Symbol(Arc::from("x")), Value::Symbol(Arc::from("y")));
        assert_eq!(Value::Str(Arc::from("ab")), Value::Str(Arc::from("ab")));
        assert_ne!(Value::Str(Arc::from("ab")), Value::Str(Arc::from("cd")));
        assert_ne!(Value::Int(1), Value::Bool(true));
    }

    #[test]
    fn eq_block_uses_pointer_identity() {
        let b = Arc::new(Block { params: vec![], locals: vec![], body: vec![], env: Env::new(), home_depth: 0 });
        assert_eq!(Value::Block(b.clone()), Value::Block(b.clone()));
        let b2 = Arc::new(Block { params: vec![], locals: vec![], body: vec![], env: Env::new(), home_depth: 0 });
        assert_ne!(Value::Block(b), Value::Block(b2));
    }

    #[test]
    fn eq_object_class_namespace_use_pointer_identity() {
        let class = make_class("Z");
        let class2 = make_class("Z");
        assert_eq!(Value::Class(class.clone()), Value::Class(class.clone()));
        assert_ne!(Value::Class(class.clone()), Value::Class(class2));

        let obj = Arc::new(Object::new(class.clone()));
        assert_eq!(Value::Object(obj.clone()), Value::Object(obj.clone()));
        let obj2 = Arc::new(Object::new(class));
        assert_ne!(Value::Object(obj), Value::Object(obj2));

        let ns = Arc::new(Namespace::new(Arc::from("A"), None));
        assert_eq!(Value::Namespace(ns.clone()), Value::Namespace(ns.clone()));
        let ns2 = Arc::new(Namespace::new(Arc::from("A"), None));
        assert_ne!(Value::Namespace(ns), Value::Namespace(ns2));
    }

    #[test]
    #[allow(clippy::mutable_key_type)]
    fn hash_does_not_panic() {
        let mut map: HashMap<Value, i32> = HashMap::new();
        map.insert(Value::Int(1), 1);
        map.insert(Value::Float(1.5), 2);
        map.insert(Value::Bool(false), 3);
        map.insert(Value::Nil, 4);
        map.insert(Value::Symbol(Arc::from("s")), 5);
        map.insert(Value::Str(Arc::from("t")), 6);
        map.insert(Value::LargeInt(Arc::new(BigInt::from(999))), 7);
        let class = make_class("H");
        map.insert(Value::Class(class.clone()), 8);
        map.insert(Value::Object(Arc::new(Object::new(class))), 9);
        map.insert(Value::Namespace(Arc::new(Namespace::new(Arc::from("N"), None))), 10);
        let b = Arc::new(Block { params: vec![], locals: vec![], body: vec![], env: Env::new(), home_depth: 0 });
        map.insert(Value::Block(b), 11);
        assert_eq!(map.len(), 11);
    }
}
