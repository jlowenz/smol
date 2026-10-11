use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::entity::InstanceHandle;
use crate::value::Value;

/// A named scope of definitions (classes, traits) with explicit import semantics.
///
/// A namespace never implicitly inherits or searches its parent's names; a parent
/// is in scope only if explicitly imported via `add_import`, like any other
/// namespace. `parent` exists solely for qualified-name construction.
pub struct Namespace {
    pub name: Arc<str>,
    /// For qualified-name construction only. NOT an implicit search scope.
    pub parent: Option<Arc<Namespace>>,
    sub_namespaces: RwLock<HashMap<Arc<str>, Arc<Namespace>>>,
    /// Every named definition in this namespace — a class (as its canonical
    /// `Value::Instance` handle, so pointer identity is stable across lookups) or a
    /// trait value. One table for all kinds: a namespace maps names to definitions.
    entries: RwLock<HashMap<Arc<str>, Value>>,
    /// Explicit imports — parent namespace NOT implicitly included.
    imports: RwLock<Vec<Arc<Namespace>>>,
}

impl Namespace {
    pub fn new(name: Arc<str>, parent: Option<Arc<Namespace>>) -> Self {
        Self {
            name,
            parent,
            sub_namespaces: RwLock::new(HashMap::new()),
            entries: RwLock::new(HashMap::new()),
            imports: RwLock::new(Vec::new()),
        }
    }

    /// Register a class as defined in this namespace.
    ///
    /// Ensures: `handle`'s class is an entry under its own name (replacing any
    ///   previous entry of that name), and the class's namespace link points here
    ///   unless it was already set — a class belongs to exactly one namespace, the
    ///   first one it is registered in.
    pub fn register(self: &Arc<Self>, handle: InstanceHandle) {
        let _ = handle.0.class.namespace.set(Arc::downgrade(self));
        self.entries
            .write()
            .insert(handle.0.class.name.clone(), Value::Instance(handle));
    }

    /// Bind a non-class definition (e.g. a trait) under `name`, replacing any
    /// previous entry of that name.
    pub fn bind(&self, name: Arc<str>, value: Value) {
        self.entries.write().insert(name, value);
    }

    pub fn register_sub(&self, ns: Arc<Namespace>) {
        self.sub_namespaces.write().insert(ns.name.clone(), ns);
    }

    pub fn add_import(&self, ns: Arc<Namespace>) {
        self.imports.write().push(ns);
    }

    /// Resolve a simple name to a definition: own entries first, then each import's
    /// own entries, in import order. Does NOT search the parent namespace.
    pub fn resolve(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.entries.read().get(name) {
            return Some(v.clone());
        }
        for imp in self.imports.read().iter() {
            if let Some(v) = imp.entries.read().get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    /// Look up a class by simple name with [`Namespace::resolve`]'s search order,
    /// answering only class entries.
    ///
    /// Returns the canonical `InstanceHandle` for the class. The `Arc<Class>`
    /// descriptor is accessible as `handle.0.class`.
    pub fn lookup_class(&self, name: &str) -> Option<InstanceHandle> {
        match self.resolve(name)? {
            Value::Instance(h) => Some(h),
            _ => None,
        }
    }

    /// Snapshot of this namespace's own entries (not its imports).
    pub(crate) fn own_entries(&self) -> Vec<(Arc<str>, Value)> {
        self.entries
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Look up a sub-namespace by name.
    pub fn lookup_sub(&self, name: &str) -> Option<Arc<Namespace>> {
        self.sub_namespaces.read().get(name).cloned()
    }

    /// Fully-qualified name built from parent chain (e.g. `Kernel::Collections`).
    pub fn qualified_name(&self) -> String {
        match &self.parent {
            None => self.name.to_string(),
            Some(p) => format!("{}::{}", p.qualified_name(), self.name),
        }
    }
}

impl std::fmt::Debug for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Namespace({})", self.qualified_name())
    }
}

#[cfg(test)]
#[path = "tests/namespace.rs"]
mod tests;
