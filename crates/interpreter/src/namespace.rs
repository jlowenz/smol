use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::class::Class;

/// A named container of classes with explicit import semantics.
///
/// `parent` is used only for constructing fully-qualified names; parent items
/// are NOT implicitly in scope — callers must import explicitly via `add_import`.
pub struct Namespace {
    pub name: Arc<str>,
    /// For qualified-name construction only. NOT an implicit search scope.
    pub parent: Option<Arc<Namespace>>,
    sub_namespaces: RwLock<HashMap<Arc<str>, Arc<Namespace>>>,
    pub(crate) classes: RwLock<HashMap<Arc<str>, Arc<Class>>>,
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

    pub fn register(&self, class: Arc<Class>) {
        self.classes.write().insert(class.name.clone(), class);
    }

    pub fn register_sub(&self, ns: Arc<Namespace>) {
        self.sub_namespaces.write().insert(ns.name.clone(), ns);
    }

    pub fn add_import(&self, ns: Arc<Namespace>) {
        self.imports.write().push(ns);
    }

    /// Look up a class by simple name: own classes first, then imports (in order).
    /// Does NOT search the parent namespace.
    pub fn lookup_class(&self, name: &str) -> Option<Arc<Class>> {
        if let Some(c) = self.classes.read().get(name) {
            return Some(c.clone());
        }
        for imp in self.imports.read().iter() {
            if let Some(c) = imp.classes.read().get(name) {
                return Some(c.clone());
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
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::class::Class;
    use parking_lot::RwLock;

    fn bare_class(name: &str) -> Arc<Class> {
        Arc::new(Class {
            name: Arc::from(name),
            superclass: None,
            own_instance_vars: vec![],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
        })
    }

    #[test]
    fn lookup_sub_found_and_missing() {
        let parent = Arc::new(Namespace::new(Arc::from("Parent"), None));
        let child = Arc::new(Namespace::new(Arc::from("Child"), Some(parent.clone())));
        parent.register_sub(child);
        assert!(parent.lookup_sub("Child").is_some());
        assert!(parent.lookup_sub("Missing").is_none());
    }

    #[test]
    fn qualified_name_with_parent_chain() {
        let root = Arc::new(Namespace::new(Arc::from("Kernel"), None));
        let child = Arc::new(Namespace::new(Arc::from("Collections"), Some(root)));
        assert_eq!(child.qualified_name(), "Kernel::Collections");
    }

    #[test]
    fn lookup_class_via_import() {
        let ns_a = Arc::new(Namespace::new(Arc::from("A"), None));
        let ns_b = Arc::new(Namespace::new(Arc::from("B"), None));
        ns_b.register(bare_class("Widget"));
        assert!(ns_a.lookup_class("Widget").is_none());
        ns_a.add_import(ns_b);
        assert!(ns_a.lookup_class("Widget").is_some());
    }

    #[test]
    fn own_class_shadows_import() {
        let ns_a = Arc::new(Namespace::new(Arc::from("A"), None));
        let ns_b = Arc::new(Namespace::new(Arc::from("B"), None));
        ns_b.register(bare_class("Foo"));
        ns_a.register(bare_class("Foo"));
        ns_a.add_import(ns_b);
        let found = ns_a.lookup_class("Foo").unwrap();
        // own class is returned (not the import)
        assert!(Arc::ptr_eq(&found, &ns_a.classes.read()["Foo"]));
    }

    #[test]
    fn debug_includes_qualified_name() {
        let ns = Arc::new(Namespace::new(Arc::from("Kernel"), None));
        assert!(format!("{ns:?}").contains("Kernel"));
    }
}
