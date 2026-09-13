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

#[cfg(debug_assertions)]
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
            version: std::sync::atomic::AtomicU64::new(0),
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
        version: std::sync::atomic::AtomicU64::new(0),
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
        snapshot: Arc::from(Vec::new()),
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

// ── C31 S5: Transaction kernel class — end-to-end ─────────────────────────────

/// Run `src` through the production coroutine path (kernel loaded on the test
/// thread, program evaluated inside a `may` coroutine via `with_registry`), as
/// `entity_stack_depth_probe` does. Required for `Transaction atomic:`, which
/// spawns participant coroutines and blocks the coordinator on their replies.
#[cfg(test)]
fn run_in_coroutine(src: &str) -> Value {
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
    rx.recv().unwrap()
}

#[test]
fn tcp_server_serves_one_request_roundtrip() {
    // C-web spike end-to-end: a Smol server binds a listener, accepts one
    // connection, reads the request, writes a response containing "Hello", and
    // closes. A plain std::net client (this thread) drives it. Exercises the full
    // async socket path: _primTcpListen:/Accept:/Read:/Write:/Close: + Future
    // chaining resolved from I/O coroutines.
    use std::io::{Read as _, Write as _};

    crate::entity::init_coroutine_stack();
    let base_ev = crate::eval::Evaluator::new();
    let registry = base_ev.registry.clone();
    // Port chosen to be uncommon; the client retry-connects to absorb the bind race.
    let port = 47653u16;
    let src = format!(
        r#"
        | listener sock req |
        listener := TcpListener onPort: {port}.
        sock := listener accept await.
        req := sock read await.
        (sock write: 'HTTP/1.1 200 OK Hello') await.
        sock close.
        'served'
        "#
    );
    let prog = crate::parser::Parser::parse(&src).expect("parse");
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    may::go!(move || {
        let ev = crate::eval::Evaluator::with_registry(registry);
        let mut env = crate::env::Env::new();
        ev.registry.populate_env(&mut env);
        let v = ev.eval_program(&prog.stmts, &mut env).expect("server eval");
        tx.send(v).unwrap();
    });

    // Client: retry-connect to absorb the server's bind latency.
    let addr = format!("127.0.0.1:{port}");
    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = std::net::TcpStream::connect(&addr) {
            stream = Some(s);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut stream = stream.expect("client could not connect to Smol server");
    stream.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap(); // reads until server closes (EOF)
    assert!(resp.contains("Hello"), "unexpected response: {resp:?}");

    let served = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(served, str_val("served"));
}

#[test]
fn hc_within_entity_method_runs() {
    // Probe: does HandlerContext run: work when invoked from inside an entity's
    // own method (current_entity_id = that entity), vs. top-level eval?
    let src = r#"
        Object subclass: #Coord instanceVariableNames: ''.
        Coord >> tryHC [
            | hc |
            hc := HandlerContext new.
            hc on: Error do: [:e | e resume: 7].
            ^hc run: [ 42 ]
        ]
        | c |
        c := Coord new.
        c tryHC await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(42));
}

#[test]
fn hc_then_signal_resolves_future_within_entity() {
    // Isolates the Transaction machinery minus pvtCommit/_primBeginAtomicTx:
    // an HC inside an entity whose run: body registers a then: that signals
    // OkToCommit; the handler resolves a future. Returns the future's value.
    let src = r#"
        Object subclass: #Coord2 instanceVariableNames: ''.
        Coord2 >> tryTx [
            | txf |
            txf := Future new.
            ((HandlerContext new
                on: OkToCommit do: [:c | txf resolveWith: c value. c resume: nil])
                on: Error do: [:e | txf rejectWith: e. e return: nil])
              run: [ 42 then: [:v | (OkToCommit value: 99) signal ] orElse: #reject ].
            ^txf
        ]
        | c |
        c := Coord2 new.
        "c tryTx yields an implicit future resolving to the Future txf; awaiting
         once unwraps the entity send, awaiting again resolves the Future itself."
        c tryTx await await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(99));
}

#[test]
fn handler_context_catches_async_future_rejection() {
    // Errors flow through ONE channel: the condition system. A rejected Future
    // surfaced by `await` must be catchable by a HandlerContext `Error` handler —
    // the handler is live at the await site (that is HandlerContext's purpose).
    // Before Stage 1 the rejection surfaced as a raw error that unwound past the
    // HandlerContext (bypassing signal_condition), so the handler never fired.
    let src = r#"
        | f |
        f := Future new.
        f rejectWith: (Error withMessage: 'boom').
        (HandlerContext new on: Error do: [:e | e return: ('caught: ' , e message)])
            run: [ f await. 'not caught' ]
    "#;
    assert_eq!(
        run_in_coroutine(src),
        Value::Str(std::sync::Arc::from("caught: boom"))
    );
}

// ── Unified handler lookup: on:do: and HandlerContext share one ordered chain ──

fn str_val(s: &str) -> Value {
    Value::Str(std::sync::Arc::from(s))
}

#[test]
fn inner_on_do_shadows_outer_handler_context() {
    // Unification (guide §3/§4): a signal must reach the *dynamically innermost*
    // handler across BOTH forms. Here the inner `on:do:` is nested inside an outer
    // HandlerContext; the inner one must win. Before unification, `dispatch_signal`
    // only walked `handler_chain` (HC handlers), so the outer HC wrongly caught it.
    let src = r#"
        (HandlerContext new on: Error do: [:e | 'outer-HC'])
            run: [ [ (Error withMessage: 'boom') signal. 'unreached' ]
                     on: Error do: [:e | 'inner-onDo'] ]
    "#;
    assert_eq!(run_in_coroutine(src), str_val("inner-onDo"));
}

