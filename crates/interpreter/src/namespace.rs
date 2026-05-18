use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::class::Class;

/// A named container of classes with explicit import semantics.
///
/// `parent` is used only for constructing fully-qualified names; parent items
/// are NOT implicitly in scope — callers must import explicitly via `add_import`.
pub struct Namespace {
    pub name: Arc<str>,
    /// For qualified-name construction only. NOT an implicit search scope.
    pub parent: Option<Arc<Namespace>>,
    sub_namespaces: RefCell<HashMap<Arc<str>, Arc<Namespace>>>,
    pub(crate) classes: RefCell<HashMap<Arc<str>, Arc<Class>>>,
    /// Explicit imports — parent namespace NOT implicitly included.
    imports: RefCell<Vec<Arc<Namespace>>>,
}

impl Namespace {
    pub fn new(name: Arc<str>, parent: Option<Arc<Namespace>>) -> Self {
        Self {
            name,
            parent,
            sub_namespaces: RefCell::new(HashMap::new()),
            classes: RefCell::new(HashMap::new()),
            imports: RefCell::new(Vec::new()),
        }
    }

    pub fn register(&self, class: Arc<Class>) {
        self.classes.borrow_mut().insert(class.name.clone(), class);
    }

    pub fn register_sub(&self, ns: Arc<Namespace>) {
        self.sub_namespaces.borrow_mut().insert(ns.name.clone(), ns);
    }

    pub fn add_import(&self, ns: Arc<Namespace>) {
        self.imports.borrow_mut().push(ns);
    }

    /// Look up a class by simple name: own classes first, then imports (in order).
    /// Does NOT search the parent namespace.
    pub fn lookup_class(&self, name: &str) -> Option<Arc<Class>> {
        if let Some(c) = self.classes.borrow().get(name) {
            return Some(c.clone());
        }
        for imp in self.imports.borrow().iter() {
            if let Some(c) = imp.classes.borrow().get(name) {
                return Some(c.clone());
            }
        }
        None
    }

    /// Look up a sub-namespace by name.
    pub fn lookup_sub(&self, name: &str) -> Option<Arc<Namespace>> {
        self.sub_namespaces.borrow().get(name).cloned()
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
