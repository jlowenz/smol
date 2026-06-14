use crate::test_helpers::{eval, eval_diff};

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
    assert_eq!(eval_diff(prog), Value::Int(5));
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
            id: EntityId(99),
            data: obj,
            mailbox: Mutex::new(VecDeque::new()),
            mailbox_ready: Condvar::new(),
            status: Mutex::new(EntityStatus::Running),
            shutdown: std::sync::atomic::AtomicBool::new(false),
            store: triple_store::store::Store::in_memory(),
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
fn instance_kind_class_entity_debug() {
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
    let kind = InstanceKind::ClassEntity(Box::new(crate::entity::new_entity_data(
        &cls,
        triple_store::store::Store::in_memory(),
    )));
    let s = format!("{kind:?}");
    assert!(s.starts_with("ClassEntity(id="), "got: {s}");
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
fn instance_handle_class_entity_debug() {
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
        class: cls.clone(),
        kind: InstanceKind::ClassEntity(Box::new(crate::entity::new_entity_data(
            &cls,
            triple_store::store::Store::in_memory(),
        ))),
    }));
    let s = format!("{handle:?}");
    assert!(s.contains("ClassEntity"), "got: {s}");
    assert!(s.contains("MyClass"), "got: {s}");
}

/// Stress test: 100 entities × 1 000 messages each at the current stack size.
#[test]
fn entity_stress_100x1000() {
    let src = r#"
        | results |
        results := OrderedCollection new.
        1 to: 100 do: [:i |
            | vh |
            vh := ValueHolder initialValue: 0.
            1 to: 1000 do: [:j | vh update: [:v | v + 1]].
            results add: vh get
        ].
        results inject: 0 into: [:acc :e | acc + e]
    "#;
    assert_eq!(eval(src), Value::Int(100_000));
}

/// Stack probe using the PRODUCTION coroutine path: load kernel on the test
/// thread (which has a full OS stack), then evaluate inside a coroutine using
/// only `Evaluator::with_registry` — mirroring how `run_entity_loop` works.
/// This isolates the true coroutine stack cost from kernel-loading overhead.
#[test]
fn entity_stack_depth_probe() {
    let src = r#"
        | col acc |
        col := OrderedCollection new.
        1 to: 20 do: [:i | col add: i].
        acc := ValueHolder initialValue: 0.
        [ col do: [:each |
            [ acc update: [:v | v + each] ] ensure: [ nil ]
        ] ] on: Error do: [:e | nil].
        acc get await
    "#;
    // Load kernel on the test thread (not inside the coroutine).
    crate::entity::init_coroutine_stack();
    let base_ev = crate::eval::Evaluator::new();
    let registry = base_ev.registry.clone();
    let prog = crate::parser::Parser::parse(src).expect("parse");

    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    may::go!(move || {
        let ev = crate::eval::Evaluator::with_registry(registry);
        let mut env = crate::env::Env::new();
        ev.registry.populate_env(&mut env);
        let v = ev.eval_program(&prog.stmts, &mut env).expect("eval");
        tx.send(v).unwrap();
    });
    assert_eq!(rx.recv().unwrap(), Value::Int(210));
}