#[test]
fn inner_handler_context_shadows_outer_on_do() {
    // The mirror: an inner HandlerContext nested inside an outer `on:do:`. The
    // inner HC must win — one ordered chain, innermost first, regardless of form.
    let src = r#"
        [ (HandlerContext new on: Error do: [:e | e return: 'inner-HC'])
            run: [ (Error withMessage: 'boom') signal. 'unreached' ] ]
          on: Error do: [:e | 'outer-onDo']
    "#;
    assert_eq!(run_in_coroutine(src), str_val("inner-HC"));
}

#[test]
fn on_do_forces_its_body_future() {
    // on:do:'s distinguishing property: it forces its body synchronously. The body
    // makes an async entity send (`b boom`) and does NOT await it — its value is
    // the raw reply future, which rejects. on:do:'s sync forcing awaits it, so the
    // rejection surfaces inside the on:do: and is caught. Without body-forcing the
    // on:do: would yield the pending future instead of catching. Proves the
    // forcing survived the unification into the shared handler chain.
    let src = r#"
        Object subclass: #Boom instanceVariableNames: 'x'.
        Boom >> boom [ self error: 'kaboom' ]

        | b |
        b := Boom new.
        [ b boom ] on: Error do: [:e | 'caught-forced: ' , e message]
    "#;
    assert_eq!(run_in_coroutine(src), str_val("caught-forced: kaboom"));
}

#[test]
fn on_do_disposition_is_by_condition_kind() {
    // Unified disposition (§2): a handler's normal return resumes when its declared
    // class is resumable and aborts when non-resumable — for `on:do:` too, not just
    // HandlerContext. Here the handler class matches the condition kind: a `Warning`
    // handler (resumable) resumes past the signal; an `Error` handler (non-resumable)
    // unwinds to the on:do:, yielding the handler's value. (See
    // `disposition_keys_on_handler_class_not_condition_class` for the distinction.)
    let resumes = r#"
        [ (Warning withMessage: 'w') signal. 'after-signal' ]
          on: Warning do: [:e | 'handled']
    "#;
    assert_eq!(run_in_coroutine(resumes), str_val("after-signal"));

    let aborts = r#"
        [ (Error withMessage: 'e') signal. 'after-signal' ]
          on: Error do: [:e | 'handled']
    "#;
    assert_eq!(run_in_coroutine(aborts), str_val("handled"));
}

#[test]
fn disposition_keys_on_handler_class_not_condition_class() {
    // Resumability is determined by the class named at the handler site, NOT the
    // concrete class of the signalled condition. A handler declared for `Error`
    // aborts even when the actual condition is a resumable `Error` subclass — so
    // handler authors get a static, predictable contract.
    let src = r#"
        Error subclass: #ResumableError instanceVariableNames: ''.
        ResumableError class >> isResumable [ ^true ].
        [ (ResumableError withMessage: 'e') signal. 'after-signal' ]
          on: Error do: [:e | 'handled']
    "#;
    // Handler declared `Error` (non-resumable) → aborts, yields the handler value.
    assert_eq!(run_in_coroutine(src), str_val("handled"));
}

#[test]
fn disposition_resumes_when_handler_declares_resumable_class() {
    // The flip side: when the handler is declared for the resumable subclass
    // itself, its normal completion resumes past the signal.
    let src = r#"
        Error subclass: #ResumableError instanceVariableNames: ''.
        ResumableError class >> isResumable [ ^true ].
        [ (ResumableError withMessage: 'e') signal. 'after-signal' ]
          on: ResumableError do: [:e | 'handled']
    "#;
    // Handler declared `ResumableError` (resumable) → resumes, block continues.
    assert_eq!(run_in_coroutine(src), str_val("after-signal"));
}

#[test]
fn atomic_no_participants_commits_and_resolves_result() {
    // Minimal: no participant work. Exercises the HandlerContext + then: +
    // OkToCommit + resume: machinery in isolation.
    let src = r#"
        | tr |
        tr := (Transaction atomic: [:tx | 42]) await.
        tr result await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(42));
}

#[test]
fn atomic_transfer_commits_on_both_participants() {
    // Two accounts; an atomic block moves 30 from a to b. After commit both live
    // balances reflect the transfer.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]
        Account >> withdraw: n [ balance := balance - n ]

        | a b tr |
        a := Account new.
        b := Account new.
        (a deposit: 100) await.
        tr := (Transaction atomic: [:tx |
            (a withdraw: 30) await.
            (b deposit: 30) await ]) await.
        tr result await.
        (a balance await) + ((b balance await) * 1000)
    "#;
    // a = 70, b = 30  →  70 + 30*1000 = 30070
    assert_eq!(run_in_coroutine(src), Value::Int(30070));
}

#[test]
fn atomic_abort_rolls_back_writes() {
    // The block writes then aborts; live balances must remain pre-transaction.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a tr |
        a := Account new.
        (a deposit: 100) await.
        tr := (Transaction atomic: [:tx |
            (a deposit: 50) await.
            tx abort ]) await.
        "result rejects on abort; swallow it, then check the rollback took effect"
        [ tr result await ] on: Error do: [:e | nil].
        a balance await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(100));
}

#[test]
fn atomic_shadow_read_returns_speculative_value_mid_block() {
    // Shadow correctness: a mid-block read after a write returns the shadow
    // (speculative) value, not the stale live slot. The atomic block's value IS
    // the mid-block read, and `result` resolves to it. Pre-tx slot = 100; after
    // `deposit: 50` the in-tx read returns 150 (the live slot stays 100 until
    // commit). A broken shadow would surface the live slot → 100, so 150
    // distinguishes read-your-writes from a stale-slot read.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a tr |
        a := Account new.
        (a deposit: 100) await.
        tr := (Transaction atomic: [:tx |
            (a deposit: 50) await.
            a balance await ]) await.
        tr result await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(150));
}

