use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::class::Class;
use crate::entity::{
    InstanceHandle, InstanceKind, InstanceState, new_entity_data, spawn_class_entity,
};
use crate::env::Env;
use crate::namespace::Namespace;
use crate::value::Value;

/// Registry of root namespaces — those with no parent, bound by simple name in the
/// global environment. Nested namespaces (e.g. `Kernel::Collections`) are NOT stored
/// here; they live in their parent's `sub_namespaces` and are reached via `::` traversal.
pub struct ClassRegistry {
    kernel: Arc<Namespace>,
    /// Root namespaces keyed by their simple name. All entries satisfy `parent == None`.
    roots: RwLock<HashMap<Arc<str>, Arc<Namespace>>>,
    /// Class-entity actors spawned by this registry (bootstrap classes and user-defined
    /// subclasses). Held for lifecycle management; signalled on `shutdown_class_entities`.
    pub(crate) class_entities: RwLock<Vec<Arc<InstanceState>>>,
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
            kind: InstanceKind::ClassEntity(Box::new(new_entity_data(&object_class))),
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
            kind: InstanceKind::ClassEntity(Box::new(new_entity_data(&value_class))),
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
            class_entities: RwLock::new(Vec::new()),
        }
    }

    /// Creates a fully-live registry: `Object` and `Value` have running class-actor
    /// coroutines.  Use this in any context that will dispatch class-side messages.
    ///
    /// Two-phase init:
    ///   1. Build `Arc<ClassRegistry>` with an empty kernel so the Arc is available
    ///      for passing to `spawn_class_entity`.
    ///   2. Spawn a `ClassEntity` actor for each bootstrap class, then register it
    ///      in the kernel namespace and set `class_object`.
    ///
    /// The class actors start with an empty env; `dispatch_class_envelope` refreshes
    /// the env from the registry on every dispatch, so all classes are always in scope.
    pub fn new_arc() -> Arc<Self> {
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

        let value_class = Arc::new(Class {
            name: Arc::from("Value"),
            superclass: Some(object_class.clone()),
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: true,
            class_object: std::sync::OnceLock::new(),
        });

        let mut roots = HashMap::new();
        roots.insert(Arc::from("Kernel"), kernel.clone());

        // Phase 1: wrap in Arc before spawning (spawn_class_entity needs Arc<ClassRegistry>).
        let arc = Arc::new(Self {
            kernel: kernel.clone(),
            roots: RwLock::new(roots),
            class_entities: RwLock::new(Vec::new()),
        });

        // Phase 2: spawn actors with an empty initial env.
        // dispatch_class_envelope refreshes the env on each dispatch, so the empty
        // start is safe even for class-side methods that reference other classes.
        let bootstrap_env = Env::new();

        let object_handle =
            spawn_class_entity(object_class.clone(), arc.clone(), bootstrap_env.clone());
        arc.class_entities.write().push(object_handle.0.clone());
        object_class
            .class_object
            .set(Arc::downgrade(&object_handle.0))
            .ok();
        kernel.register(object_handle);

        let value_handle = spawn_class_entity(value_class.clone(), arc.clone(), bootstrap_env);
        arc.class_entities.write().push(value_handle.0.clone());
        value_class
            .class_object
            .set(Arc::downgrade(&value_handle.0))
            .ok();
        kernel.register(value_handle);

        arc
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

    /// Track a class entity spawned outside of `new_arc` (e.g. user-defined subclasses
    /// created via `subclass:`). Called by builtins after `spawn_class_entity` returns.
    ///
    /// Requires: `handle.0.kind` is `InstanceKind::ClassEntity`.
    /// Ensures: the entity will receive a shutdown signal on `shutdown_class_entities`.
    pub fn register_class_entity(&self, handle: &InstanceHandle) {
        self.class_entities.write().push(handle.0.clone());
    }

    /// Signal all tracked class entities to terminate and drain their mailboxes.
    ///
    /// Ensures: each tracked entity's loop will exit with `EntityStatus::Terminated`
    ///   after processing any message currently in flight.
    pub fn shutdown_class_entities(&self) {
        for state in self.class_entities.read().iter() {
            if let InstanceKind::ClassEntity(d) | InstanceKind::Entity(d) = &state.kind {
                d.signal_shutdown();
            }
        }
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
