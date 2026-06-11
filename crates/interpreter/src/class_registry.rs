use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::class::Class;
use crate::entity::{
    EntityId, InstanceHandle, InstanceKind, InstanceState, new_entity_data, spawn_class_entity,
};
use crate::env::Env;
use crate::namespace::Namespace;
use crate::value::Value;
use triple_store::predicate_registry::Visibility;
use triple_store::predicates::Cardinality;
use triple_store::store::Store;
use triple_store::term::Predicate;

/// Registry of root namespaces — those with no parent, bound by simple name in the
/// global environment. Nested namespaces (e.g. `Kernel::Collections`) are NOT stored
/// here; they live in their parent's `sub_namespaces` and are reached via `::` traversal.
pub struct ClassRegistry {
    kernel: Arc<Namespace>,
    /// Root namespaces keyed by their simple name. All entries satisfy `parent == None`.
    roots: RwLock<HashMap<Arc<str>, Arc<Namespace>>>,
    /// Class-entity actors spawned by this registry (bootstrap classes and user-defined
    /// subclasses). Keyed by EntityId for lifecycle management and future entity lookups.
    /// Signalled on `shutdown_class_entities`.
    pub(crate) class_entities: RwLock<HashMap<EntityId, Arc<InstanceState>>>,
    /// Instance entities (non-class, non-value actors) spawned during this session.
    /// Shared across all per-entity evaluators so that `World instancesOf:` and
    /// `EntityRef` resolution can find instances regardless of which evaluator spawned them.
    pub(crate) instance_entities: RwLock<HashMap<EntityId, Arc<InstanceState>>>,
    /// Shared store handle for this session. All entities share the same underlying
    /// store via `clone_handle()`.
    pub store: Store,
    /// The single canonical predicate for indexed-slot facts (`Cardinality::Indexed`).
    /// Registered once at startup; shared across all entity classes.
    pub(crate) indexed_slots_pred: Predicate,
    /// Predicate asserting the class membership of an entity.
    /// `(entity, __instance_of__, class_entity)` — `Visibility::Shared` so any
    /// entity may read; `Cardinality::Single` (one class per entity).
    pub(crate) instance_of_pred: Predicate,
}

