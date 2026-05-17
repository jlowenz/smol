use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::class::Class;
use crate::value::Value;

/// Global table of named classes.  Pre-populated with the root `Object` class.
pub struct ClassRegistry {
    classes: RefCell<HashMap<Arc<str>, Arc<Class>>>,
}

impl ClassRegistry {
    /// Creates a new registry pre-populated with the root `Object` class.
    pub fn new() -> Self {
        let registry = Self { classes: RefCell::new(HashMap::new()) };
        registry.register(Arc::new(Class {
            name: Arc::from("Object"),
            superclass: None,
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RefCell::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RefCell::new(HashMap::new()),
        }));
        registry
    }

    /// Register a class, replacing any previous registration under the same name.
    pub fn register(&self, class: Arc<Class>) {
        self.classes.borrow_mut().insert(class.name.clone(), class);
    }

    /// Look up a class by name.
    pub fn get(&self, name: &str) -> Option<Arc<Class>> {
        self.classes.borrow().get(name).cloned()
    }

    /// Returns the root `Object` class (always present).
    pub fn object_class(&self) -> Arc<Class> {
        self.classes.borrow().get("Object").expect("Object class missing").clone()
    }

    /// Bind all registered class names as `Value::Class` in the given environment.
    pub fn populate_env(&self, env: &mut crate::env::Env) {
        for (name, class) in self.classes.borrow().iter() {
            env.define(name.clone(), Value::Class(class.clone()));
        }
    }
}

impl Default for ClassRegistry {
    fn default() -> Self {
        Self::new()
    }
}
