use may::sync::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::class::Class;
use crate::class_registry::ClassRegistry;
use crate::env::Env;
use crate::eval::EvalError;
use crate::object::Object;
use crate::value::Value;

/// Unique identifier for an actor entity instance.
pub type EntityId = u64;

static NEXT_ENTITY_ID: AtomicU64 = AtomicU64::new(1);

fn next_entity_id() -> EntityId {
    NEXT_ENTITY_ID.fetch_add(1, Ordering::Relaxed)
}

// ── FutureHandle ──────────────────────────────────────────────────────────────

struct FutureInner {
    result: Option<Result<Value, EvalError>>,
}

/// Shared state between the entity that produces a result and any entity
/// waiting to consume it.
pub struct FutureState {
    inner: Mutex<FutureInner>,
    resolved: Condvar,
}

/// A lazy future reference. Resolved only when a message is dispatched to it;
/// forwarded by `Arc::clone` for all other operations.
#[derive(Clone)]
pub struct FutureHandle(pub Arc<FutureState>);

impl Default for FutureHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl FutureHandle {
    /// Allocate a new unresolved future.
    pub fn new() -> Self {
        FutureHandle(Arc::new(FutureState {
            inner: Mutex::new(FutureInner { result: None }),
            resolved: Condvar::new(),
        }))
    }

    /// Parks the calling coroutine (or OS thread) until the future is resolved,
    /// then returns the result.
    ///
    /// Requires: called from a context that may block (entity handler coroutine or test thread).
    /// Ensures: returns the resolved result; subsequent calls return the same result.
    pub fn wait(&self) -> Result<Value, EvalError> {
        let mut guard = self.0.inner.lock().unwrap();
        while guard.result.is_none() {
            guard = self.0.resolved.wait(guard).unwrap();
        }
        guard.result.clone().unwrap()
    }

    /// Resolves the future successfully with `v`, unblocking all waiters.
    ///
    /// Requires: called at most once per `FutureHandle` instance.
    /// Ensures: all waiters are woken and will return `Ok(v)`.
    pub fn resolve_ok(&self, v: Value) {
        let mut guard = self.0.inner.lock().unwrap();
        debug_assert!(guard.result.is_none(), "FutureHandle resolved twice");
        guard.result = Some(Ok(v));
        self.0.resolved.notify_all();
    }

    /// Resolves the future with an error, unblocking all waiters.
    ///
    /// Requires: called at most once per `FutureHandle` instance.
    /// Ensures: all waiters are woken and will return `Err(e)`.
    pub fn resolve_err(&self, e: EvalError) {
        let mut guard = self.0.inner.lock().unwrap();
        debug_assert!(guard.result.is_none(), "FutureHandle resolved twice");
        guard.result = Some(Err(e));
        self.0.resolved.notify_all();
    }

    /// Returns `true` if this future has already been resolved.
    pub fn is_resolved(&self) -> bool {
        self.0.inner.lock().unwrap().result.is_some()
    }

    /// If already resolved, return the result without blocking; else `None`.
    pub fn try_get(&self) -> Option<Result<Value, EvalError>> {
        self.0.inner.lock().unwrap().result.clone()
    }
}

impl std::fmt::Debug for FutureHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let resolved = self.is_resolved();
        write!(f, "FutureHandle(resolved={})", resolved)
    }
}

// ── EntityHandle ──────────────────────────────────────────────────────────────

/// Lifecycle state of an actor entity.
#[derive(Debug, Clone)]
pub enum EntityStatus {
    /// `initialize` is queued but not yet complete. The held future resolves
    /// when `initialize` finishes, allowing sequential contexts (e.g. `syncValue`)
    /// to wait before sending further messages.
    Initializing(FutureHandle),
    Running,
    Faulted(String),
    Terminated,
}

/// A message queued for delivery to an entity.
pub struct Envelope {
    pub selector: Arc<str>,
    pub args: Vec<Value>,
    /// The calling entity (or test) waits on this future for the reply.
    pub reply: FutureHandle,
}

/// Per-entity isolated state: instance variables, mailbox, and green-thread coroutine.
pub struct EntityState {
    pub id: EntityId,
    pub class: Arc<Class>,
    /// The entity's own Object instance (instance variables).
    /// Accessed exclusively by the entity's own coroutine.
    pub data: Arc<Object>,
    pub mailbox: Mutex<VecDeque<Envelope>>,
    pub mailbox_ready: Condvar,
    pub status: Mutex<EntityStatus>,
}

/// An `Arc`-wrapped reference to an `EntityState`. Actor entity references
/// in Smol are represented by this type.
#[derive(Clone)]
pub struct EntityHandle(pub Arc<EntityState>);

impl std::fmt::Debug for EntityHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EntityHandle(id={})", self.0.id)
    }
}

// ── Entity spawning ───────────────────────────────────────────────────────────