#[test]
fn stm_no_participants_commits_and_resolves_result() {
    // Minimal #stm: no participant work. Exercises pvtRunStm: → pvtStmAttempt: →
    // pvtStmCommit (empty participant list) → resolve.
    let src = r#"
        | tr |
        tr := (Transaction stm: [:tx | 42]) await.
        tr result await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(42));
}

#[test]
fn stm_transfer_commits_on_both_participants() {
    // Uncontended #stm: a transfer of 30 across two participants commits via the
    // two-phase protocol (prepare/validate both, then apply). No concurrent
    // mutation, so versions validate and the first attempt succeeds.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]
        Account >> withdraw: n [ balance := balance - n ]

        | a b tr |
        a := Account new.
        b := Account new.
        (a deposit: 100) await.
        tr := (Transaction stm: [:tx |
            (a withdraw: 30) await.
            (b deposit: 30) await ]) await.
        tr result await.
        (a balance await) + ((b balance await) * 1000)
    "#;
    // a = 70, b = 30  →  70 + 30*1000 = 30070
    assert_eq!(run_in_coroutine(src), Value::Int(30070));
}

#[test]
fn entities_run_concurrently_cross_gate() {
    // Concurrency canary (C32 B): two entities cross-wait on gates — each resolves
    // its own gate and awaits the other's. This completes ONLY if their handlers
    // run simultaneously (separate coroutines). A serialized executor — one
    // handler must finish before the other starts — would deadlock here (the first
    // awaits a gate the second never gets to resolve). It guards against ever
    // accidentally making entity dispatch serial.
    let src = r#"
        Object subclass: #Crosser instanceVariableNames: ''.
        Crosser >> enter: myGate thenAwait: otherGate [
            myGate resolveWith: true.
            otherGate await.
            ^true ]

        | g1 g2 c1 c2 f1 f2 |
        g1 := Future new.
        g2 := Future new.
        c1 := Crosser new.
        c2 := Crosser new.
        "Both sends are in flight (not awaited), so the handlers run concurrently."
        f1 := c1 enter: g1 thenAwait: g2.
        f2 := c2 enter: g2 thenAwait: g1.
        (f1 await) and: [ f2 await ]
    "#;
    assert_eq!(run_in_coroutine(src), Value::Bool(true));
}

#[test]
fn atomic_participants_run_concurrently() {
    // Concurrency WITHIN an #atomic transaction: the block fans out to two locked
    // participants that cross-wait on gates (each resolves its own, awaits the
    // other's). It completes only if the participants' handlers run simultaneously
    // — i.e. the transaction does not serialize its participants. The block sends
    // both without awaiting, then awaits both; a serial executor would deadlock.
    let src = r#"
        Object subclass: #Crosser instanceVariableNames: ''.
        Crosser >> enter: myGate thenAwait: otherGate [
            myGate resolveWith: true.
            otherGate await.
            ^true ]

        | g1 g2 a b tr |
        g1 := Future new.
        g2 := Future new.
        a := Crosser new.
        b := Crosser new.
        tr := (Transaction atomic: [:tx |
            | fa fb |
            fa := a enter: g1 thenAwait: g2.
            fb := b enter: g2 thenAwait: g1.
            (fa await) and: [ fb await ] ]) await.
        tr result await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Bool(true));
}

#[test]
fn stm_participants_run_concurrently() {
    // Concurrency WITHIN an #stm transaction: same cross-gate, non-locking
    // participants. Completes only if they run simultaneously.
    let src = r#"
        Object subclass: #Crosser instanceVariableNames: ''.
        Crosser >> enter: myGate thenAwait: otherGate [
            myGate resolveWith: true.
            otherGate await.
            ^true ]

        | g1 g2 a b tr |
        g1 := Future new.
        g2 := Future new.
        a := Crosser new.
        b := Crosser new.
        tr := (Transaction stm: [:tx |
            | fa fb |
            fa := a enter: g1 thenAwait: g2.
            fb := b enter: g2 thenAwait: g1.
            (fa await) and: [ fb await ] ]) await.
        tr result await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Bool(true));
}

#[test]
fn stm_abort_rolls_back_writes() {
    // The block writes then aborts (a raw Error); #stm does not retry errors — it
    // rolls back and rejects. Live balance must remain pre-transaction.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a tr |
        a := Account new.
        (a deposit: 100) await.
        tr := (Transaction stm: [:tx |
            (a deposit: 50) await.
            tx abort ]) await.
        [ tr result await ] on: Error do: [:e | nil].
        a balance await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(100));
}

#[test]
fn stm_conflict_from_external_write_is_detected() {
    // Ctx-aware routing (C32 B): an external (non-tx) write to an STM-enrolled
    // entity must hit the LIVE slot and bump its version, invalidating the STM
    // tx's optimistic snapshot. With retries disabled the conflict surfaces as
    // TxConflict. Determinism: `readDone` fires after the STM's snapshot read, the
    // test then does the external write, then releases `go`. On the buggy
    // (pre-ctx-aware) routing the external write lands in the STM shadow — no bump,
    // no conflict — and this returns false.
    let src = r#"
        Object subclass: #Cell instanceVariableNames: 'val'.
        Cell >> initialize [ val := 0 ]
        Cell >> val        [ ^val ]
        Cell >> setVal: v  [ val := v ]

        Transaction subclass: #NoRetryTx.
        NoRetryTx class >> stmRetryLimit [ ^0 ]

        | c readDone go trf |
        c := Cell new.
        readDone := Future new.
        go := Future new.
        trf := NoRetryTx stm: [:tx |
            c val await.
            readDone resolveWith: true.
            go await.
            (c setVal: 99) await ].
        readDone await.
        (c setVal: 7) await.
        go resolveWith: true.
        [ trf result await. false ] on: TxConflict do: [:e | true ]
    "#;
    assert_eq!(run_in_coroutine(src), Value::Bool(true));
}

