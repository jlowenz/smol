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
            tx_frames: Mutex::new(Vec::new()),
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

/// `handle_commit_error` — direct unit test of the extracted fault helper.
///
/// Verifies that a `StoreError` passed to `handle_commit_error`:
///   - sets `EntityStatus::Faulted` on the entity
///   - resolves the current envelope's reply with `EvalError::Signal`
///   - drains and resolves every message in the mailbox with the same error
#[test]
fn handle_commit_error_faults_entity_drains_mailbox() {
    use crate::class::Class;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use triple_store::store::StoreError;

    let cls = Arc::new(Class {
        name: Arc::from("Stub"),
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

    let data = EntityData {
        id: EntityId(1),
        data: Arc::new(crate::object::Object::new(cls.clone())),
        mailbox: Mutex::new(VecDeque::new()),
        mailbox_ready: Condvar::new(),
        status: Mutex::new(EntityStatus::Running),
        shutdown: std::sync::atomic::AtomicBool::new(false),
        store: triple_store::store::Store::in_memory(),
        tx_frames: Mutex::new(Vec::new()),
    };

    // Queue a second message so the mailbox-drain path is exercised.
    let queued_reply = FutureHandle::new();
    data.mailbox.lock().unwrap().push_back(Envelope {
        selector: Arc::from("queued"),
        args: vec![],
        reply: queued_reply.clone(),
        inherited_chain: vec![],
        tx_tag: None,
    });

    let ev = crate::eval::Evaluator::new_without_kernel();
    let envelope = Envelope {
        selector: Arc::from("test"),
        args: vec![],
        reply: FutureHandle::new(),
        inherited_chain: vec![],
        tx_tag: None,
    };

    handle_commit_error(
        StoreError::Internal("injected failure"),
        &data,
        &ev,
        &envelope,
    );

    assert!(
        matches!(*data.status.lock().unwrap(), EntityStatus::Faulted(_)),
        "entity must be Faulted after commit error"
    );
    assert!(
        envelope.reply.is_resolved(),
        "current envelope reply must be resolved"
    );
    assert!(
        queued_reply.is_resolved(),
        "queued mailbox reply must be drained and resolved"
    );
    assert!(
        data.mailbox.lock().unwrap().is_empty(),
        "mailbox must be empty after drain"
    );
}

/// entity.rs:537 — `handle_commit_error` called from the `Err(EvalError::Return)` arm.
///
/// An entity spawned with `AlwaysFailingStore` receives a block whose
/// `home_depth = 2` (doesn't match the receiver's `run:` frame at depth 1).
/// The `^` inside the block fires a non-local return that exhausts the entity's
/// unwind stack → `EvalError::Return` escapes `dispatch_envelope`.  The active
/// transaction (from `ran := true`) then fails to commit → `handle_commit_error`
/// is invoked from the NLR arm, covering the previously-unreachable lines.
#[test]
fn nlr_with_failing_commit_faults_entity() {
    use crate::ast::{Expr, Literal, Stmt};
    use crate::entity::spawn_entity_bare;
    use crate::env::Env;
    use crate::value::Block;
    use triple_store::store::Store;
    use triple_store::test_support::AlwaysFailingStore;

    crate::entity::init_coroutine_stack();

    // Define a class with `run: aBlock [ ran := true. ^aBlock value ]`
    // via a full kernel evaluator so predicates and class infrastructure
    // are registered in the in-memory store normally.
    let base_ev = crate::eval::Evaluator::new();
    let mut env = Env::new();
    base_ev.registry.populate_env(&mut env);
    let prog = crate::parser::Parser::parse(
        "Object subclass: #NLRFaultActor instanceVariableNames: 'ran'.",
    )
    .expect("parse");
    base_ev
        .eval_program(&prog.stmts, &mut env)
        .expect("eval class");
    let prog2 =
        crate::parser::Parser::parse("NLRFaultActor >> run: aBlock [ ran := true. ^aBlock value ]")
            .expect("parse method");
    base_ev
        .eval_program(&prog2.stmts, &mut env)
        .expect("eval method");

    // Look up the class.
    let class_handle = base_ev
        .registry
        .kernel()
        .lookup_class("NLRFaultActor")
        .expect("NLRFaultActor must be defined");
    let class = class_handle.0.class.clone();

    // Spawn an instance entity backed by AlwaysFailingStore so its
    // transaction commit always returns Err(StoreError::Internal).
    let inst = spawn_entity_bare(
        class,
        base_ev.registry.clone(),
        env.clone(),
        None,
        Store::new(Arc::new(AlwaysFailingStore)),
    );

    // Construct a block with home_depth=2.  The receiver's `run:` frame is
    // always at depth 1, so the NLR target (2) exhausts the unwind stack.
    let nlr_block = Arc::new(Block {
        params: vec![],
        locals: vec![],
        body: vec![Stmt::Return(Expr::Lit(Literal::Int(77)))],
        env: Env::new(),
        home_depth: 2,
        home_entity: None,
        home_self: None,
    });

    // Push the envelope directly onto the entity's mailbox.
    let reply = FutureHandle::new();
    if let InstanceKind::Entity(d) = &inst.0.kind {
        d.mailbox.lock().unwrap().push_back(Envelope {
            selector: Arc::from("run:"),
            args: vec![Value::Block(nlr_block)],
            reply: reply.clone(),
            inherited_chain: vec![],
            tx_tag: None,
        });
        d.mailbox_ready.notify_all();
    }

    // Wait for the reply — must be an error because the commit failed.
    let result = reply.wait();
    assert!(
        result.is_err(),
        "reply must be Err after commit failure: {result:?}"
    );

    // Entity must have faulted.
    if let InstanceKind::Entity(d) = &inst.0.kind {
        assert!(
            matches!(*d.status.lock().unwrap(), EntityStatus::Faulted(_)),
            "entity must be Faulted"
        );
    }

    base_ev.shutdown();
}

/// entity.rs:571 — mailbox-drain loop body in the unhandled-signal / instance-entity
/// termination path.
///
/// Three messages are sent to the entity before `r1 await` yields the main
/// coroutine: `crash`, then two `noop`s whose futures are discarded.
/// `may`'s cooperative scheduler does not preempt on `Condvar::notify_all`,
/// so the actor runs to completion — crashing on `crash` and draining both
/// `noop` envelopes — before the main coroutine resumes.  The Signal from
/// `crash` is catchable by `on:do:` at the Smol level.
#[test]
fn signal_crash_drains_queued_mailbox() {
    let src = r#"
Object subclass: #DrainSignalActor
    instanceVariableNames: ''
    classVariableNames: ''
    indexedSlots: #none.
DrainSignalActor >> crash [ Error signal: 'boom' ]
DrainSignalActor >> noop [ ^nil ]

| actor r1 |
actor := DrainSignalActor new.
r1 := actor crash.
actor noop.
actor noop.
[r1 await] on: Error do: [:e | nil].
"#;
    assert_eq!(eval(src), Value::Nil);
}

/// entity.rs:590 — mailbox-drain loop body in the `Err(e)` (non-Signal) entity
/// termination path.
///
/// A `TypeError` from `1 + false` is not wrapped in a `Signal`; it propagates
/// directly to `run_entity_loop` as `Err(EvalError::TypeError(...))`, hitting
/// the `Err(e)` arm at line 578.  Two queued `noop` messages ensure the drain
/// loop body at line 590 is reached.
#[test]
fn type_error_crash_drains_queued_mailbox() {
    use crate::entity::spawn_entity_bare;
    use crate::env::Env;
    use triple_store::store::Store;

    crate::entity::init_coroutine_stack();

    let ev = crate::eval::Evaluator::new();
    let mut env = Env::new();
    ev.registry.populate_env(&mut env);

    let prog = crate::parser::Parser::parse(
        "Object subclass: #DrainTypeErrorActor instanceVariableNames: ''.
         DrainTypeErrorActor >> badOp [ ^1 + false ]
         DrainTypeErrorActor >> noop [ ^nil ]",
    )
    .expect("parse");
    ev.eval_program(&prog.stmts, &mut env).expect("eval");

    let class = ev
        .registry
        .kernel()
        .lookup_class("DrainTypeErrorActor")
        .expect("class")
        .0
        .class
        .clone();

    let inst = spawn_entity_bare(
        class,
        ev.registry.clone(),
        env.clone(),
        None,
        Store::in_memory(),
    );

    let reply_bad = FutureHandle::new();
    let reply_a = FutureHandle::new();
    let reply_b = FutureHandle::new();

    if let InstanceKind::Entity(d) = &inst.0.kind {
        let mut mb = d.mailbox.lock().unwrap();
        mb.push_back(Envelope {
            selector: Arc::from("badOp"),
            args: vec![],
            reply: reply_bad.clone(),
            inherited_chain: vec![],
            tx_tag: None,
        });
        mb.push_back(Envelope {
            selector: Arc::from("noop"),
            args: vec![],
            reply: reply_a.clone(),
            inherited_chain: vec![],
            tx_tag: None,
        });
        mb.push_back(Envelope {
            selector: Arc::from("noop"),
            args: vec![],
            reply: reply_b.clone(),
            inherited_chain: vec![],
            tx_tag: None,
        });
        drop(mb);
        d.mailbox_ready.notify_all();
    }

    assert!(reply_bad.wait().is_err(), "badOp reply must be Err");
    assert!(
        reply_a.wait().is_err(),
        "queued reply_a must be drained with Err"
    );
    assert!(
        reply_b.wait().is_err(),
        "queued reply_b must be drained with Err"
    );

    ev.shutdown();
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

// ── C31 S2: transaction lock + shadow frames ──────────────────────────────────

#[cfg(test)]
mod tx_frames {
    use super::*;

    /// Build a bare entity `EntityData` with one declared ivar `slot` seeded to
    /// `Value::Int(seed)` in its live slots.
    fn entity_with_slot(seed: i64) -> EntityData {
        use crate::class::Class;
        use parking_lot::RwLock;
        use std::collections::HashMap;
        let cls = Arc::new(Class {
            name: Arc::from("Cell"),
            superclass: None,
            own_instance_vars: vec![Arc::from("slot")],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
            class_object: std::sync::OnceLock::new(),
        });
        let obj = Arc::new(crate::object::Object::new(cls.clone()));
        obj.set_slot(Arc::from("slot"), Value::Int(seed));
        EntityData {
            id: EntityId(1),
            data: obj,
            mailbox: Mutex::new(VecDeque::new()),
            mailbox_ready: Condvar::new(),
            status: Mutex::new(EntityStatus::Running),
            shutdown: std::sync::atomic::AtomicBool::new(false),
            store: triple_store::store::Store::in_memory(),
            tx_frames: Mutex::new(Vec::new()),
        }
    }

    /// One shared TxId reused across all of an entity's frames (tree invariant).
    fn shared_id(d: &EntityData) -> triple_store::id::TxId {
        d.store.alloc_tx_id()
    }

    #[test]
    fn fresh_entity_is_not_in_tx() {
        let d = entity_with_slot(0);
        assert!(!d.in_tx());
        assert_eq!(d.tx_depth(), 0);
        assert_eq!(d.tx_holder(), None);
    }

    #[test]
    fn push_frame_sets_depth_and_holder_but_not_live_slot() {
        let d = entity_with_slot(1);
        d.tx_push_frame(7, shared_id(&d));
        assert!(d.in_tx());
        assert_eq!(d.tx_depth(), 1);
        assert_eq!(d.tx_holder(), Some(7));
        // Live slot untouched by enrollment.
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }

    #[test]
    fn shadow_write_masks_live_slot_via_shadow_get() {
        let d = entity_with_slot(1);
        d.tx_push_frame(7, shared_id(&d));
        d.tx_shadow_set(Arc::from("slot"), Value::Int(99));
        // Read-your-writes through the shadow; live slot still the old value.
        assert_eq!(d.tx_shadow_get("slot"), Some(Value::Int(99)));
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }

    #[test]
    fn root_commit_applies_shadow_to_live_and_returns_buffer() {
        let d = entity_with_slot(1);
        d.tx_push_frame(7, shared_id(&d));
        d.tx_shadow_set(Arc::from("slot"), Value::Int(99));
        let outcome = d.tx_commit_frame(None); // root commit
        assert!(
            matches!(outcome, crate::entity::FrameCommitOutcome::RootCommitted(_)),
            "root commit hands its durable buffer up"
        );
        assert!(!d.in_tx());
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(99)));
    }

    #[test]
    fn root_abort_discards_shadow_and_leaves_live_slot() {
        let d = entity_with_slot(1);
        d.tx_push_frame(7, shared_id(&d));
        d.tx_shadow_set(Arc::from("slot"), Value::Int(99));
        d.tx_abort_frame();
        assert!(!d.in_tx());
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }

    #[test]
    fn child_commit_merges_into_parent_frame_not_live() {
        // Entity enrolled at root (ctx 1) then a child (ctx 2), one shared TxId.
        let d = entity_with_slot(1);
        let tx_id = shared_id(&d);
        d.tx_push_frame(1, tx_id);
        d.tx_shadow_set(Arc::from("slot"), Value::Int(10)); // root-level write
        d.tx_push_frame(2, tx_id);
        d.tx_shadow_set(Arc::from("slot"), Value::Int(20)); // child-level write
        let outcome = d.tx_commit_frame(Some(1)); // child commits into parent
        assert!(
            matches!(
                outcome,
                crate::entity::FrameCommitOutcome::MergedIntoAncestor
            ),
            "child commit merges into the ancestor frame"
        );
        assert_eq!(d.tx_depth(), 1, "back to the root level");
        // Child write superseded the root write within the surviving frame.
        assert_eq!(d.tx_shadow_get("slot"), Some(Value::Int(20)));
        // Still not live until the root commits.
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }

    #[test]
    fn child_abort_discards_only_child_frame() {
        let d = entity_with_slot(1);
        let tx_id = shared_id(&d);
        d.tx_push_frame(1, tx_id);
        d.tx_shadow_set(Arc::from("slot"), Value::Int(10));
        d.tx_push_frame(2, tx_id);
        d.tx_shadow_set(Arc::from("slot"), Value::Int(20));
        d.tx_abort_frame(); // child aborts
        assert_eq!(d.tx_depth(), 1);
        // Root-level write survives; child write gone.
        assert_eq!(d.tx_shadow_get("slot"), Some(Value::Int(10)));
    }

    #[test]
    fn child_commit_passes_lock_up_when_parent_never_enrolled() {
        // Entity first touched during the child (ctx 2); root (ctx 1) never
        // separately enrolled it, so depth is 1.
        let d = entity_with_slot(1);
        d.tx_push_frame(2, shared_id(&d));
        d.tx_shadow_set(Arc::from("slot"), Value::Int(20));
        let outcome = d.tx_commit_frame(Some(1)); // lock passes up to parent
        assert!(
            matches!(outcome, crate::entity::FrameCommitOutcome::PassedUpTo(1)),
            "lock pass-up retags the frame to the parent"
        );
        assert_eq!(d.tx_depth(), 1, "still locked, now under the parent");
        assert_eq!(d.tx_holder(), Some(1));
        // Speculative write retained at the parent level, not yet live.
        assert_eq!(d.tx_shadow_get("slot"), Some(Value::Int(20)));
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }
}

// ── C31 S3: ancestry-based mailbox admission ──────────────────────────────────

#[cfg(test)]
mod tx_admission {
    use super::*;
    use crate::shared_tx::{ChildTxInner, RootTxInner, TxContext};
    use std::collections::VecDeque;
    use triple_store::id::TxId;

    fn root_ctx(ctx_id: u64) -> Arc<TxContext> {
        Arc::new(TxContext::Root(Arc::new(RootTxInner {
            ctx_id,
            tx_id: TxId(1),
            tx: Mutex::new(None),
            enrolled: Mutex::new(Vec::new()),
        })))
    }

    fn child_ctx(ctx_id: u64, outer: Arc<TxContext>) -> Arc<TxContext> {
        Arc::new(TxContext::Child(Arc::new(ChildTxInner {
            ctx_id,
            outer,
            tx: Mutex::new(None),
            participants: Mutex::new(Vec::new()),
        })))
    }

    fn envelope(tag: Option<Arc<TxContext>>) -> Envelope {
        Envelope {
            selector: Arc::from("ping"),
            args: vec![],
            reply: FutureHandle::new(),
            inherited_chain: vec![],
            tx_tag: tag,
        }
    }

    fn queue(tags: Vec<Option<Arc<TxContext>>>) -> VecDeque<Envelope> {
        tags.into_iter().map(envelope).collect()
    }

    #[test]
    fn not_in_tx_admits_front_fifo_regardless_of_tag() {
        let mb = queue(vec![None, None]);
        assert_eq!(crate::entity::admissible_index(&mb, None), Some(0));
        let mb = queue(vec![Some(root_ctx(1)), None]);
        assert_eq!(crate::entity::admissible_index(&mb, None), Some(0));
    }

    #[test]
    fn not_in_tx_empty_mailbox_admits_nothing() {
        let mb: VecDeque<Envelope> = VecDeque::new();
        assert_eq!(crate::entity::admissible_index(&mb, None), None);
    }

    #[test]
    fn locked_parks_untagged_and_admits_holder_tagged() {
        let root = root_ctx(1);
        // Untagged in front parks; the holder-tagged one behind is admitted.
        let mb = queue(vec![None, Some(root.clone())]);
        assert_eq!(crate::entity::admissible_index(&mb, Some(1)), Some(1));
    }

    #[test]
    fn root_holder_admits_descendant_child() {
        let root = root_ctx(1);
        let child = child_ctx(2, root.clone());
        // Held at root; a child-tagged message is a descendant → admitted.
        let mb = queue(vec![Some(child)]);
        assert_eq!(crate::entity::admissible_index(&mb, Some(1)), Some(0));
    }

    #[test]
    fn child_holder_parks_sibling() {
        let root = root_ctx(1);
        let child_a = child_ctx(2, root.clone());
        let sibling_b = child_ctx(3, root.clone());
        // Held at child A (ctx 2); sibling B (ctx 3) is neither A nor a descendant
        // of A → parks. A same-context message is admitted.
        let mb = queue(vec![Some(sibling_b), Some(child_a)]);
        assert_eq!(crate::entity::admissible_index(&mb, Some(2)), Some(1));
    }

    #[test]
    fn child_holder_parks_everything_when_only_siblings_present() {
        let root = root_ctx(1);
        let sibling_b = child_ctx(3, root.clone());
        let mb = queue(vec![Some(sibling_b), None]);
        assert_eq!(crate::entity::admissible_index(&mb, Some(2)), None);
    }

    #[test]
    fn ancestor_tagged_message_parks_while_descendant_holds() {
        // Held at child (ctx 2); a message tagged with the root (ctx 1, an
        // ancestor) parks — the child must exit first.
        let root = root_ctx(1);
        let _child = child_ctx(2, root.clone());
        let mb = queue(vec![Some(root)]);
        assert_eq!(crate::entity::admissible_index(&mb, Some(2)), None);
    }
}

// ── C31 S4: transaction primitives ────────────────────────────────────────────

#[cfg(test)]
mod tx_primitives {
    use super::*;
    use crate::builtins_iterative::dispatch_object_primitive;
    use crate::eval::Evaluator;
    use crate::shared_tx::TxContext;

    /// Build a standalone entity handle (no coroutine) with one ivar `slot`,
    /// sharing `ev`'s store so committed facts are visible through `ev.store`.
    fn cell_entity(ev: &Evaluator, seed: i64) -> InstanceHandle {
        use crate::class::Class;
        use parking_lot::RwLock;
        use std::collections::HashMap;
        let cls = Arc::new(Class {
            name: Arc::from("Cell"),
            superclass: None,
            own_instance_vars: vec![Arc::from("slot")],
            own_class_vars: vec![],
            class_var_storage: RwLock::new(HashMap::new()),
            indexed_slot_kind: None,
            methods: RwLock::new(HashMap::new()),
            class_methods: RwLock::new(HashMap::new()),
            is_value: false,
            class_object: std::sync::OnceLock::new(),
        });
        let data = crate::entity::new_entity_data(&cls, ev.store.clone_handle());
        data.data.set_slot(Arc::from("slot"), Value::Int(seed));
        InstanceHandle(Arc::new(InstanceState {
            class: cls,
            kind: InstanceKind::Entity(Box::new(data)),
        }))
    }

    fn call(
        ev: &Evaluator,
        recv: &InstanceHandle,
        sel: &str,
        args: Vec<Value>,
    ) -> Result<Value, EvalError> {
        let mut env = crate::env::Env::new();
        dispatch_object_primitive(ev, recv.clone(), &Arc::from(sel), args, &mut env)
    }

    fn entity_data(h: &InstanceHandle) -> &crate::entity::EntityData {
        match &h.0.kind {
            InstanceKind::Entity(d) => d,
            _ => unreachable!(),
        }
    }

    #[test]
    fn begin_creates_root_and_sets_ctx() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let h = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        let ctx = match h {
            Value::Handle(h) => h.downcast::<TxContext>().unwrap(),
            _ => panic!("expected a handle"),
        };
        assert!(matches!(&*ctx, TxContext::Root(_)));
        assert!(ev.current_tx_ctx.borrow().is_some());
        assert_eq!(
            ev.current_tx_ctx.borrow().as_ref().unwrap().ctx_id(),
            ctx.ctx_id()
        );
    }

    #[test]
    fn nested_begin_creates_child_of_root() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        let root_ctx = match &root {
            Value::Handle(h) => h.clone().downcast::<TxContext>().unwrap(),
            _ => unreachable!(),
        };
        let child = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        let child_ctx = match child {
            Value::Handle(h) => h.downcast::<TxContext>().unwrap(),
            _ => unreachable!(),
        };
        match &*child_ctx {
            TxContext::Child(c) => assert_eq!(c.outer.ctx_id(), root_ctx.ctx_id()),
            _ => panic!("expected a child"),
        }
        // Child shares the root's store TxId (one atomic commit for the tree).
        assert_eq!(child_ctx.tx_id(), root_ctx.tx_id());
        // Active context is now the child.
        assert_eq!(
            ev.current_tx_ctx.borrow().as_ref().unwrap().ctx_id(),
            child_ctx.ctx_id()
        );
    }

    #[test]
    fn commit_root_clears_ctx() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        call(&ev, &coord, "_primCommitSharedTx:", vec![root]).unwrap();
        assert!(ev.current_tx_ctx.borrow().is_none());
    }

    #[test]
    fn child_commit_restores_parent_as_active() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        let root_id = match &root {
            Value::Handle(h) => h.clone().downcast::<TxContext>().unwrap().ctx_id(),
            _ => unreachable!(),
        };
        let child = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        call(&ev, &coord, "_primCommitSharedTx:", vec![child]).unwrap();
        assert_eq!(
            ev.current_tx_ctx.borrow().as_ref().unwrap().ctx_id(),
            root_id
        );
    }

    #[test]
    fn rollback_root_clears_ctx() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        call(&ev, &coord, "_primRollbackSharedTx:", vec![root]).unwrap();
        assert!(ev.current_tx_ctx.borrow().is_none());
    }

    #[test]
    fn enrolled_participants_returns_array_of_entities() {
        let ev = Evaluator::new(); // build_array needs the kernel Array class
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        let ctx = match &root {
            Value::Handle(h) => h.clone().downcast::<TxContext>().unwrap(),
            _ => unreachable!(),
        };
        // Simulate two participants enrolled at the root level.
        let p1 = cell_entity(&ev, 1);
        let p2 = cell_entity(&ev, 2);
        ctx.participants().lock().unwrap().push(p1.clone());
        ctx.participants().lock().unwrap().push(p2.clone());
        let arr = call(&ev, &coord, "_primEnrolledParticipants:", vec![root]).unwrap();
        match arr {
            Value::Instance(h) => {
                if let InstanceKind::Entity(d) = &h.0.kind
                    && let Some(crate::object::IndexedStorage::Pointer(slots)) = &d.data.indexed
                {
                    assert_eq!(slots.lock().len(), 2);
                } else {
                    panic!("expected an Array entity");
                }
            }
            _ => panic!("expected an Array"),
        }
    }

    #[test]
    fn exit_commit_applies_shadow_to_live_and_unlocks() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        let ctx = match &root {
            Value::Handle(h) => h.clone().downcast::<TxContext>().unwrap(),
            _ => unreachable!(),
        };
        // Enroll a participant at root and stage a speculative write.
        let p = cell_entity(&ev, 1);
        let pd = entity_data(&p);
        pd.tx_push_frame(ctx.ctx_id(), ctx.tx_id());
        pd.tx_shadow_set(Arc::from("slot"), Value::Int(42));
        assert_eq!(pd.data.get_slot("slot"), Some(Value::Int(1))); // live untouched

        // The coordinator's pvtExitTx arrives tagged with the context.
        *ev.current_tx_ctx.borrow_mut() = Some(ctx.clone());
        call(
            &ev,
            &p,
            "_primExitTxMode:",
            vec![Value::Symbol(Arc::from("commit"))],
        )
        .unwrap();

        assert!(!pd.in_tx(), "participant unlocks after exit");
        assert_eq!(
            pd.data.get_slot("slot"),
            Some(Value::Int(42)),
            "shadow applied to live"
        );
    }

    #[test]
    fn exit_rollback_discards_shadow_and_leaves_live() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginSharedTx", vec![]).unwrap();
        let ctx = match &root {
            Value::Handle(h) => h.clone().downcast::<TxContext>().unwrap(),
            _ => unreachable!(),
        };
        let p = cell_entity(&ev, 1);
        let pd = entity_data(&p);
        pd.tx_push_frame(ctx.ctx_id(), ctx.tx_id());
        pd.tx_shadow_set(Arc::from("slot"), Value::Int(42));

        *ev.current_tx_ctx.borrow_mut() = Some(ctx.clone());
        call(
            &ev,
            &p,
            "_primExitTxMode:",
            vec![Value::Symbol(Arc::from("rollback"))],
        )
        .unwrap();

        assert!(!pd.in_tx());
        assert_eq!(
            pd.data.get_slot("slot"),
            Some(Value::Int(1)),
            "live unchanged on rollback"
        );
    }
}