impl ClassRegistry {
    /// Creates a new registry with `Kernel` namespace pre-populated with `Object` and `Value`.
    ///
    /// Class-entity actors are NOT spawned (no coroutines). Use `new_arc` for a
    /// fully-live registry that dispatches messages.
    pub fn new() -> Self {
        let store = Store::in_memory();
        let indexed_slots_pred = store
            .declare(
                "__indexed_slots__",
                Visibility::Private,
                Cardinality::Indexed,
            )
            .expect("failed to register __indexed_slots__ predicate");
        let instance_of_pred = store
            .declare("__instance_of__", Visibility::Shared, Cardinality::Single)
            .expect("failed to register __instance_of__ predicate");
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
            kind: InstanceKind::ClassEntity(Box::new(new_entity_data(
                &object_class,
                store.clone_handle(),
            ))),
        });
        object_class
            .class_object
            .set(Arc::downgrade(&object_state))
            .ok();
        kernel.register(InstanceHandle(object_state));

        // `Value` is the built-in passive base class (is_value: true), a subclass of Object.
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
        let value_state = Arc::new(InstanceState {
            class: value_class.clone(),
            kind: InstanceKind::ClassEntity(Box::new(new_entity_data(
                &value_class,
                store.clone_handle(),
            ))),
        });
        value_class
            .class_object
            .set(Arc::downgrade(&value_state))
            .ok();
        kernel.register(InstanceHandle(value_state));

        // `Nil` is the class of `nil` (analogous to Smalltalk's `UndefinedObject`).
        // Subclass of Object; user-defined methods on `Nil` are dispatched for `Value::Nil`
        // receivers via `invoke_primitive_method`.
        let nil_class = Arc::new(Class {
            name: Arc::from("Nil"),
            superclass: Some(object_class.clone()),
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
            class_object: std::sync::OnceLock::new(),
        });
        let nil_state = Arc::new(InstanceState {
            class: nil_class.clone(),
            kind: InstanceKind::ClassEntity(Box::new(new_entity_data(
                &nil_class,
                store.clone_handle(),
            ))),
        });
        nil_class.class_object.set(Arc::downgrade(&nil_state)).ok();
        kernel.register(InstanceHandle(nil_state));

        // `Block` is the class of block closures. Subclass of Object; user-defined
        // methods dispatch for `Value::Block` receivers via `invoke_primitive_method`.
        let block_class = Arc::new(Class {
            name: Arc::from("Block"),
            superclass: Some(object_class.clone()),
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
            class_object: std::sync::OnceLock::new(),
        });
        let block_state = Arc::new(InstanceState {
            class: block_class.clone(),
            kind: InstanceKind::ClassEntity(Box::new(new_entity_data(
                &block_class,
                store.clone_handle(),
            ))),
        });
        block_class
            .class_object
            .set(Arc::downgrade(&block_state))
            .ok();
        kernel.register(InstanceHandle(block_state));

        // `Int` is the class of small integers (`Value::Int`). Subclass of Object;
        // user-defined methods dispatch via `invoke_primitive_method`.
        let int_class = Arc::new(Class {
            name: Arc::from("Int"),
            superclass: Some(object_class.clone()),
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
            class_object: std::sync::OnceLock::new(),
        });
        let int_state = Arc::new(InstanceState {
            class: int_class.clone(),
            kind: InstanceKind::ClassEntity(Box::new(new_entity_data(
                &int_class,
                store.clone_handle(),
            ))),
        });
        int_class.class_object.set(Arc::downgrade(&int_state)).ok();
        kernel.register(InstanceHandle(int_state));

        let mut roots = HashMap::new();
        roots.insert(Arc::from("Kernel"), kernel.clone());

        // Primitive classes have no own_instance_vars; register_class_predicates is a
        // no-op for them. No call needed here — user-defined subclasses register via
        // handle_subclass.
        Self {
            kernel,
            roots: RwLock::new(roots),
            class_entities: RwLock::new(HashMap::new()),
            instance_entities: RwLock::new(HashMap::new()),
            store,
            indexed_slots_pred,
            instance_of_pred,
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
    /// the env on every dispatch, so all classes are always in scope.
    pub fn new_arc() -> Arc<Self> {
        let store = Store::in_memory();
        let indexed_slots_pred = store
            .declare(
                "__indexed_slots__",
                Visibility::Private,
                Cardinality::Indexed,
            )
            .expect("failed to register __indexed_slots__ predicate");
        let instance_of_pred = store
            .declare("__instance_of__", Visibility::Shared, Cardinality::Single)
            .expect("failed to register __instance_of__ predicate");
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

        let nil_class = Arc::new(Class {
            name: Arc::from("Nil"),
            superclass: Some(object_class.clone()),
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
            class_object: std::sync::OnceLock::new(),
        });

        let block_class = Arc::new(Class {
            name: Arc::from("Block"),
            superclass: Some(object_class.clone()),
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
            class_object: std::sync::OnceLock::new(),
        });

        let int_class = Arc::new(Class {
            name: Arc::from("Int"),
            superclass: Some(object_class.clone()),
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
            class_object: std::sync::OnceLock::new(),
        });

        let mut roots = HashMap::new();
        roots.insert(Arc::from("Kernel"), kernel.clone());

        // Phase 1: wrap in Arc before spawning (spawn_class_entity needs Arc<ClassRegistry>).
        let arc = Arc::new(Self {
            kernel: kernel.clone(),
            roots: RwLock::new(roots),
            class_entities: RwLock::new(HashMap::new()),
            instance_entities: RwLock::new(HashMap::new()),
            store: store.clone_handle(),
            indexed_slots_pred,
            instance_of_pred,
        });

        // Phase 2: spawn actors with an empty initial env.
        // dispatch_class_envelope refreshes the env on each dispatch, so the empty
        // start is safe even for class-side methods that reference other classes.
        let bootstrap_env = Env::new();

        let object_handle = spawn_class_entity(
            object_class.clone(),
            arc.clone(),
            bootstrap_env.clone(),
            store.clone_handle(),
        );
        let object_id = match &object_handle.0.kind {
            InstanceKind::ClassEntity(d) => d.id,
            _ => unreachable!(),
        };
        arc.class_entities
            .write()
            .insert(object_id, object_handle.0.clone());
        object_class
            .class_object
            .set(Arc::downgrade(&object_handle.0))
            .ok();
        kernel.register(object_handle);

        let value_handle = spawn_class_entity(
            value_class.clone(),
            arc.clone(),
            bootstrap_env.clone(),
            store.clone_handle(),
        );
        let value_id = match &value_handle.0.kind {
            InstanceKind::ClassEntity(d) => d.id,
            _ => unreachable!(),
        };
        arc.class_entities
            .write()
            .insert(value_id, value_handle.0.clone());
        value_class
            .class_object
            .set(Arc::downgrade(&value_handle.0))
            .ok();
        kernel.register(value_handle);

        let nil_handle = spawn_class_entity(
            nil_class.clone(),
            arc.clone(),
            bootstrap_env.clone(),
            store.clone_handle(),
        );
        let nil_id = match &nil_handle.0.kind {
            InstanceKind::ClassEntity(d) => d.id,
            _ => unreachable!(),
        };
        arc.class_entities
            .write()
            .insert(nil_id, nil_handle.0.clone());
        nil_class
            .class_object
            .set(Arc::downgrade(&nil_handle.0))
            .ok();
        kernel.register(nil_handle);

        let block_handle = spawn_class_entity(
            block_class.clone(),
            arc.clone(),
            bootstrap_env.clone(),
            store.clone_handle(),
        );
        let block_id = match &block_handle.0.kind {
            InstanceKind::ClassEntity(d) => d.id,
            _ => unreachable!(),
        };
        arc.class_entities
            .write()
            .insert(block_id, block_handle.0.clone());
        block_class
            .class_object
            .set(Arc::downgrade(&block_handle.0))
            .ok();
        kernel.register(block_handle);

        let int_handle = spawn_class_entity(
            int_class.clone(),
            arc.clone(),
            bootstrap_env,
            store.clone_handle(),
        );
        let int_id = match &int_handle.0.kind {
            InstanceKind::ClassEntity(d) => d.id,
            _ => unreachable!(),
        };
        arc.class_entities
            .write()
            .insert(int_id, int_handle.0.clone());
        int_class
            .class_object
            .set(Arc::downgrade(&int_handle.0))
            .ok();
        kernel.register(int_handle);

        arc.register_class_predicates(&object_class);
        arc.register_class_predicates(&value_class);
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

    /// Register store predicates for all `own_instance_vars` of a class.
    ///
    /// Requires: `class.is_value` must be `false`; value classes are not journalled.
    ///   (A value class call is a silent no-op.)
    /// Ensures: each `own_instance_var` name is declared as `(Private, Single)` in the
    ///   shared store. Re-declaration with identical metadata is idempotent. Predicates
    ///   for inherited vars are NOT re-declared here; they were registered when the
    ///   declaring superclass was installed.
    pub(crate) fn register_class_predicates(&self, class: &Arc<Class>) {
        if class.is_value {
            return;
        }
        for ivar in &class.own_instance_vars {
            let _ = self
                .store
                .declare(ivar, Visibility::Private, Cardinality::Single);
        }
    }

    /// Track a class entity spawned outside of `new_arc` (e.g. user-defined subclasses
    /// created via `subclass:`). Called by builtins after `spawn_class_entity` returns.
    ///
    /// Requires: `handle.0.kind` is `InstanceKind::ClassEntity`.
    /// Ensures: the entity will receive a shutdown signal on `shutdown_class_entities`.
    pub fn register_class_entity(&self, handle: &InstanceHandle) {
        let id = match &handle.0.kind {
            InstanceKind::ClassEntity(d) | InstanceKind::Entity(d) => d.id,
            InstanceKind::ValueObject(_) => panic!("register_class_entity called on ValueObject"),
        };
        self.class_entities.write().insert(id, handle.0.clone());
    }

    /// Signal all tracked class and instance entities to terminate.
    ///
    /// Ensures: each tracked entity's loop will exit with `EntityStatus::Terminated`
    ///   after processing any message currently in flight.
    pub fn shutdown_class_entities(&self) {
        for state in self.class_entities.read().values() {
            if let InstanceKind::ClassEntity(d) | InstanceKind::Entity(d) = &state.kind {
                d.signal_shutdown();
            }
        }
        for state in self.instance_entities.read().values() {
            if let InstanceKind::Entity(d) = &state.kind {
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