#[test]
fn stm_conflict_retries_and_succeeds() {
    // Full optimistic loop: read c, an external write bumps c's version mid-tx →
    // first commit conflicts → retry re-reads the new live value and commits. The
    // final value (107) proves the retry re-ran with a fresh snapshot; without
    // retry (or without ctx-aware routing) it would be 100 (stale read) or the
    // external write would be lost. `readDone`'s resolve is wrapped so the retry's
    // re-run doesn't fail on the already-settled future.
    let src = r#"
        Object subclass: #Cell instanceVariableNames: 'val'.
        Cell >> initialize [ val := 0 ]
        Cell >> val        [ ^val ]
        Cell >> setVal: v  [ val := v ]

        | c readDone go trf |
        c := Cell new.
        readDone := Future new.
        go := Future new.
        trf := Transaction stm: [:tx |
            | cur |
            cur := c val await.
            [ readDone resolveWith: true ] on: Error do: [:e | nil].
            go await.
            (c setVal: cur + 100) await ].
        readDone await.
        (c setVal: 7) await.
        go resolveWith: true.
        trf result await.
        c val await
    "#;
    // attempt 1: cur=0, external sets live 7 (bump) → conflict → retry;
    // retry: cur=7 → 107, no further external write → commits. Final 107.
    assert_eq!(run_in_coroutine(src), Value::Int(107));
}

#[test]
fn stm_conflicts_with_interleaved_atomic_tx() {
    // Two independent trees on one entity (C32 S1 ancestry-aware merge, end-to-end):
    // an #stm tx S enrolls e (non-locking), then an unrelated #atomic tx A commits
    // a write to e. On e the frames are [Stm(S), Atomic(A)]; A's root commit
    // applies to LIVE and bumps e's version (it must NOT merge into S's shadow),
    // so S conflicts and retries against the value A committed. Final 107 (vs 100
    // if A's commit had wrongly landed in S's shadow / not bumped the version).
    let src = r#"
        Object subclass: #Cell instanceVariableNames: 'val'.
        Cell >> initialize [ val := 0 ]
        Cell >> val        [ ^val ]
        Cell >> setVal: v  [ val := v ]

        | e readDone go trS |
        e := Cell new.
        readDone := Future new.
        go := Future new.
        trS := Transaction stm: [:tx |
            | cur |
            cur := e val await.
            [ readDone resolveWith: true ] on: Error do: [:x | nil].
            go await.
            (e setVal: cur + 100) await ].
        readDone await.
        (Transaction atomic: [:tA | (e setVal: 7) await ]) await result await.
        go resolveWith: true.
        trS result await.
        e val await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(107));
}

#[test]
fn stm_returns_result_before_block_work_completes() {
    // Non-blocking (C32 point 8): `stm:` yields its TxResult immediately via the
    // `then:` continuation. The block does its write, then *returns* a still-pending
    // future `g` as its work future — the coordinator chains it with `then:` rather
    // than awaiting it, so it returns the TxResult before `g` resolves and the
    // commit is deferred until it does. We obtain the TxResult and read the still
    // uncommitted live balance, then release `g` to let the commit finish. A
    // blocking (`await`-based) coordinator would park on `g` and never yield the
    // TxResult, deadlocking here.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a g tr seen |
        a := Account new.
        (a deposit: 100) await.
        g := Future new.
        tr := Transaction stm: [:tx | (a deposit: 5) await. g ].
        "TxResult available before g resolves; the write is still shadowed (live 100)."
        seen := a balance await.
        g resolveWith: true.
        tr result await.
        (seen * 1000) + (a balance await)
    "#;
    // seen = 100 (uncommitted while g pending); after commit, balance = 105 → 100105
    assert_eq!(run_in_coroutine(src), Value::Int(100105));
}

#[test]
fn atomic_mailbox_lock_defers_external_message_until_commit() {
    // Mailbox lock: an external (untagged) message issued while a participant is
    // locked by the transaction is delivered only after commit. Determinism is
    // forced with two gates: `g1` confirms the tx has locked `a` before the
    // external read is enqueued; `g2` keeps the tx from committing until that
    // untagged read is already in `a`'s mailbox. So the read is provably enqueued
    // *during* the lock. The tx writes again (deposit: 25) AFTER the gate, so the
    // committed balance (175) differs from the mid-tx shadow (150). With the lock
    // honoured the read parks and is admitted only after commit, observing 175. A
    // broken lock would admit it mid-tx and observe the speculative shadow (150),
    // so 175 specifically proves the read was deferred until commit.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a g1 g2 trf seen |
        a := Account new.
        (a deposit: 100) await.
        g1 := Future new.
        g2 := Future new.
        trf := Transaction atomic: [:tx |
            (a deposit: 50) await.
            g1 resolveWith: true.
            g2 await.
            (a deposit: 25) await ].
        g1 await.
        seen := a balance.
        g2 resolveWith: true.
        (trf await) result await.
        seen await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(175));
}

