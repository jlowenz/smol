use crate::test_helpers::eval;

use super::*;

#[test]
fn future_resolves_and_wakes_waiter() {
    let fut = FutureHandle::new();
    let fut2 = fut.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(10));
        fut2.resolve_ok(Value::Int(42));
    });
    assert_eq!(fut.wait(), Ok(Value::Int(42)));
    t.join().unwrap();
}

#[test]
fn future_already_resolved_returns_immediately() {
    let fut = FutureHandle::new();
    fut.resolve_ok(Value::Int(7));
    assert_eq!(fut.wait(), Ok(Value::Int(7)));
    assert_eq!(fut.wait(), Ok(Value::Int(7))); // idempotent reads
}

#[test]
#[should_panic(expected = "FutureHandle resolved twice")]
fn future_double_resolve_panics_in_debug() {
    let fut = FutureHandle::new();
    fut.resolve_ok(Value::Int(1));
    fut.resolve_ok(Value::Int(2)); // debug_assert fires
}

#[test]
fn message_send_resolves_future() {
    let prog = r#"
| f f2 | Object subclass: #Five.
Five >> value [ ^5 ].
Five >> anotherFive [ ^Five new ].
f := Five new.
f anotherFive anotherFive value.
"#;
    assert_eq!(eval(prog), Value::Int(5));
}

#[test]
fn instance_handle_entity_debug() {
    use crate::class::Class;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    let cls = Arc::new(Class {
        name: Arc::from("TestClass"),
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
    let obj = Arc::new(crate::object::Object::new(cls.clone()));
    let handle = InstanceHandle(Arc::new(InstanceState {
        class: cls,
        kind: InstanceKind::Entity(Box::new(EntityData {
            id: 99,
            data: obj,
            mailbox: Mutex::new(VecDeque::new()),
            mailbox_ready: Condvar::new(),
            status: Mutex::new(EntityStatus::Running),
        })),
    }));
    assert!(format!("{handle:?}").contains("99"));
}

#[test]
fn future_default() {
    let fut = FutureHandle::default();
    fut.resolve_ok(Value::Int(1));
    assert_eq!(fut.wait(), Ok(Value::Int(1)));
}

#[test]
fn future_is_resolved() {
    let fut = FutureHandle::new();
    assert!(!fut.is_resolved());
    fut.resolve_ok(Value::Int(5));
    assert!(fut.is_resolved());
}

#[test]
fn future_try_get() {
    let fut = FutureHandle::new();
    assert!(fut.try_get().is_none());
    fut.resolve_ok(Value::Int(7));
    assert_eq!(fut.try_get(), Some(Ok(Value::Int(7))));
}

#[test]
fn future_debug_shows_resolved_state() {
    let fut = FutureHandle::new();
    assert!(format!("{fut:?}").contains("resolved=false"));
    fut.resolve_ok(Value::Int(1));
    assert!(format!("{fut:?}").contains("resolved=true"));
}

#[test]
fn instance_kind_value_object_debug() {
    use crate::class::Class;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    let cls = Arc::new(Class {
        name: Arc::from("TestClass"),
        superclass: None,
        own_instance_vars: vec![],
        own_class_vars: vec![],
        class_var_storage: RwLock::new(HashMap::new()),
        indexed_slot_kind: None,
        methods: RwLock::new(HashMap::new()),
        class_methods: RwLock::new(HashMap::new()),
        is_value: true,
        class_object: std::sync::OnceLock::new(),
    });
    let obj = Arc::new(crate::object::Object::new(cls.clone()));
    let kind = InstanceKind::ValueObject(obj);
    assert_eq!(format!("{kind:?}"), "ValueObject");
}

#[test]
fn instance_kind_class_object_debug() {
    let kind = InstanceKind::ClassObject;
    assert_eq!(format!("{kind:?}"), "ClassObject");
}

#[test]
fn instance_handle_value_object_debug() {
    use crate::class::Class;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    let cls = Arc::new(Class {
        name: Arc::from("MyValue"),
        superclass: None,
        own_instance_vars: vec![],
        own_class_vars: vec![],
        class_var_storage: RwLock::new(HashMap::new()),
        indexed_slot_kind: None,
        methods: RwLock::new(HashMap::new()),
        class_methods: RwLock::new(HashMap::new()),
        is_value: true,
        class_object: std::sync::OnceLock::new(),
    });
    let obj = Arc::new(crate::object::Object::new(cls.clone()));
    let handle = InstanceHandle(Arc::new(InstanceState {
        class: cls,
        kind: InstanceKind::ValueObject(obj),
    }));
    let s = format!("{handle:?}");
    assert!(s.contains("ValueObject"), "got: {s}");
    assert!(s.contains("MyValue"), "got: {s}");
}

#[test]
fn instance_handle_class_object_debug() {
    use crate::class::Class;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    let cls = Arc::new(Class {
        name: Arc::from("MyClass"),
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
    let handle = InstanceHandle(Arc::new(InstanceState {
        class: cls,
        kind: InstanceKind::ClassObject,
    }));
    let s = format!("{handle:?}");
    assert!(s.contains("ClassObject"), "got: {s}");
    assert!(s.contains("MyClass"), "got: {s}");
}
