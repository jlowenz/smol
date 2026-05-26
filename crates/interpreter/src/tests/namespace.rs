use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::class::Class;
use crate::entity::{InstanceHandle, InstanceKind, InstanceState, new_entity_data};
use parking_lot::RwLock;

fn bare_handle(name: &str) -> InstanceHandle {
    let cls = Arc::new(Class {
        name: Arc::from(name),
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
    let data = new_entity_data(&cls);
    InstanceHandle(Arc::new(InstanceState {
        class: cls,
        kind: InstanceKind::ClassEntity(Box::new(data)),
    }))
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
    ns_b.register(bare_handle("Widget"));
    assert!(ns_a.lookup_class("Widget").is_none());
    ns_a.add_import(ns_b);
    assert!(ns_a.lookup_class("Widget").is_some());
}

#[test]
fn own_class_shadows_import() {
    let ns_a = Arc::new(Namespace::new(Arc::from("A"), None));
    let ns_b = Arc::new(Namespace::new(Arc::from("B"), None));
    ns_b.register(bare_handle("Foo"));
    let own_foo = bare_handle("Foo");
    ns_a.register(own_foo.clone());
    ns_a.add_import(ns_b);
    let found = ns_a.lookup_class("Foo").unwrap();
    assert!(Arc::ptr_eq(&found.0, &own_foo.0));
}

#[test]
fn debug_includes_qualified_name() {
    let ns = Arc::new(Namespace::new(Arc::from("Kernel"), None));
    assert!(format!("{ns:?}").contains("Kernel"));
}
