use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::class::Class;
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
        });
        kernel.register(object_class.clone());

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
        });
        kernel.register(value_class);

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
        for (name, class) in self.kernel.classes.read().iter() {
            env.define(name.clone(), Value::Class(class.clone()));
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
mod tests {
    use super::*;
    use crate::env::Env;

    #[test]
    fn default_creates_valid_registry() {
        let reg = ClassRegistry::default();
        assert!(reg.get_namespace("Kernel").is_some());
    }

    #[test]
    fn get_namespace_returns_kernel() {
        let reg = ClassRegistry::new();
        let ns = reg.get_namespace("Kernel").unwrap();
        assert_eq!(ns.name.as_ref(), "Kernel");
    }

    #[test]
    fn get_namespace_returns_none_for_unknown() {
        let reg = ClassRegistry::new();
        assert!(reg.get_namespace("NoSuchNamespace").is_none());
    }

    #[test]
    fn register_namespace_and_retrieve() {
        let reg = ClassRegistry::new();
        let ns = Arc::new(Namespace::new(Arc::from("MyApp"), None));
        reg.register_namespace(ns.clone());
        let retrieved = reg.get_namespace("MyApp").unwrap();
        assert!(Arc::ptr_eq(&retrieved, &ns));
    }

    #[test]
    #[should_panic(expected = "register_namespace requires a root namespace")]
    fn register_nested_namespace_panics() {
        let reg = ClassRegistry::new();
        let parent = Arc::new(Namespace::new(Arc::from("Parent"), None));
        let child = Arc::new(Namespace::new(Arc::from("Child"), Some(parent)));
        reg.register_namespace(child);
    }

    #[test]
    fn populate_env_includes_object_and_kernel() {
        let reg = ClassRegistry::new();
        let mut env = Env::new();
        reg.populate_env(&mut env);

        assert!(env.get(&Arc::from("Kernel")).is_some());
        assert!(env.get(&Arc::from("Object")).is_some());
        assert!(env.get(&Arc::from("Value")).is_some());
    }

    #[test]
    fn value_class_is_value_true() {
        let reg = ClassRegistry::new();
        let kernel = reg.kernel();
        let value_class = kernel.lookup_class("Value").unwrap();
        assert!(value_class.is_value);
    }

    #[test]
    fn object_class_is_value_false() {
        let reg = ClassRegistry::new();
        let kernel = reg.kernel();
        let object_class = kernel.lookup_class("Object").unwrap();
        assert!(!object_class.is_value);
    }
}
