use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::{Mutex, RwLock};

use crate::class::Class;
use crate::coverage::CoverageCollector;
use crate::entity::{EntityId, InstanceHandle, InstanceKind, InstanceState, spawn_class_entity};
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
    /// Shared, opt-in execution-coverage sink. `None` until `enable_coverage` is
    /// called; then every evaluator sharing this registry — main thread and
    /// entity coroutines — records executed statement spans into it. See
    /// [`crate::coverage`].
    pub(crate) coverage: OnceLock<Arc<Mutex<CoverageCollector>>>,
}

impl ClassRegistry {
    /// Enable execution-coverage collection on this registry (idempotent).
    /// Every evaluator sharing this registry — main thread and entity coroutines
    /// — records executed statement spans into the returned collector. Read it at
    /// a quiescent point to build the coverage report.
    pub fn enable_coverage(&self) -> Arc<Mutex<CoverageCollector>> {
        self.coverage
            .get_or_init(|| Arc::new(Mutex::new(CoverageCollector::new())))
            .clone()
    }

    /// Creates the registry — the single bootstrap path. The bootstrap classes
    /// (`Object`, `Value`, `Nil`, `Block`) are registered in `Kernel` with running
    /// class-actor coroutines; stop them with `shutdown_class_entities`.
    ///
    /// Two-phase init:
    ///   1. Build `Arc<ClassRegistry>` with an empty kernel so the Arc is available
    ///      for passing to `spawn_class_entity`.
    ///   2. Spawn a `ClassEntity` actor for each bootstrap class, then register it
    ///      in the kernel namespace and set `class_object`.
    ///
    /// The class actors start with an empty env: methods resolve free names through
    /// their defining class's namespace, read live, so no env snapshot of the
    /// registry is needed.
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

        let object_class = Arc::new(Class::new(
            Arc::from("Object"),
            None,
            vec![],
            vec![],
            None,
            false,
        ));

        let value_class = Arc::new(Class::new(
            Arc::from("Value"),
            Some(object_class.clone()),
            vec![],
            vec![],
            None,
            true,
        ));

        let nil_class = Arc::new(Class::new(
            Arc::from("Nil"),
            Some(object_class.clone()),
            vec![],
            vec![],
            None,
            false,
        ));

        let block_class = Arc::new(Class::new(
            Arc::from("Block"),
            Some(object_class.clone()),
            vec![],
            vec![],
            None,
            false,
        ));

        // Numeric tower + String/Symbol live in kernel.smol (see new()).

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
            coverage: OnceLock::new(),
        });

        // Phase 2: spawn actors with an empty initial env. Class-side methods resolve
        // other classes through their defining namespace, not the env.
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
            bootstrap_env,
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

    /// Seed `env` with Kernel's definitions (classes and traits) and every root
    /// namespace name. Sub-namespaces are intentionally excluded — they are reached
    /// via `::` only.
    pub fn populate_env(&self, env: &mut crate::env::Env) {
        for (name, value) in self.kernel.own_entries() {
            env.define(name, value);
        }
        for (name, ns) in self.roots.read().iter() {
            env.define(name.clone(), Value::Namespace(ns.clone()));
        }
    }
}

impl Drop for ClassRegistry {
    /// When whole-suite coverage is enabled (`$SMOL_COVERAGE`), flush the global
    /// kernel coverage report as registries are torn down. A no-op otherwise.
    fn drop(&mut self) {
        crate::coverage::flush_global_report();
    }
}

#[cfg(test)]
#[path = "tests/class_registry.rs"]
mod tests;
