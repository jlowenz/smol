use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::class::{Class, IndexedSlotKind};
use crate::value::Value;
use parking_lot::RwLock;

fn make_class(name: &str, ivars: Vec<&str>, indexed: Option<IndexedSlotKind>) -> Arc<Class> {
    Arc::new(Class {
        name: Arc::from(name),
        superclass: None,
        own_instance_vars: ivars.into_iter().map(Arc::from).collect(),
        own_class_vars: vec![],
        class_var_storage: RwLock::new(HashMap::new()),
        indexed_slot_kind: indexed,
        methods: RwLock::new(HashMap::new()),
        class_methods: RwLock::new(HashMap::new()),
        is_value: false,
        class_object: std::sync::OnceLock::new(),
        namespace: std::sync::OnceLock::new(),
    })
}

#[test]
fn new_initialises_slots_to_nil() {
    let class = make_class("Point", vec!["x", "y"], None);
    let obj = Object::new(class);
    assert_eq!(obj.get_slot("x"), Some(Value::Nil));
    assert_eq!(obj.get_slot("y"), Some(Value::Nil));
    assert_eq!(obj.get_slot("z"), None);
}

#[test]
fn set_slot_updates_value() {
    let class = make_class("Box", vec!["val"], None);
    let obj = Object::new(class);
    obj.set_slot(Arc::from("val"), Value::Int(42));
    assert_eq!(obj.get_slot("val"), Some(Value::Int(42)));
}

#[test]
fn debug_includes_class_name() {
    let class = make_class("Rocket", vec![], None);
    let obj = Object::new(class);
    assert!(format!("{obj:?}").contains("Rocket"));
}

#[test]
fn new_with_indexed_pointer() {
    let class = make_class("Arr", vec![], Some(IndexedSlotKind::Pointer));
    let obj = Object::new_with_indexed(class, 3);
    match obj.indexed.as_ref().unwrap() {
        IndexedStorage::Pointer(slots) => assert_eq!(slots.lock().len(), 3),
        _ => panic!("expected Pointer storage"),
    }
}

#[test]
fn new_with_indexed_byte() {
    let class = make_class("ByteArr", vec![], Some(IndexedSlotKind::Byte));
    let obj = Object::new_with_indexed(class, 4);
    match obj.indexed.as_ref().unwrap() {
        IndexedStorage::Byte(slots) => assert_eq!(slots.lock().len(), 4),
        _ => panic!("expected Byte storage"),
    }
}

#[test]
fn new_with_indexed_float64() {
    let class = make_class("FloatArr", vec![], Some(IndexedSlotKind::Float64));
    let obj = Object::new_with_indexed(class, 2);
    match obj.indexed.as_ref().unwrap() {
        IndexedStorage::Float64(slots) => assert_eq!(slots.lock().len(), 2),
        _ => panic!("expected Float64 storage"),
    }
}

#[test]
fn has_slot_returns_true_for_known_ivar() {
    let class = make_class("Box", vec!["val"], None);
    let obj = Object::new(class);
    assert!(obj.has_slot("val"));
    assert!(!obj.has_slot("missing"));
}
