use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use crate::class::{Class, IndexedSlotKind};
use crate::value::Value;

impl std::fmt::Debug for Object {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Object({})", self.class.name)
    }
}

/// Indexed storage region for variadic-slot instances.
pub enum IndexedStorage {
    Pointer(Mutex<Vec<Value>>),
    Byte(Mutex<Vec<u8>>),
    Float64(Mutex<Vec<f64>>),
}

/// A user-defined object instance.
///
/// `slots` and indexed storage are behind `parking_lot::Mutex` so that
/// `Arc<Object>: Send + Sync`, making value objects safely shareable across
/// entity threads. In practice the mutex is uncontended: value objects are
/// immutable after construction and entity objects are owned by a single
/// entity thread.
///
/// For `ValueObject` instances, `frozen` is set to `true` by `_primNewValue:`
/// after the initialization block returns. Once frozen, ivar writes via
/// `assign_var` raise `EvalError::TypeError`.
pub struct Object {
    pub class: Arc<Class>,
    /// Named instance variable storage (own + inherited), keyed by variable name.
    pub slots: Mutex<HashMap<Arc<str>, Value>>,
    /// Present iff `class.indexed_slot_kind` is `Some(_)`.
    pub indexed: Option<IndexedStorage>,
    /// Set to `true` by `freeze()` after `_primNewValue:` initialization completes.
    /// Checked by `assign_var` before any ivar write on a `ValueObject`.
    pub frozen: AtomicBool,
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
            slots: Mutex::new(slots),
            indexed: None,
            frozen: AtomicBool::new(false),
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
        let indexed = match class
            .indexed_slot_kind
            .as_ref()
            .expect("no indexed slot kind")
        {
            IndexedSlotKind::Pointer => IndexedStorage::Pointer(Mutex::new(vec![Value::Nil; size])),
            IndexedSlotKind::Byte => IndexedStorage::Byte(Mutex::new(vec![0u8; size])),
            IndexedSlotKind::Float64 => IndexedStorage::Float64(Mutex::new(vec![0f64; size])),
        };
        Self {
            class,
            slots: Mutex::new(slots),
            indexed: Some(indexed),
            frozen: AtomicBool::new(false),
        }
    }

    /// Permanently prevent ivar writes on this object.
    ///
    /// Requires: called exactly once, after `_primNewValue:` initialization block returns.
    /// Ensures: `is_frozen()` returns `true`; subsequent `assign_var` calls on the
    ///   owning `ValueObject` raise `EvalError::TypeError`.
    pub fn freeze(&self) {
        self.frozen.store(true, Ordering::Release);
    }

    /// Returns `true` if this object has been frozen by `freeze()`.
    pub fn is_frozen(&self) -> bool {
        self.frozen.load(Ordering::Acquire)
    }

    /// Returns a clone of the named instance variable, or `None` if unknown.
    pub fn get_slot(&self, name: &str) -> Option<Value> {
        self.slots.lock().get(name).cloned()
    }

    /// Write a named instance variable slot.
    ///
    /// Requires: `name` is a known instance variable of this object's class.
    /// Ensures: the slot is updated to `value`.
    pub fn set_slot(&self, name: Arc<str>, value: Value) {
        self.slots.lock().insert(name, value);
    }

    /// Returns `true` if the named slot exists (regardless of its value).
    pub fn has_slot(&self, name: &str) -> bool {
        self.slots.lock().contains_key(name)
    }
}

#[cfg(test)]
#[path = "tests/object.rs"]
mod tests;