#[test]
fn atomic_transfer_commits_both_participants_under_one_tx_id() {
    // Shared TxId: a transfer touching two participants commits both entities'
    // facts under a single TxId. We subscribe (System ctx, wildcard pattern) to
    // capture every committed change, then assert there is a TxId whose `balance`
    // changes span both distinct participant subjects — i.e. the shared-tx commit
    // wrote both under one id, not two per-entity ids.
    use triple_store::exec_context::ExecContext;
    use triple_store::pattern::{Match, MatchAsserter, Pattern};

    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]
        Account >> withdraw: n [ balance := balance - n ]

        | a b tr |
        a := Account new.
        b := Account new.
        (a deposit: 100) await.
        tr := (Transaction atomic: [:tx |
            (a withdraw: 30) await.
            (b deposit: 30) await ]) await.
        tr result await
    "#;

    crate::entity::init_coroutine_stack();
    let base_ev = crate::eval::Evaluator::new();
    let registry = base_ev.registry.clone();
    let store = registry.store.clone_handle();
    let sub = store.subscribe(
        ExecContext::System,
        Pattern {
            subject: Match::Any,
            predicate: Match::Any,
            object: Match::Any,
            asserter: MatchAsserter::Any,
        },
    );
    let prog = crate::parser::Parser::parse(src).expect("parse");
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    may::go!(move || {
        let ev = crate::eval::Evaluator::with_registry(registry);
        let mut env = crate::env::Env::new();
        ev.registry.populate_env(&mut env);
        let v = ev.eval_program(&prog.stmts, &mut env).expect("eval");
        tx.send(v).unwrap();
    });
    rx.recv().unwrap();

    // Drain committed changes; group `balance` subjects by TxId.
    use std::collections::HashMap;
    let mut by_tx: HashMap<
        triple_store::id::TxId,
        std::collections::HashSet<triple_store::id::EntityId>,
    > = HashMap::new();
    while let Ok(Some(ev)) = sub.try_recv() {
        if store.resolve(ev.predicate).as_deref() != Some("balance") {
            continue;
        }
        by_tx.entry(ev.tx_id).or_default().insert(ev.subject);
    }

    let shared = by_tx.values().any(|subjects| subjects.len() >= 2);
    assert!(
        shared,
        "expected one TxId to cover both participants' balance facts; got {by_tx:?}"
    );
}

// S5b (C31): nested `atomic:`. The inner `Transaction atomic:` send is tagged
// with the outer tx context. Fixed by excluding transaction coordinators (any
// entity that is-a `Transaction`) from tagged auto-enrollment in
// `run_entity_loop`; tag propagation still occurs so the inner coordinator builds
// a Child of the outer tx.
#[test]
fn atomic_nested_inner_commit_absorbs_into_outer() {
    // S5b: an inner `atomic:` runs as a Child of the outer; on inner commit its
    // buffer absorbs into the outer, and the outer commit writes both. a ends at
    // 100 + 10 (outer) + 5 (inner) = 115.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a tr |
        a := Account new.
        (a deposit: 100) await.
        tr := (Transaction atomic: [:tx |
            (a deposit: 10) await.
            ((Transaction atomic: [:tx2 | (a deposit: 5) await ]) await result await) ]) await.
        tr result await.
        a balance await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(115));
}

#[test]
fn atomic_nested_inner_rollback_keeps_outer_write() {
    // S5b: an inner `atomic:` aborts; its Child buffer is discarded while the
    // outer transaction still commits its own write. The outer block catches the
    // inner rejection and continues. a ends at 100 + 10 (outer); the inner +5 is
    // rolled back.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a tr |
        a := Account new.
        (a deposit: 100) await.
        tr := (Transaction atomic: [:tx |
            (a deposit: 10) await.
            [ (Transaction atomic: [:tx2 | (a deposit: 5) await. tx2 abort ]) await result await ]
                on: Error do: [:e | nil] ]) await.
        tr result await.
        a balance await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(110));
}

#[test]
fn stm_outer_with_nested_atomic_commits_both() {
    // Mixed-mode nesting (C32 B): an #stm outer whose block runs a nested #atomic
    // child on the same entity. The atomic child is a Child of the stm root; its
    // commit absorbs into the outer, and the outer's optimistic commit (no
    // conflict) applies both. a = 100 + 10 (outer stm) + 5 (inner atomic) = 115.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a tr |
        a := Account new.
        (a deposit: 100) await.
        tr := (Transaction stm: [:tx |
            (a deposit: 10) await.
            ((Transaction atomic: [:tx2 | (a deposit: 5) await ]) await result await) ]) await.
        tr result await.
        a balance await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(115));
}

#[test]
fn atomic_outer_with_nested_stm_is_coerced_and_commits() {
    // Contagious-atomic end-to-end (C32 B): an #stm nested inside an #atomic outer
    // is coerced to atomic (its parent is an AtomicTx), so it behaves as a nested
    // atomic child — its writes absorb into the outer and commit with it (no
    // optimistic/retry path). a = 100 + 10 + 5 = 115.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a tr |
        a := Account new.
        (a deposit: 100) await.
        tr := (Transaction atomic: [:tx |
            (a deposit: 10) await.
            ((Transaction stm: [:tx2 | (a deposit: 5) await ]) await result await) ]) await.
        tr result await.
        a balance await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(115));
}

#[test]
fn atomic_sibling_children_have_independent_buffers() {
    // S5b: two sibling inner `atomic:` blocks under one outer touch different
    // accounts. The first commits (absorbs into the outer), the second aborts.
    // The aborting sibling does not disturb the committed one: a = 100 + 5,
    // b = 100 (unchanged). Encoded as a*1000 + b = 105100.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a b tr |
        a := Account new.
        b := Account new.
        (a deposit: 100) await.
        (b deposit: 100) await.
        tr := (Transaction atomic: [:tx |
            (Transaction atomic: [:t1 | (a deposit: 5) await ]) await result await.
            [ (Transaction atomic: [:t2 | (b deposit: 7) await. t2 abort ]) await result await ]
                on: Error do: [:e | nil] ]) await.
        tr result await.
        (a balance await * 1000) + b balance await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(105100));
}

