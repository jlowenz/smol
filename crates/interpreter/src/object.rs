use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::class::{Class, IndexedSlotKind};
use crate::value::Value;

impl std::fmt::Debug for Object {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Object({})", self.class.name)
    }
}

/// Indexed storage region for variadic-slot instances.
pub enum IndexedStorage {
    Pointer(RefCell<Vec<Value>>),
    Byte(RefCell<Vec<u8>>),
    Float64(RefCell<Vec<f64>>),
}

/// A user-defined object instance.
pub struct Object {
    pub class: Arc<Class>,
    /// Named instance variable storage (own + inherited), keyed by variable name.
    pub slots: RefCell<HashMap<Arc<str>, Value>>,
    /// Present iff `class.indexed_slot_kind` is `Some(_)`.
    pub indexed: Option<IndexedStorage>,
}

impl Object {
    /// Allocate an object with all instance variable slots initialised to `nil`.
    ///
    /// Requires: `class` has been fully initialised (all superclasses registered).
    /// Ensures: `slots` keys match `class.all_instance_vars()`; no indexed region.
    pub fn new(class: Arc<Class>) -> Self {
        let slots = class
            .all_instance_vars()
            .into_iter()
            .map(|name| (name, Value::Nil))
            .collect();
        Self {
            class,
            slots: RefCell::new(slots),
            indexed: None,
        }
    }

    /// Allocate an object with indexed storage of `size` elements.
    ///
    /// Requires: `class.indexed_slot_kind` is `Some(_)`.
    /// Ensures: named slots initialised to `nil`; indexed region allocated with
    ///          `size` zero-valued elements.
    pub fn new_with_indexed(class: Arc<Class>, size: usize) -> Self {
        let slots = class
            .all_instance_vars()
            .into_iter()
            .map(|name| (name, Value::Nil))
            .collect();
        let indexed = match class.indexed_slot_kind.as_ref().expect("no indexed slot kind") {
            IndexedSlotKind::Pointer => {
                IndexedStorage::Pointer(RefCell::new(vec![Value::Nil; size]))
            }
            IndexedSlotKind::Byte => IndexedStorage::Byte(RefCell::new(vec![0u8; size])),
            IndexedSlotKind::Float64 => {
                IndexedStorage::Float64(RefCell::new(vec![0f64; size]))
            }
        };
        Self {
            class,
            slots: RefCell::new(slots),
            indexed: Some(indexed),
        }
    }

    /// Returns a clone of the named instance variable, or `None` if unknown.
    pub fn get_slot(&self, name: &str) -> Option<Value> {
        self.slots.borrow().get(name).cloned()
    }

    /// Write a named instance variable slot.
    ///
    /// Requires: `name` is a known instance variable of this object's class.
    /// Ensures: the slot is updated to `value`.
    pub fn set_slot(&self, name: Arc<str>, value: Value) {
        self.slots.borrow_mut().insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::class::{Class, IndexedSlotKind};
    use crate::value::Value;

    fn make_class(name: &str, ivars: Vec<&str>, indexed: Option<IndexedSlotKind>) -> Arc<Class> {
        Arc::new(Class {
            name: Arc::from(name),
            superclass: None,
            own_instance_vars: ivars.into_iter().map(Arc::from).collect(),
            own_class_vars: vec![],
            class_var_storage: std::cell::RefCell::new(HashMap::new()),
            indexed_slot_kind: indexed,
            methods: std::cell::RefCell::new(HashMap::new()),
            class_methods: std::cell::RefCell::new(HashMap::new()),
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
            IndexedStorage::Pointer(slots) => assert_eq!(slots.borrow().len(), 3),
            _ => panic!("expected Pointer storage"),
        }
    }

    #[test]
    fn new_with_indexed_byte() {
        let class = make_class("ByteArr", vec![], Some(IndexedSlotKind::Byte));
        let obj = Object::new_with_indexed(class, 4);
        match obj.indexed.as_ref().unwrap() {
            IndexedStorage::Byte(slots) => assert_eq!(slots.borrow().len(), 4),
            _ => panic!("expected Byte storage"),
        }
    }

    #[test]
    fn new_with_indexed_float64() {
        let class = make_class("FloatArr", vec![], Some(IndexedSlotKind::Float64));
        let obj = Object::new_with_indexed(class, 2);
        match obj.indexed.as_ref().unwrap() {
            IndexedStorage::Float64(slots) => assert_eq!(slots.borrow().len(), 2),
            _ => panic!("expected Float64 storage"),
        }
    }
}
