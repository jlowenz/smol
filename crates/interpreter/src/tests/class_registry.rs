use super::*;
use crate::env::Env;

#[test]
fn default_creates_valid_registry() {
    let reg = ClassRegistry::default();
    assert!(reg.get_namespace("Kernel").is_some());
}

#[test]
fn get_namespace_returns_kernel() {
    let reg = ClassRegistry::new();
    let ns = reg.get_namespace("Kernel").unwrap();
    assert_eq!(ns.name.as_ref(), "Kernel");
}

#[test]
fn get_namespace_returns_none_for_unknown() {
    let reg = ClassRegistry::new();
    assert!(reg.get_namespace("NoSuchNamespace").is_none());
}

#[test]
fn register_namespace_and_retrieve() {
    let reg = ClassRegistry::new();
    let ns = Arc::new(Namespace::new(Arc::from("MyApp"), None));
    reg.register_namespace(ns.clone());
    let retrieved = reg.get_namespace("MyApp").unwrap();
    assert!(Arc::ptr_eq(&retrieved, &ns));
}

#[test]
#[should_panic(expected = "register_namespace requires a root namespace")]
fn register_nested_namespace_panics() {
    let reg = ClassRegistry::new();
    let parent = Arc::new(Namespace::new(Arc::from("Parent"), None));
    let child = Arc::new(Namespace::new(Arc::from("Child"), Some(parent)));
    reg.register_namespace(child);
}

#[test]
fn populate_env_includes_object_and_kernel() {
    let reg = ClassRegistry::new();
    let mut env = Env::new();
    reg.populate_env(&mut env);

    assert!(env.get(&Arc::from("Kernel")).is_some());
    assert!(env.get(&Arc::from("Object")).is_some());
    assert!(env.get(&Arc::from("Value")).is_some());
}

#[test]
fn value_class_is_value_true() {
    let reg = ClassRegistry::new();
    let kernel = reg.kernel();
    let handle = kernel.lookup_class("Value").unwrap();
    assert!(handle.0.class.is_value);
}

#[test]
fn object_class_is_value_false() {
    let reg = ClassRegistry::new();
    let kernel = reg.kernel();
    let handle = kernel.lookup_class("Object").unwrap();
    assert!(!handle.0.class.is_value);
}