/// Spawn a new actor entity: allocate its state, start its green-thread coroutine
/// message loop, queue `initialize`, and return the handle.
///
/// Requires: `class.inherits_is_value()` is `false`.
/// Ensures: the entity's coroutine is running and will process `initialize`
///   as its first message.
pub fn spawn_entity(
    class: Arc<Class>,
    registry: Arc<ClassRegistry>,
    global_env: Env,
    indexed_size: Option<usize>,
) -> EntityHandle {
    let id = next_entity_id();
    let obj = if let Some(n) = indexed_size {
        Arc::new(Object::new_with_indexed(class.clone(), n))
    } else {
        Arc::new(Object::new(class.clone()))
    };

    let init_future = FutureHandle::new();

    let entity = Arc::new(EntityState {
        id,
        class: class.clone(),
        data: obj,
        mailbox: Mutex::new(VecDeque::new()),
        mailbox_ready: Condvar::new(),
        status: Mutex::new(EntityStatus::Initializing(init_future.clone())),
    });

    let handle = EntityHandle(entity.clone());

    // Enqueue `initialize` using the same future stored in the status so that
    // `eval_program_sync` can wait on it via the entity's status.
    {
        let mut mb = entity.mailbox.lock().unwrap();
        mb.push_back(Envelope {
            selector: Arc::from("initialize"),
            args: vec![],
            reply: init_future,
        });
        entity.mailbox_ready.notify_one();
    }

    // Spawn the entity's green-thread coroutine.
    let entity_clone = entity.clone();
    let registry_clone = registry.clone();
    let env_clone = global_env.clone();
    may::config().set_stack_size(0x4000);
    may::go!(move || {
        run_entity_loop(entity_clone, registry_clone, env_clone);
    });

    handle
}

/// Enqueue a message to an entity and return a future for the reply.
///
/// Requires: `entity` status is `Running`.
/// Ensures: the envelope is appended to the mailbox; the returned
///   `FutureHandle` resolves when the entity completes the handler.
pub fn send_to_entity(
    entity: &Arc<EntityState>,
    selector: Arc<str>,
    args: Vec<Value>,
) -> FutureHandle {
    let future = FutureHandle::new();
    let mut mb = entity.mailbox.lock().unwrap();
    mb.push_back(Envelope {
        selector,
        args,
        reply: future.clone(),
    });
    entity.mailbox_ready.notify_one();
    future
}

// ── Entity message loop ───────────────────────────────────────────────────────

fn run_entity_loop(entity: Arc<EntityState>, registry: Arc<ClassRegistry>, global_env: Env) {
    use crate::eval::EvalError;
    use crate::eval::Evaluator;

    let ev = Evaluator::with_registry(registry);
    ev.current_entity_id.set(Some(entity.id));

    let mut env = global_env;
    let self_val = Value::Object(EntityHandle(entity.clone()));

    loop {
        // Wait for an envelope.
        let envelope = {
            let mut mb = entity.mailbox.lock().unwrap();
            while mb.is_empty() {
                mb = entity.mailbox_ready.wait(mb).unwrap();
            }
            mb.pop_front().unwrap()
        };

        // Reject envelopes if the entity has already faulted.
        {
            let status = entity.status.lock().unwrap();
            if let EntityStatus::Faulted(msg) = &*status {
                envelope
                    .reply
                    .resolve_err(EvalError::Signal(Arc::from(msg.as_str())));
                continue;
            }
        }

        let result = dispatch_envelope(
            &ev,
            &entity,
            &envelope.selector,
            envelope.args,
            self_val.clone(),
            &mut env,
        );

        // ev.current_entity_id.set(None);

        match result {
            Ok(v) => {
                envelope.reply.resolve_ok(v);
                // Transition Initializing → Running once `initialize` completes.
                let mut status = entity.status.lock().unwrap();
                if matches!(*status, EntityStatus::Initializing(_)) {
                    *status = EntityStatus::Running;
                }
            }
            Err(EvalError::Return(v, _)) => {
                // Non-local return from a cross-entity block: resolve with v.
                envelope.reply.resolve_ok(v);
                let mut status = entity.status.lock().unwrap();
                if matches!(*status, EntityStatus::Initializing(_)) {
                    *status = EntityStatus::Running;
                }
            }
            Err(e) => {
                let msg = e.to_string();
                // Overwrites Initializing if initialize itself errored.
                *entity.status.lock().unwrap() = EntityStatus::Faulted(msg.clone());
                envelope.reply.resolve_err(e);
                // Drain all already-queued messages so their futures resolve immediately
                // rather than parking forever.
                let mut mb = entity.mailbox.lock().unwrap();
                for pending in mb.drain(..) {
                    pending
                        .reply
                        .resolve_err(EvalError::Signal(Arc::from(msg.as_str())));
                }
                break;
            }
        }
    }
}

fn dispatch_envelope(
    ev: &crate::eval::Evaluator,
    entity: &Arc<EntityState>,
    selector: &Arc<str>,
    args: Vec<Value>,
    self_val: Value,
    env: &mut Env,
) -> Result<Value, crate::eval::EvalError> {
    let receiver = entity.data.clone();
    let class = entity.class.clone();

    // User-defined method?
    if let Some((method, def_class)) = class.lookup(selector) {
        return ev.invoke_method(receiver, def_class, method, args, env, self_val);
    }

    // Primitive Object methods (printString, class, =, ~=, _primHash, etc.)
    crate::builtins::dispatch_object_primitive_as_entity(receiver, selector, args, self_val)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
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
}
