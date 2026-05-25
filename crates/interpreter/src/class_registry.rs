use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::class::Class;
use crate::entity::{InstanceHandle, InstanceKind, InstanceState};
use crate::namespace::Namespace;
use crate::value::Value;

/// Registry of root namespaces — those with no parent, bound by simple name in the
/// global environment. Nested namespaces (e.g. `Kernel::Collections`) are NOT stored
/// here; they live in their parent's `sub_namespaces` and are reached via `::` traversal.
pub struct ClassRegistry {
    kernel: Arc<Namespace>,
    /// Root namespaces keyed by their simple name. All entries satisfy `parent == None`.
    roots: RwLock<HashMap<Arc<str>, Arc<Namespace>>>,
}

impl ClassRegistry {
    /// Creates a new registry with `Kernel` namespace pre-populated with `Object` and `Value`.
    pub fn new() -> Self {
        let kernel = Arc::new(Namespace::new(Arc::from("Kernel"), None));

        let object_class = Arc::new(Class {
            name: Arc::from("Object"),
            superclass: None,
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
            class_object: std::sync::OnceLock::new(),
        });
        let object_state = Arc::new(InstanceState {
            class: object_class.clone(),
            kind: InstanceKind::ClassObject,
        });
        object_class
            .class_object
            .set(Arc::downgrade(&object_state))
            .ok();
        kernel.register(InstanceHandle(object_state));

        // `Value` is the built-in passive base class (is_value: true), a subclass of Object.
        let value_class = Arc::new(Class {
            name: Arc::from("Value"),
            superclass: Some(object_class),
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: true,
            class_object: std::sync::OnceLock::new(),
        });
        let value_state = Arc::new(InstanceState {
            class: value_class.clone(),
            kind: InstanceKind::ClassObject,
        });
        value_class
            .class_object
            .set(Arc::downgrade(&value_state))
            .ok();
        kernel.register(InstanceHandle(value_state));

        let mut roots = HashMap::new();
        roots.insert(Arc::from("Kernel"), kernel.clone());

        Self {
            kernel,
            roots: RwLock::new(roots),
        }
    }

    pub fn kernel(&self) -> Arc<Namespace> {
        self.kernel.clone()
    }

    /// Look up a root namespace by its simple name.
    pub fn get_namespace(&self, name: &str) -> Option<Arc<Namespace>> {
        self.roots.read().get(name).cloned()
    }

    /// Register a root namespace (one with no parent) by its simple name.
    ///
    /// Requires: `ns.parent.is_none()` — nested namespaces must be registered
    ///   via `parent.register_sub(ns)` and are not tracked here.
    pub fn register_namespace(&self, ns: Arc<Namespace>) {
        assert!(
            ns.parent.is_none(),
            "register_namespace requires a root namespace (parent == None); \
             use parent.register_sub() for nested namespaces"
        );
        self.roots.write().insert(ns.name.clone(), ns);
    }

    /// Seed `env` with Kernel's classes and every root namespace name.
    /// Sub-namespaces are intentionally excluded — they are reached via `::` only.
    pub fn populate_env(&self, env: &mut crate::env::Env) {
        for (name, handle) in self.kernel.classes.read().iter() {
            env.define(name.clone(), Value::Instance(handle.clone()));
        }
        for (name, ns) in self.roots.read().iter() {
            env.define(name.clone(), Value::Namespace(ns.clone()));
        }
    }
}

impl Default for ClassRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "tests/class_registry.rs"]
mod tests;