#[test]
fn atomic_sibling_children_serialize_on_shared_entity() {
    // S5b: two sibling children of one outer both do a read-modify-write on the
    // SAME account. Gated futures make the interleaving deterministic without any
    // timing assumption: child1 acquires a's tx lock (its deposit enrolls+locks a
    // to its ctx), signals g1, then parks on g2. Only after g1 does the outer
    // launch child2, whose deposit message parks in a's mailbox (its sibling ctx
    // is not a descendant of child1's) until child1 exits. Releasing g2 lets
    // child1 commit/absorb; child2 then proceeds and reads child1's value.
    //
    // Serialized-correct result: 100 + 5 + 7 = 112 (child2 reads child1's absorbed
    // write via the shared outer tx). A broken lock would let child2 read the stale
    // 100 and lose child1's update.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a g1 g2 tr |
        a := Account new.
        (a deposit: 100) await.
        g1 := Future new.
        g2 := Future new.
        tr := (Transaction atomic: [:tx |
            | r1 r2 |
            r1 := Transaction atomic: [:t1 |
                (a deposit: 5) await.
                g1 resolveWith: true.
                g2 await ].
            g1 await.
            r2 := Transaction atomic: [:t2 | (a deposit: 7) await ].
            g2 resolveWith: true.
            r1 result await.
            r2 result await ]) await.
        tr result await.
        a balance await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(112));
}

#[test]
fn atomic_deadlock_is_detected_and_one_victim_aborts() {
    // S5c: two independent root transactions cross-wait — T1 locks a then wants b;
    // T2 locks b then wants a. Gated futures make the cycle deterministic: each
    // signals after acquiring its first lock, then both are released together to
    // grab the second (which parks, forming A->B->A). With a short deadlock
    // timeout, the detector finds the cycle and aborts one victim; the survivor
    // commits both its deposits. Whichever tx wins, it deposits 1 into each of a
    // and b and the loser rolls back entirely, so a=101, b=101 either way.
    // Encoded as a*1000 + b = 101101.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a b g1 g2 go tr1 tr2 |
        Transaction deadlockTimeoutMs: 50.
        a := Account new.
        b := Account new.
        (a deposit: 100) await.
        (b deposit: 100) await.
        g1 := Future new.
        g2 := Future new.
        go := Future new.
        tr1 := Transaction atomic: [:t1 |
            (a deposit: 1) await. g1 resolveWith: true. go await. (b deposit: 1) await ].
        tr2 := Transaction atomic: [:t2 |
            (b deposit: 1) await. g2 resolveWith: true. go await. (a deposit: 1) await ].
        g1 await.
        g2 await.
        go resolveWith: true.
        [ tr1 await result await ] on: Error do: [:e | nil].
        [ tr2 await result await ] on: Error do: [:e | nil].
        (a balance await * 1000) + b balance await
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(101101));
}

