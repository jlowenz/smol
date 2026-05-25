use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::class::Class;
use crate::env::Env;
use parking_lot::RwLock;

fn make_class(name: &str) -> Arc<Class> {
    Arc::new(Class {
        name: Arc::from(name),
        superclass: None,
        own_instance_vars: vec![],
        own_class_vars: vec![],
        class_var_storage: RwLock::new(HashMap::new()),
        indexed_slot_kind: None,
        methods: RwLock::new(HashMap::new()),
        class_methods: RwLock::new(HashMap::new()),
        is_value: false,
    })
}

#[test]
fn float_eq_bit_pattern() {
    let nan = Value::Float(f64::NAN);
    assert_eq!(nan, nan);
    assert_eq!(Value::Float(1.5), Value::Float(1.5));
    assert_ne!(Value::Float(1.5), Value::Float(2.0));
    assert_ne!(Value::Float(0.0_f64), Value::Float(-0.0_f64));
}

#[test]
fn value_kind_names() {
    assert_eq!(Value::Int(0).kind(), "Int");
    assert_eq!(Value::Nil.kind(), "Nil");
    assert_eq!(
        Value::LargeInt(Arc::new(BigInt::from(0))).kind(),
        "LargeInt"
    );
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
        home_entity: None,
    }));
    assert_eq!(Value::Float(1.0).kind(), "Float");
    assert_eq!(Value::Bool(true).kind(), "Bool");
    assert_eq!(Value::Symbol(Arc::from("s")).kind(), "Symbol");
    assert_eq!(Value::Str(Arc::from("s")).kind(), "String");
    assert_eq!(block.kind(), "Block");
    assert_eq!(
        Value::ValueObject(Arc::new(Object::new(class.clone()))).kind(),
        "Object"
    );
    assert_eq!(Value::Class(class).kind(), "Class");
    assert_eq!(
        Value::Namespace(Arc::new(Namespace::new(Arc::from("N"), None))).kind(),
        "Namespace"
    );
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
    assert_eq!(
        format!("{}", Value::LargeInt(Arc::new(BigInt::from(99)))),
        "99"
    );
}

#[test]
fn display_composite_variants() {
    let class = make_class("Dog");
    assert_eq!(format!("{}", Value::Class(class.clone())), "Dog");
    assert_eq!(
        format!("{}", Value::ValueObject(Arc::new(Object::new(class)))),
        "a Dog"
    );
    assert_eq!(
        format!(
            "{}",
            Value::Namespace(Arc::new(Namespace::new(Arc::from("Lib"), None)))
        ),
        "Namespace(Lib)"
    );
    let block = Value::Block(Arc::new(Block {
        params: vec![],
        locals: vec![],
        body: vec![],
        env: Env::new(),
        home_depth: 0,
        home_entity: None,
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
    let b = Arc::new(Block {
        params: vec![],
        locals: vec![],
        body: vec![],
        env: Env::new(),
        home_depth: 0,
        home_entity: None,
    });
    assert_eq!(Value::Block(b.clone()), Value::Block(b.clone()));
    let b2 = Arc::new(Block {
        params: vec![],
        locals: vec![],
        body: vec![],
        env: Env::new(),
        home_depth: 0,
        home_entity: None,
    });
    assert_ne!(Value::Block(b), Value::Block(b2));
}

#[test]
fn eq_value_object_class_namespace_use_pointer_identity() {
    let class = make_class("Z");
    let class2 = make_class("Z");
    assert_eq!(Value::Class(class.clone()), Value::Class(class.clone()));
    assert_ne!(Value::Class(class.clone()), Value::Class(class2));

    let obj = Arc::new(Object::new(class.clone()));
    assert_eq!(
        Value::ValueObject(obj.clone()),
        Value::ValueObject(obj.clone())
    );
    let obj2 = Arc::new(Object::new(class));
    assert_ne!(Value::ValueObject(obj), Value::ValueObject(obj2));

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
    map.insert(Value::ValueObject(Arc::new(Object::new(class))), 9);
    map.insert(
        Value::Namespace(Arc::new(Namespace::new(Arc::from("N"), None))),
        10,
    );
    let b = Arc::new(Block {
        params: vec![],
        locals: vec![],
        body: vec![],
        env: Env::new(),
        home_depth: 0,
        home_entity: None,
    });
    map.insert(Value::Block(b), 11);
    assert_eq!(map.len(), 11);
}
