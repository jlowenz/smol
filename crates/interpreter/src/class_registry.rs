use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::class::Class;
use crate::namespace::Namespace;
use crate::value::Value;

/// Registry of root namespaces — those with no parent, bound by simple name in the
/// global environment. Nested namespaces (e.g. `Kernel::Collections`) are NOT stored
/// here; they live in their parent's `sub_namespaces` and are reached via `::` traversal.
pub struct ClassRegistry {
    kernel: Arc<Namespace>,
    /// Root namespaces keyed by their simple name. All entries satisfy `parent == None`.
    roots: RefCell<HashMap<Arc<str>, Arc<Namespace>>>,
}

impl ClassRegistry {
    /// Creates a new registry with `Kernel` namespace pre-populated with `Object`.
    pub fn new() -> Self {
        let kernel = Arc::new(Namespace::new(Arc::from("Kernel"), None));
        let object_class = Arc::new(Class {
            name: Arc::from("Object"),
            superclass: None,
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RefCell::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RefCell::new(HashMap::new()),
            class_methods: RefCell::new(HashMap::new()),
        });
        kernel.register(object_class);

        let mut roots = HashMap::new();
        roots.insert(Arc::from("Kernel"), kernel.clone());

        Self {
            kernel,
            roots: RefCell::new(roots),
        }
    }

    pub fn kernel(&self) -> Arc<Namespace> {
        self.kernel.clone()
    }

    /// Look up a root namespace by its simple name.
    pub fn get_namespace(&self, name: &str) -> Option<Arc<Namespace>> {
        self.roots.borrow().get(name).cloned()
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
        self.roots.borrow_mut().insert(ns.name.clone(), ns);
    }

    /// Seed `env` with Kernel's classes and every root namespace name.
    /// Sub-namespaces are intentionally excluded — they are reached via `::` only.
    pub fn populate_env(&self, env: &mut crate::env::Env) {
        for (name, class) in self.kernel.classes.borrow().iter() {
            env.define(name.clone(), Value::Class(class.clone()));
        }
        for (name, ns) in self.roots.borrow().iter() {
            env.define(name.clone(), Value::Namespace(ns.clone()));
        }
    }
}

impl Default for ClassRegistry {
    fn default() -> Self {
        Self::new()
    }
}