#[test]
fn atomic_deadlock_victim_raises_deadlock_condition() {
    // S5c: the aborted victim's result future rejects specifically with a Deadlock
    // condition (an Error subclass). Exactly one of the two rejects; catch Deadlock
    // on each and count.
    let src = r#"
        Object subclass: #Account instanceVariableNames: 'balance'.
        Account >> initialize [ balance := 0 ]
        Account >> balance    [ ^balance ]
        Account >> deposit: n  [ balance := balance + n ]

        | a b g1 g2 go tr1 tr2 c1 c2 |
        Transaction deadlockTimeoutMs: 50.
        a := Account new.
        b := Account new.
        (a deposit: 100) await.
        (b deposit: 100) await.
        g1 := Future new.
        g2 := Future new.
        go := Future new.
        tr1 := Transaction atomic: [:t1 |
            (a deposit: 1) await. g1 resolveWith: true. go await. (b deposit: 1) await ].
        tr2 := Transaction atomic: [:t2 |
            (b deposit: 1) await. g2 resolveWith: true. go await. (a deposit: 1) await ].
        g1 await.
        g2 await.
        go resolveWith: true.
        "Each on:do: returns 1 if that tx's result rejected with a Deadlock, else 0."
        c1 := [ tr1 await result await. 0 ] on: Deadlock do: [:e | 1].
        c2 := [ tr2 await result await. 0 ] on: Deadlock do: [:e | 1].
        c1 + c2
    "#;
    assert_eq!(run_in_coroutine(src), Value::Int(1));
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
            version: std::sync::atomic::AtomicU64::new(0),
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
        d.tx_push_atomic_frame(7, shared_id(&d));
        assert!(d.in_tx());
        assert_eq!(d.tx_depth(), 1);
        assert_eq!(d.tx_holder(), Some(7));
        // Live slot untouched by enrollment.
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }

    #[test]
    fn shadow_write_masks_live_slot_via_shadow_get() {
        let d = entity_with_slot(1);
        d.tx_push_atomic_frame(7, shared_id(&d));
        d.tx_shadow_set_for(7, Arc::from("slot"), Value::Int(99));
        // Read-your-writes through the shadow; live slot still the old value.
        assert_eq!(d.tx_shadow_get_for(7, "slot"), Some(Value::Int(99)));
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }

    #[test]
    fn root_commit_applies_shadow_to_live_and_returns_buffer() {
        let d = entity_with_slot(1);
        d.tx_push_atomic_frame(7, shared_id(&d));
        d.tx_shadow_set_for(7, Arc::from("slot"), Value::Int(99));
        let outcome = d.tx_commit_frame(7, None); // root commit
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
        d.tx_push_atomic_frame(7, shared_id(&d));
        d.tx_shadow_set_for(7, Arc::from("slot"), Value::Int(99));
        d.tx_abort_frame(7);
        assert!(!d.in_tx());
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }

    #[test]
    fn child_commit_merges_into_parent_frame_not_live() {
        // Entity enrolled at root (ctx 1) then a child (ctx 2), one shared TxId.
        let d = entity_with_slot(1);
        let tx_id = shared_id(&d);
        d.tx_push_atomic_frame(1, tx_id);
        d.tx_shadow_set_for(1, Arc::from("slot"), Value::Int(10)); // root-level write
        d.tx_push_atomic_frame(2, tx_id);
        d.tx_shadow_set_for(2, Arc::from("slot"), Value::Int(20)); // child-level write
        let outcome = d.tx_commit_frame(2, Some(1)); // child commits into parent
        assert!(
            matches!(
                outcome,
                crate::entity::FrameCommitOutcome::MergedIntoAncestor
            ),
            "child commit merges into the ancestor frame"
        );
        assert_eq!(d.tx_depth(), 1, "back to the root level");
        // Child write superseded the root write within the surviving frame.
        assert_eq!(d.tx_shadow_get_for(1, "slot"), Some(Value::Int(20)));
        // Still not live until the root commits.
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }

    #[test]
    fn child_abort_discards_only_child_frame() {
        let d = entity_with_slot(1);
        let tx_id = shared_id(&d);
        d.tx_push_atomic_frame(1, tx_id);
        d.tx_shadow_set_for(1, Arc::from("slot"), Value::Int(10));
        d.tx_push_atomic_frame(2, tx_id);
        d.tx_shadow_set_for(2, Arc::from("slot"), Value::Int(20));
        d.tx_abort_frame(2); // child aborts
        assert_eq!(d.tx_depth(), 1);
        // Root-level write survives; child write gone.
        assert_eq!(d.tx_shadow_get_for(1, "slot"), Some(Value::Int(10)));
    }

    #[test]
    fn child_commit_passes_lock_up_when_parent_never_enrolled() {
        // Entity first touched during the child (ctx 2); root (ctx 1) never
        // separately enrolled it, so depth is 1.
        let d = entity_with_slot(1);
        d.tx_push_atomic_frame(2, shared_id(&d));
        d.tx_shadow_set_for(2, Arc::from("slot"), Value::Int(20));
        let outcome = d.tx_commit_frame(2, Some(1)); // lock passes up to parent
        assert!(
            matches!(outcome, crate::entity::FrameCommitOutcome::PassedUpTo(1)),
            "lock pass-up retags the frame to the parent"
        );
        assert_eq!(d.tx_depth(), 1, "still locked, now under the parent");
        assert_eq!(d.tx_holder(), Some(1));
        // Speculative write retained at the parent level, not yet live.
        assert_eq!(d.tx_shadow_get_for(1, "slot"), Some(Value::Int(20)));
        assert_eq!(d.data.get_slot("slot"), Some(Value::Int(1)));
    }
}

// ── C31 S3: ancestry-based mailbox admission ──────────────────────────────────

#[cfg(test)]
mod tx_admission {
    use super::*;
    use crate::shared_tx::{AtomicTx, GenTx, TxCommon};
    use std::collections::VecDeque;
    use triple_store::id::TxId;

    // Admission is discipline-agnostic; these helpers build locking (atomic)
    // levels with no real durable buffer (tx = None).
    fn atomic_common(ctx_id: u64) -> TxCommon {
        TxCommon {
            ctx_id,
            tx_id: TxId(1),
            tx: Mutex::new(None),
            participants: Mutex::new(Vec::new()),
            spawned: Mutex::new(std::collections::HashSet::new()),
        }
    }

    fn root_ctx(ctx_id: u64) -> GenTx {
        GenTx::Atomic(Arc::new(AtomicTx {
            common: atomic_common(ctx_id),
            parent: None,
        }))
    }

    fn child_ctx(ctx_id: u64, outer: GenTx) -> GenTx {
        GenTx::Atomic(Arc::new(AtomicTx {
            common: atomic_common(ctx_id),
            parent: Some(outer),
        }))
    }

    fn envelope(tag: Option<GenTx>) -> Envelope {
        Envelope {
            selector: Arc::from("ping"),
            args: vec![],
            reply: FutureHandle::new(),
            inherited_chain: vec![],
            tx_tag: tag,
        }
    }

    fn queue(tags: Vec<Option<GenTx>>) -> VecDeque<Envelope> {
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
    use crate::shared_tx::{GenTx, TxMode};

    #[test]
    fn contagious_atomic_coercion_in_gentx_begin() {
        // The type-level invariant: `#stm` can never be nested under an `#atomic`
        // ancestor. `GenTx::begin` coerces such a request to `Atomic`, so
        // `discipline()` reflects the effective (contagious) discipline.
        use triple_store::exec_context::ExecContext;
        let ev = Evaluator::new_without_kernel();
        let mk = |requested: TxMode, parent: Option<GenTx>| {
            let tx_id = ev.store.alloc_tx_id();
            let buf = ev.store.begin_owned_with_id(ExecContext::System, tx_id);
            GenTx::begin(requested, tx_id, buf, parent)
        };
        let a_root = mk(TxMode::Atomic, None);
        assert_eq!(a_root.discipline(), TxMode::Atomic);
        let s_root = mk(TxMode::Stm, None);
        assert_eq!(s_root.discipline(), TxMode::Stm);

        // stm under atomic → coerced to atomic.
        let s_under_a = mk(TxMode::Stm, Some(a_root.clone()));
        assert_eq!(s_under_a.discipline(), TxMode::Atomic);
        // stm under stm → stays stm.
        assert_eq!(
            mk(TxMode::Stm, Some(s_root.clone())).discipline(),
            TxMode::Stm
        );
        // atomic under stm → atomic (atomic nests freely).
        assert_eq!(
            mk(TxMode::Atomic, Some(s_root.clone())).discipline(),
            TxMode::Atomic
        );
        // stm under a *coerced*-atomic is also atomic (contagion is transitive).
        assert_eq!(
            mk(TxMode::Stm, Some(s_under_a.clone())).discipline(),
            TxMode::Atomic
        );
    }

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
        let h = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        let ctx = match h {
            Value::Handle(h) => h.downcast::<GenTx>().unwrap(),
            _ => panic!("expected a handle"),
        };
        assert!(ctx.is_root());
        assert!(ev.current_tx_ctx.borrow().is_some());
        assert_eq!(
            ev.current_tx_ctx.borrow().as_ref().unwrap().ctx_id(),
            ctx.ctx_id()
        );
    }

