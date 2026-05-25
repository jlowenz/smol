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

// ── Instance types ────────────────────────────────────────────────────────────

/// Lifecycle state of an actor entity.
#[derive(Debug, Clone)]
pub enum EntityStatus {
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

/// Per-entity isolated data: instance variables, mailbox, status.
///
/// `class` is held in the enclosing `InstanceState`; this struct carries only
/// the data that is exclusive to actor entities.
pub struct EntityData {
    pub id: EntityId,
    /// Instance variable storage. Accessed exclusively by the entity's coroutine.
    pub data: Arc<Object>,
    pub mailbox: Mutex<VecDeque<Envelope>>,
    pub mailbox_ready: Condvar,
    pub status: Mutex<EntityStatus>,
}

/// Discriminates the semantics of an object instance.
pub enum InstanceKind {
    /// Actor entity — async mailbox dispatch, dedicated coroutine.
    Entity(Box<EntityData>),
    /// Passive value object — synchronous dispatch, safely shared across threads.
    ValueObject(Arc<Object>),
    /// Class object — synchronous class-side dispatch.
    ///
    /// The class descriptor is `InstanceState.class`. Making class objects full
    /// actors (async dispatch) is deferred to a later chunk.
    ClassObject,
}

impl std::fmt::Debug for InstanceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstanceKind::Entity(data) => write!(f, "Entity(id={})", data.id),
            InstanceKind::ValueObject(_) => write!(f, "ValueObject"),
            InstanceKind::ClassObject => write!(f, "ClassObject"),
        }
    }
}

/// Shared state for every object instance regardless of kind.
pub struct InstanceState {
    pub class: Arc<Class>,
    pub kind: InstanceKind,
}

/// Reference to any object instance.
///
/// Replaces the old `EntityHandle`, `Arc<Object>` (in ValueObject context), and
/// `Arc<Class>` (as a `Value` binding). The class descriptor is always accessible
/// as `handle.0.class` for method lookup.
#[derive(Clone)]
pub struct InstanceHandle(pub Arc<InstanceState>);

impl InstanceHandle {
    /// Returns the instance variable storage for this handle.
    ///
    /// Requires: `self.0.kind` is `Entity` or `ValueObject`.
    pub fn ivar_storage(&self) -> &Arc<Object> {
        match &self.0.kind {
            InstanceKind::Entity(d) => &d.data,
            InstanceKind::ValueObject(o) => o,
            InstanceKind::ClassObject => panic!("ClassObject has no instance variable storage"),
        }
    }
}

impl std::fmt::Debug for InstanceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0.kind {
            InstanceKind::Entity(data) => write!(f, "InstanceHandle::Entity(id={})", data.id),
            InstanceKind::ValueObject(_) => {
                write!(f, "InstanceHandle::ValueObject({})", self.0.class.name)
            }
            InstanceKind::ClassObject => {
                write!(f, "InstanceHandle::ClassObject({})", self.0.class.name)
            }
        }
    }
}

// ── Entity spawning ───────────────────────────────────────────────────────────

/// Spawn a new actor entity with an empty mailbox and no `initialize` queued.
///
/// Requires: `class.inherits_is_value()` is `false`.
/// Ensures: the entity's coroutine is running in `Running` state; the mailbox
///   is empty; `initialize` has NOT been enqueued.
pub fn spawn_entity_bare(
    class: Arc<Class>,
    registry: Arc<ClassRegistry>,
    global_env: Env,
    indexed_size: Option<usize>,
) -> InstanceHandle {
    let id = next_entity_id();
    let obj = if let Some(n) = indexed_size {
        Arc::new(Object::new_with_indexed(class.clone(), n))
    } else {
        Arc::new(Object::new(class.clone()))
    };

    let state = Arc::new(InstanceState {
        class,
        kind: InstanceKind::Entity(Box::new(EntityData {
            id,
            data: obj,
            mailbox: Mutex::new(VecDeque::new()),
            mailbox_ready: Condvar::new(),
            status: Mutex::new(EntityStatus::Running),
        })),
    });

    let handle = InstanceHandle(state.clone());

    let state_clone = state.clone();
    let registry_clone = registry.clone();
    let env_clone = global_env.clone();
    may::config().set_stack_size(0x4000);
    may::go!(move || {
        run_entity_loop(state_clone, registry_clone, env_clone);
    });

    handle
}

/// Enqueue a message to an entity and return a future for the reply.
///
/// Requires: `data.status` is `Running`; called from outside the entity's own coroutine.
/// Ensures: the envelope is appended to the mailbox; the returned
///   `FutureHandle` resolves when the entity completes the handler.
pub fn send_to_entity(data: &EntityData, selector: Arc<str>, args: Vec<Value>) -> FutureHandle {
    let future = FutureHandle::new();
    let mut mb = data.mailbox.lock().unwrap();
    mb.push_back(Envelope {
        selector,
        args,
        reply: future.clone(),
    });
    data.mailbox_ready.notify_one();
    future
}

// ── Entity message loop ───────────────────────────────────────────────────────

fn run_entity_loop(state: Arc<InstanceState>, registry: Arc<ClassRegistry>, global_env: Env) {
    use crate::eval::EvalError;
    use crate::eval::Evaluator;

    let data = match &state.kind {
        InstanceKind::Entity(d) => d.as_ref(),
        _ => panic!("run_entity_loop called on non-Entity instance"),
    };

    let ev = Evaluator::with_registry(registry);
    ev.current_entity_id.set(Some(data.id));

    let mut env = global_env;

    loop {
        // Wait for an envelope.
        let envelope = {
            let mut mb = data.mailbox.lock().unwrap();
            while mb.is_empty() {
                mb = data.mailbox_ready.wait(mb).unwrap();
            }
            mb.pop_front().unwrap()
        };

        // Reject envelopes if the entity has already faulted.
        {
            let status = data.status.lock().unwrap();
            if let EntityStatus::Faulted(msg) = &*status {
                envelope
                    .reply
                    .resolve_err(EvalError::Signal(Arc::from(msg.as_str())));
                continue;
            }
        }

        let result = dispatch_envelope(
            &ev,
            InstanceHandle(state.clone()),
            &envelope.selector,
            envelope.args,
            &mut env,
        );

        match result {
            Ok(v) => {
                envelope.reply.resolve_ok(v);
            }
            Err(EvalError::Return(v, _)) => {
                // Non-local return from a cross-entity block: resolve with v.
                envelope.reply.resolve_ok(v);
            }
            Err(e) => {
                let msg = e.to_string();
                *data.status.lock().unwrap() = EntityStatus::Faulted(msg.clone());
                envelope.reply.resolve_err(e);
                // Drain all already-queued messages so their futures resolve immediately
                // rather than parking forever.
                let mut mb = data.mailbox.lock().unwrap();
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
    handle: InstanceHandle,
    selector: &Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, crate::eval::EvalError> {
    if let Some((method, def_class)) = handle.0.class.lookup(selector) {
        return ev.invoke_method(handle.clone(), def_class, method, args, env);
    }
    crate::builtins::dispatch_object_primitive(handle, selector, args)
}

#[cfg(test)]
#[path = "tests/entity.rs"]
mod tests;
