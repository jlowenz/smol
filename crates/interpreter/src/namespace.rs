use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::entity::InstanceHandle;

/// A named container of classes with explicit import semantics.
///
/// `parent` is used only for constructing fully-qualified names; parent items
/// are NOT implicitly in scope — callers must import explicitly via `add_import`.
pub struct Namespace {
    pub name: Arc<str>,
    /// For qualified-name construction only. NOT an implicit search scope.
    pub parent: Option<Arc<Namespace>>,
    sub_namespaces: RwLock<HashMap<Arc<str>, Arc<Namespace>>>,
    /// Canonical `InstanceHandle` for each registered class. Storing the handle
    /// (not `Arc<Class>`) ensures pointer-identity comparisons on class objects
    /// are stable across multiple calls to `populate_env`.
    pub(crate) classes: RwLock<HashMap<Arc<str>, InstanceHandle>>,
    /// Explicit imports — parent namespace NOT implicitly included.
    imports: RwLock<Vec<Arc<Namespace>>>,
}

impl Namespace {
    pub fn new(name: Arc<str>, parent: Option<Arc<Namespace>>) -> Self {
        Self {
            name,
            parent,
            sub_namespaces: RwLock::new(HashMap::new()),
            classes: RwLock::new(HashMap::new()),
            imports: RwLock::new(Vec::new()),
        }
    }

    pub fn register(&self, handle: InstanceHandle) {
        self.classes
            .write()
            .insert(handle.0.class.name.clone(), handle);
    }

    pub fn register_sub(&self, ns: Arc<Namespace>) {
        self.sub_namespaces.write().insert(ns.name.clone(), ns);
    }

    pub fn add_import(&self, ns: Arc<Namespace>) {
        self.imports.write().push(ns);
    }

    /// Look up a class by simple name: own classes first, then imports (in order).
    /// Does NOT search the parent namespace.
    ///
    /// Returns the canonical `InstanceHandle` for the class. The `Arc<Class>`
    /// descriptor is accessible as `handle.0.class`.
    pub fn lookup_class(&self, name: &str) -> Option<InstanceHandle> {
        if let Some(h) = self.classes.read().get(name) {
            return Some(h.clone());
        }
        for imp in self.imports.read().iter() {
            if let Some(h) = imp.classes.read().get(name) {
                return Some(h.clone());
            }
        }
        None
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