    #[test]
    fn version_bumps_while_stm_active() {
        use crate::entity::{stm_active_enter, stm_active_exit};
        let ev = Evaluator::new_without_kernel();
        let cell = cell_entity(&ev, 0);
        let d = entity_data(&cell);

        // `STM_ACTIVE` is process-global, so we cannot assert the gated-OFF case
        // reliably (parallel STM tests may hold it armed). We assert the robust
        // direction: while WE hold the gate, each bump advances THIS entity's
        // version (only this test touches it) by exactly one. The gated-off
        // optimization is covered by inspection + the integration conflict tests.
        let base = d.version();
        stm_active_enter();
        d.bump_version_if_stm();
        d.bump_version_if_stm();
        stm_active_exit();
        assert_eq!(
            d.version(),
            base + 2,
            "each live-write bump advances the version while STM is active"
        );
    }

    #[test]
    fn nested_begin_creates_child_of_root() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        let root_ctx = match &root {
            Value::Handle(h) => h.clone().downcast::<GenTx>().unwrap(),
            _ => unreachable!(),
        };
        let child = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        let child_ctx = match child {
            Value::Handle(h) => h.downcast::<GenTx>().unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(
            child_ctx.parent().expect("expected a child").ctx_id(),
            root_ctx.ctx_id()
        );
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
        let root = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        call(&ev, &coord, "_primCommitSharedTx:", vec![root]).unwrap();
        assert!(ev.current_tx_ctx.borrow().is_none());
    }

    #[test]
    fn child_commit_restores_parent_as_active() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        let root_id = match &root {
            Value::Handle(h) => h.clone().downcast::<GenTx>().unwrap().ctx_id(),
            _ => unreachable!(),
        };
        let child = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
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
        let root = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        call(&ev, &coord, "_primRollbackSharedTx:", vec![root]).unwrap();
        assert!(ev.current_tx_ctx.borrow().is_none());
    }

    #[test]
    fn enrolled_participants_returns_cons_list() {
        let ev = Evaluator::new(); // build_cons_list needs the kernel Cons class
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        let ctx = match &root {
            Value::Handle(h) => h.clone().downcast::<GenTx>().unwrap(),
            _ => unreachable!(),
        };
        // Simulate two participants enrolled at the root level.
        let p1 = cell_entity(&ev, 1);
        let p2 = cell_entity(&ev, 2);
        ctx.participants().lock().unwrap().push(p1.clone());
        ctx.participants().lock().unwrap().push(p2.clone());
        let list = call(&ev, &coord, "_primEnrolledParticipants:", vec![root]).unwrap();
        // Walk the head/tail Cons chain and count the nodes.
        let mut count = 0;
        let mut node = list;
        while let Value::Instance(h) = &node {
            assert_eq!(h.0.class.name.as_ref(), "Cons");
            let tail = h.ivar_storage().get_slot("tail").unwrap_or(Value::Nil);
            count += 1;
            node = tail;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn enrolled_participants_empty_is_nil() {
        let ev = Evaluator::new();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        let list = call(&ev, &coord, "_primEnrolledParticipants:", vec![root]).unwrap();
        assert_eq!(list, Value::Nil);
    }

    #[test]
    fn exit_commit_applies_shadow_to_live_and_unlocks() {
        let ev = Evaluator::new_without_kernel();
        let coord = cell_entity(&ev, 0);
        let root = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        let ctx = match &root {
            Value::Handle(h) => h.clone().downcast::<GenTx>().unwrap(),
            _ => unreachable!(),
        };
        // Enroll a participant at root and stage a speculative write.
        let p = cell_entity(&ev, 1);
        let pd = entity_data(&p);
        pd.tx_push_atomic_frame(ctx.ctx_id(), ctx.tx_id());
        pd.tx_shadow_set_for(ctx.ctx_id(), Arc::from("slot"), Value::Int(42));
        assert_eq!(pd.data.get_slot("slot"), Some(Value::Int(1))); // live untouched

        // The coordinator's pvtExitTx arrives tagged with the context.
        *ev.current_tx_ctx.borrow_mut() = Some((*ctx).clone());
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
        let root = call(&ev, &coord, "_primBeginAtomicTx", vec![]).unwrap();
        let ctx = match &root {
            Value::Handle(h) => h.clone().downcast::<GenTx>().unwrap(),
            _ => unreachable!(),
        };
        let p = cell_entity(&ev, 1);
        let pd = entity_data(&p);
        pd.tx_push_atomic_frame(ctx.ctx_id(), ctx.tx_id());
        pd.tx_shadow_set_for(ctx.ctx_id(), Arc::from("slot"), Value::Int(42));

        *ev.current_tx_ctx.borrow_mut() = Some((*ctx).clone());
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
