use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::class::Class;
use crate::entity::{InstanceHandle, InstanceKind, InstanceState, new_entity_data};
use crate::env::Env;
use crate::object::Object;
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
        class_object: std::sync::OnceLock::new(),
    })
}

fn class_handle(name: &str) -> InstanceHandle {
    let cls = make_class(name);
    let data = new_entity_data(&cls, triple_store::store::Store::in_memory());
    InstanceHandle(Arc::new(InstanceState {
        class: cls,
        kind: InstanceKind::ClassEntity(Box::new(data)),
    }))
}

fn value_object_handle(name: &str) -> InstanceHandle {
    let cls = make_class(name);
    let obj = Arc::new(Object::new(cls.clone()));
    InstanceHandle(Arc::new(InstanceState {
        class: cls,
        kind: InstanceKind::ValueObject(obj),
    }))
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
    let block = Value::Block(Arc::new(Block {
        params: vec![],
        locals: vec![],
        body: vec![],
        snapshot: Arc::from(Vec::new()),
        env: Env::new(),
        home_depth: 0,
        home_entity: None,
        home_self: None,
    }));
    assert_eq!(Value::Float(1.0).kind(), "Float");
    assert_eq!(Value::Bool(true).kind(), "Bool");
    assert_eq!(Value::Symbol(Arc::from("s")).kind(), "Symbol");
    assert_eq!(Value::Str(Arc::from("s")).kind(), "String");
    assert_eq!(block.kind(), "Block");
    assert_eq!(Value::Instance(value_object_handle("X")).kind(), "Object");
    assert_eq!(Value::Instance(class_handle("X")).kind(), "Class");
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
    assert_eq!(format!("{}", Value::Instance(class_handle("Dog"))), "Dog");
    assert_eq!(
        format!("{}", Value::Instance(value_object_handle("Dog"))),
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
        snapshot: Arc::from(Vec::new()),
        env: Env::new(),
        home_depth: 0,
        home_entity: None,
        home_self: None,
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
        snapshot: Arc::from(Vec::new()),
        env: Env::new(),
        home_depth: 0,
        home_entity: None,
        home_self: None,
    });
    assert_eq!(Value::Block(b.clone()), Value::Block(b.clone()));
    let b2 = Arc::new(Block {
        params: vec![],
        locals: vec![],
        body: vec![],
        snapshot: Arc::from(Vec::new()),
        env: Env::new(),
        home_depth: 0,
        home_entity: None,
        home_self: None,
    });
    assert_ne!(Value::Block(b), Value::Block(b2));
}

#[test]
fn eq_instance_uses_pointer_identity() {
    let ch = class_handle("Z");
    assert_eq!(Value::Instance(ch.clone()), Value::Instance(ch.clone()));
    let ch2 = class_handle("Z");
    assert_ne!(Value::Instance(ch), Value::Instance(ch2));

    let vo = value_object_handle("Z");
    assert_eq!(Value::Instance(vo.clone()), Value::Instance(vo.clone()));
    let vo2 = value_object_handle("Z");
    assert_ne!(Value::Instance(vo), Value::Instance(vo2));

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
    map.insert(Value::Instance(class_handle("H")), 8);
    map.insert(Value::Instance(value_object_handle("H")), 9);
    map.insert(
        Value::Namespace(Arc::new(Namespace::new(Arc::from("N"), None))),
        10,
    );
    let b = Arc::new(Block {
        params: vec![],
        locals: vec![],
        body: vec![],
        snapshot: Arc::from(Vec::new()),
        env: Env::new(),
        home_depth: 0,
        home_entity: None,
        home_self: None,
    });
    map.insert(Value::Block(b), 11);
    assert_eq!(map.len(), 11);
}
