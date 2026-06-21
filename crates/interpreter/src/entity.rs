use may::sync::{Condvar, Mutex};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::class::Class;
use crate::class_registry::ClassRegistry;
use crate::env::Env;
use crate::eval::EvalError;
use crate::object::Object;
use crate::shared_tx::{TxCtxId, TxFrame};
use crate::value::{Block, Value};
use triple_store::exec_context::ExecContext;
use triple_store::id::TxId;
use triple_store::store::{OwnedStoreTransaction, Store, StoreError};

pub use triple_store::id::EntityId;

/// Coroutine stack size for all entity and continuation coroutines.
///
/// Sized empirically with the iterative evaluator, which uses heap `CallFrame`s
/// for Smol method calls. The dominant Rust stack cost is nested
/// builtin→`eval_block` chains (e.g. `on:do:` wrapping `ensure:` wrapping
/// `do:`). Measured debug-build minimum: ~27 KB; release builds use noticeably
/// less due to inlining. Constant set to 32 KB for headroom.
pub(crate) const COROUTINE_STACK_SIZE: usize = 0x8000; // 256 KB — see S6 notes in chunk_30_plan.md

/// Set the global `may` stack size exactly once. Idempotent — safe to call from
/// any spawn site; subsequent calls are a no-op.
pub(crate) fn init_coroutine_stack() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        may::config().set_stack_size(COROUTINE_STACK_SIZE);
    });
}

static NEXT_ENTITY_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_entity_id() -> EntityId {
    EntityId(NEXT_ENTITY_ID.fetch_add(1, Ordering::Relaxed))
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
    /// HandlerContext handler pairs inherited from the sender's evaluator.
    /// Empty for ordinary sends; populated by cross-entity sends that originate
    /// inside a HandlerContext body.
    pub inherited_chain: Vec<(Arc<Class>, Arc<Block>)>,
    /// Transaction context this message is sent under (C31), or `None` for an
    /// ordinary send. Set from the sender's `current_tx_ctx`. The receiving
    /// entity uses it for ancestry-based mailbox admission and auto-enrollment.
    pub tx_tag: Option<Arc<crate::shared_tx::TxContext>>,
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
    /// Set by `signal_shutdown` to request the entity loop to exit cleanly.
    pub shutdown: AtomicBool,
    /// Shared store handle for temporal fact recording. All entities share the
    /// same underlying store; this is a cheap clone of the shared handle.
    pub store: Store,
    /// Lock + shadow stack for multi-entity transactions (C31). Empty when the
    /// entity is in no transaction. Each frame is owned by the context that
    /// pushed it (one per held level along the current ancestry path) and holds
    /// that level's speculative named-ivar writes. A non-empty stack means the
    /// mailbox is locked; the depth is the nesting/lock count.
    ///
    /// Invariant: while non-empty, the frames' `ctx_id`s run from an ancestor
    /// (bottom) to a descendant (top) of a single transaction tree — siblings
    /// never coexist here because they serialize on the entity.
    pub tx_frames: Mutex<Vec<TxFrame>>,
}

impl EntityData {
    /// Request the entity's coroutine to exit. Sets the shutdown flag and wakes
    /// the mailbox condvar so the loop sees the flag without waiting for a message.
    ///
    /// Ensures: the entity loop will transition to `EntityStatus::Terminated` and
    ///   drain any queued envelopes with a `Signal("entity terminated")` error.
    pub fn signal_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.mailbox_ready.notify_all();
    }

    // ── Transaction lock + shadow frames (C31) ────────────────────────────────
    //
    // These methods are the single place that manipulates `tx_frames`. The
    // evaluator (`assign_var`/`lookup_var`) and the tx primitives call them
    // rather than locking the stack directly, so the shadow-vs-live and
    // merge/discard rules live in one spot. They concern the *shared*
    // transaction's in-memory visibility only — durable store ops still flow
    // through the entity's private `current_tx` (see `store_bridge::buffer_ivar`).

    /// Number of transaction levels this entity is currently enrolled in along
    /// the active ancestry path; also the lock count.
    pub fn tx_depth(&self) -> usize {
        self.tx_frames.lock().unwrap().len()
    }

    /// Whether the entity is enrolled in any transaction (mailbox locked).
    pub fn in_tx(&self) -> bool {
        !self.tx_frames.lock().unwrap().is_empty()
    }

    /// `ctx_id` of the context currently holding the entity (top frame), if any.
    pub fn tx_holder(&self) -> Option<TxCtxId> {
        self.tx_frames.lock().unwrap().last().map(|f| f.ctx_id)
    }

    /// Enroll at a new level: push a fresh frame owned by `ctx_id`, opening this
    /// entity's durable buffer under the shared `tx_id`.
    ///
    /// Ensures: durable ops buffered while this frame is on top commit under
    ///   `tx_id`, so the whole transaction tree commits atomically.
    pub fn tx_push_frame(&self, ctx_id: TxCtxId, tx_id: TxId) {
        let tx = self
            .store
            .begin_owned_with_id(ExecContext::Entity(self.id), tx_id);
        self.tx_frames.lock().unwrap().push(TxFrame {
            ctx_id,
            shadow: HashMap::new(),
            tx,
        });
    }

    /// Run `f` with this entity's top frame's durable buffer.
    ///
    /// Requires: `in_tx()`. Used by `store_bridge` to route durable ops to the
    ///   active level's buffer instead of the private `current_tx`.
    pub fn with_frame_tx<R>(&self, f: impl FnOnce(&mut OwnedStoreTransaction) -> R) -> R {
        let mut frames = self.tx_frames.lock().unwrap();
        let top = frames
            .last_mut()
            .expect("with_frame_tx: entity has no active transaction frame");
        f(&mut top.tx)
    }

    /// Speculatively write a named ivar into the top shadow frame.
    ///
    /// Requires: `in_tx()`.
    pub fn tx_shadow_set(&self, name: Arc<str>, value: Value) {
        let mut frames = self.tx_frames.lock().unwrap();
        let top = frames
            .last_mut()
            .expect("tx_shadow_set: entity has no active transaction frame");
        top.shadow.insert(name, value);
    }

    /// Read a named ivar from the shadow stack, scanning top-to-bottom.
    ///
    /// Ensures: `Some(v)` iff some frame holds a speculative value for `name`
    ///   (read-your-writes with nesting); `None` if no frame does — the caller
    ///   then falls through to the live slot.
    pub fn tx_shadow_get(&self, name: &str) -> Option<Value> {
        let frames = self.tx_frames.lock().unwrap();
        frames
            .iter()
            .rev()
            .find_map(|f| f.shadow.get(name).cloned())
    }

    /// Commit the top transaction level, merging its speculative writes upward.
    ///
    /// `parent_ctx_id`:
    ///   - `None` → committing the root level for this entity: the frame's shadow
    ///     is applied to the live slots, the frame is popped (entity unlocks),
    ///     and its durable buffer is **returned** so the caller can absorb it
    ///     into the coordinator's context buffer for the eventual store commit.
    ///   - `Some(parent)` → committing a child level. If an ancestor frame is
    ///     below, the top frame's shadow and durable buffer are merged into it
    ///     (`None` returned). If the top is the only frame (the entity first
    ///     joined during this child; ancestors never touched it), the frame is
    ///     retagged to `parent` so the lock passes up — the entity stays locked,
    ///     keeping its durable buffer, and `None` is returned.
    ///
    /// Requires: `in_tx()`.
    /// Ensures: returns `Some(buffer)` exactly when the entity's last frame is a
    ///   root commit (`parent_ctx_id == None`); `None` otherwise.
    #[must_use]
    pub fn tx_commit_frame(&self, parent_ctx_id: Option<TxCtxId>) -> Option<OwnedStoreTransaction> {
        let mut frames = self.tx_frames.lock().unwrap();
        let top = frames
            .pop()
            .expect("tx_commit_frame: entity has no active transaction frame");
        match frames.last_mut() {
            // An ancestor level remains: child writes supersede ancestor writes
            // for shared keys (read-your-writes), and durable ops merge up.
            Some(below) => {
                below.shadow.extend(top.shadow);
                below.tx.absorb(top.tx);
                None
            }
            None => match parent_ctx_id {
                // Lock passes up to a parent that never separately enrolled: keep
                // the frame's shadow and durable buffer, retagged to the parent.
                Some(parent) => {
                    frames.push(TxFrame {
                        ctx_id: parent,
                        shadow: top.shadow,
                        tx: top.tx,
                    });
                    None
                }
                // Root commit: speculative writes become live; hand the durable
                // buffer to the caller to absorb into the context buffer.
                None => {
                    for (name, value) in top.shadow {
                        self.data.set_slot(name, value);
                    }
                    Some(top.tx)
                }
            },
        }
    }

    /// Abort the top transaction level: pop and discard its speculative writes
    /// and its durable buffer (dropping the buffer rolls back its ops — they were
    /// never absorbed into the context, so they never commit). Live slots are
    /// untouched.
    ///
    /// Requires: `in_tx()`.
    pub fn tx_abort_frame(&self) {
        self.tx_frames
            .lock()
            .unwrap()
            .pop()
            .expect("tx_abort_frame: entity has no active transaction frame");
    }
}

/// Discriminates the semantics of an object instance.
pub enum InstanceKind {
    /// Actor entity — async mailbox dispatch, dedicated coroutine.
    Entity(Box<EntityData>),
    /// Passive value object — synchronous dispatch, safely shared across threads.
    ValueObject(Arc<Object>),
    /// Class-side singleton actor — async mailbox dispatch, dedicated coroutine.
    ///
    /// Dispatch uses `class.lookup_class_method` rather than `class.lookup`.
    /// Class variables remain in `Class.class_var_storage`; unification with
    /// instance variable storage is deferred to a later chunk.
    ClassEntity(Box<EntityData>),
}

impl std::fmt::Debug for InstanceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstanceKind::Entity(data) => write!(f, "Entity(id={})", data.id.0),
            InstanceKind::ValueObject(_) => write!(f, "ValueObject"),
            InstanceKind::ClassEntity(data) => write!(f, "ClassEntity(id={})", data.id.0),
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
            InstanceKind::ClassEntity(_) => panic!("ClassEntity has no instance variable storage"),
        }
    }
}

impl std::fmt::Debug for InstanceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0.kind {
            InstanceKind::Entity(data) => write!(f, "InstanceHandle::Entity(id={})", data.id.0),
            InstanceKind::ValueObject(_) => {
                write!(f, "InstanceHandle::ValueObject({})", self.0.class.name)
            }
            InstanceKind::ClassEntity(_) => {
                write!(f, "InstanceHandle::ClassEntity({})", self.0.class.name)
            }
        }
    }
}

// ── Entity data construction ──────────────────────────────────────────────────

/// Allocate a fresh `EntityData` for `class` with an empty mailbox and `Running` status.
///
/// Requires: `store` is a handle to the shared store for this session.
/// Ensures: the returned data has a unique entity ID; no coroutine is spawned.
pub(crate) fn new_entity_data(class: &Arc<Class>, store: Store) -> EntityData {
    EntityData {
        id: next_entity_id(),
        data: Arc::new(Object::new(class.clone())),
        mailbox: Mutex::new(VecDeque::new()),
        mailbox_ready: Condvar::new(),
        status: Mutex::new(EntityStatus::Running),
        shutdown: AtomicBool::new(false),
        store,
        tx_frames: Mutex::new(Vec::new()),
    }
}

// ── Entity spawning ───────────────────────────────────────────────────────────

/// Spawn a class-side singleton actor for `class`.
///
/// Requires: `global_env` is built from a fully-populated registry before this
///   call — the caller must call `registry.populate_env` after all classes are
///   registered and then pass the result here. This matches `spawn_entity_bare`
///   and avoids a work-stealing race where the coroutine could call `populate_env`
///   on a still-incomplete registry.
/// Requires: `store` is a handle to the shared store for this session.
/// Ensures: the returned handle has `InstanceKind::ClassEntity`; `class.class_object`
///   is NOT set here — the caller must set it after this call returns.
pub fn spawn_class_entity(
    class: Arc<Class>,
    registry: Arc<ClassRegistry>,
    global_env: Env,
    store: Store,
) -> InstanceHandle {
    let data = new_entity_data(&class, store);
    let state = Arc::new(InstanceState {
        class,
        kind: InstanceKind::ClassEntity(Box::new(data)),
    });
    let handle = InstanceHandle(state.clone());

    let state_clone = state;
    let registry_clone = registry;
    let env_clone = global_env;
    init_coroutine_stack();
    may::go!(move || {
        run_entity_loop(state_clone, registry_clone, env_clone);
    });

    handle
}

/// Spawn a new actor entity with an empty mailbox and no `initialize` queued.
///
/// Requires: `class.inherits_is_value()` is `false`.
/// Requires: `store` is a handle to the shared store for this session.
/// Ensures: the entity's coroutine is running in `Running` state; the mailbox
///   is empty; `initialize` has NOT been enqueued.
pub fn spawn_entity_bare(
    class: Arc<Class>,
    registry: Arc<ClassRegistry>,
    global_env: Env,
    indexed_size: Option<usize>,
    store: Store,
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
            shutdown: AtomicBool::new(false),
            store,
            tx_frames: Mutex::new(Vec::new()),
        })),
    });

    let handle = InstanceHandle(state.clone());

    let state_clone = state.clone();
    let registry_clone = registry.clone();
    let env_clone = global_env.clone();
    init_coroutine_stack();
    may::go!(move || {
        run_entity_loop(state_clone, registry_clone, env_clone);
    });

    handle
}

/// Enqueue a message to an entity and return a future for the reply.
///
/// Requires: `data.status` is `Running`; called from outside the entity's own coroutine.
/// Ensures: the envelope is appended to the mailbox with `chain` attached;
///   the returned `FutureHandle` resolves when the entity completes the handler.
pub fn send_to_entity(
    data: &EntityData,
    selector: Arc<str>,
    args: Vec<Value>,
    chain: Vec<(Arc<Class>, Arc<Block>)>,
    tx_tag: Option<Arc<crate::shared_tx::TxContext>>,
) -> FutureHandle {
    let future = FutureHandle::new();
    let mut mb = data.mailbox.lock().unwrap();
    mb.push_back(Envelope {
        selector,
        args,
        reply: future.clone(),
        inherited_chain: chain,
        tx_tag,
    });
    data.mailbox_ready.notify_one();
    future
}

// ── Entity message loop ───────────────────────────────────────────────────────

/// Commit a transaction, treating `FactNotFound` as a non-fatal warning.
///
/// `FactNotFound` arises when nil is assigned to an ivar that was never set —
/// the retraction targets a non-existent open fact, but the desired state
/// (no value for this ivar) is already achieved, so no action is needed.
///
/// Ensures: returns `Ok(())` on success or on `FactNotFound`; returns `Err`
///   for all other `StoreError` variants so the caller can fault the entity.
/// Fault the entity, drain its mailbox, and resolve the current envelope's
/// reply with an error derived from `e`.
///
/// Requires: `data.status` is not already `Faulted` (called only on the first
///   commit failure for this dispatch).
/// Ensures: `data.status` is `EntityStatus::Faulted`; `envelope.reply` is
///   resolved with `EvalError::Signal`; every message currently in
///   `data.mailbox` is drained and resolved with the same error.
pub(crate) fn handle_commit_error(
    e: StoreError,
    data: &EntityData,
    ev: &crate::eval::Evaluator,
    envelope: &Envelope,
) {
    use crate::builtins::make_error_value;
    use crate::eval::EvalError;
    let msg = e.to_string();
    *data.status.lock().unwrap() = EntityStatus::Faulted(msg.clone());
    let fault_val = make_error_value(&ev.registry, &msg);
    envelope
        .reply
        .resolve_err(EvalError::Signal(fault_val.clone()));
    let mut mb = data.mailbox.lock().unwrap();
    for pending in mb.drain(..) {
        pending
            .reply
            .resolve_err(EvalError::Signal(fault_val.clone()));
    }
}

fn commit_or_warn(tx: OwnedStoreTransaction) -> Result<(), StoreError> {
    match tx.commit() {
        Ok(_) => Ok(()),
        Err(StoreError::FactNotFound) => {
            eprintln!(
                "[smol] commit warning: FactNotFound — nil assigned to never-set ivar; ignored"
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Index of the first mailbox envelope admissible given the current lock holder.
///
/// - `holder == None` (entity in no transaction): FIFO — the front envelope.
/// - `holder == Some(h)` (entity locked by context `h`): the first **tagged**
///   envelope whose context is `h` or a descendant of `h` (so the holder or a
///   deeper nested level may proceed). Untagged envelopes and envelopes from
///   sibling/unrelated contexts are skipped — they park in the mailbox until the
///   entity unlocks back to a common ancestor (sibling serialization).
pub(crate) fn admissible_index(mb: &VecDeque<Envelope>, holder: Option<TxCtxId>) -> Option<usize> {
    match holder {
        None => (!mb.is_empty()).then_some(0),
        Some(h) => mb.iter().position(|e| {
            e.tx_tag
                .as_ref()
                .is_some_and(|ctx| ctx.has_ancestor_or_self(h))
        }),
    }
}

fn run_entity_loop(state: Arc<InstanceState>, registry: Arc<ClassRegistry>, global_env: Env) {
    use crate::builtins::make_error_value;
    use crate::eval::EvalError;
    use crate::eval::Evaluator;

    let data = match &state.kind {
        InstanceKind::Entity(d) | InstanceKind::ClassEntity(d) => d.as_ref(),
        _ => panic!("run_entity_loop called on ValueObject instance"),
    };

    let ev = Evaluator::with_registry(registry);
    ev.current_entity_id.set(Some(data.id));

    let mut env = global_env;

    loop {
        // Wait for an envelope (or shutdown signal).
        let envelope = {
            let mut mb = data.mailbox.lock().unwrap();
            loop {
                if data.shutdown.load(Ordering::Acquire) {
                    let term_val = make_error_value(&ev.registry, "entity terminated");
                    for pending in mb.drain(..) {
                        pending
                            .reply
                            .resolve_err(EvalError::Signal(term_val.clone()));
                    }
                    *data.status.lock().unwrap() = EntityStatus::Terminated;
                    return;
                }
                // Ancestry-based admission: while locked into a transaction, only
                // take envelopes from the holding context or a descendant.
                let holder = data.tx_holder();
                if let Some(i) = admissible_index(&mb, holder) {
                    break mb
                        .remove(i)
                        .expect("admissible_index returns a valid index");
                }
                mb = data.mailbox_ready.wait(mb).unwrap();
            }
        };

        // Reject envelopes if the entity has already faulted.
        {
            let status = data.status.lock().unwrap();
            if let EntityStatus::Faulted(msg) = &*status {
                let fault_val = make_error_value(&ev.registry, msg);
                envelope.reply.resolve_err(EvalError::Signal(fault_val));
                continue;
            }
        }

        // C31: enroll into the tagged transaction level on the first message at
        // that level (push a frame, opening this entity's durable buffer under the
        // shared TxId, and record participation). Subsequent messages at the same
        // level reuse the existing frame.
        let tx_tag = envelope.tx_tag.clone();
        if let Some(ctx) = &tx_tag
            && data.tx_holder() != Some(ctx.ctx_id())
        {
            data.tx_push_frame(ctx.ctx_id(), ctx.tx_id());
            ctx.participants()
                .lock()
                .unwrap()
                .push(InstanceHandle(state.clone()));
        }

        // The private per-message transaction. For a tagged message this stays
        // empty — durable ivar writes route to the entity's frame buffer (see
        // `store_bridge::with_durable_tx`) — so its boundary commit is a harmless
        // no-op; the frame persists until `exitTx`.
        let tx = data.store.begin_owned(ExecContext::Entity(data.id));
        *ev.current_tx.borrow_mut() = Some(tx);

        // Propagate the transaction tag for the duration of this dispatch: it
        // routes durable writes to the frame buffer and tags any outgoing entity
        // sends so callees auto-enroll.
        *ev.current_tx_ctx.borrow_mut() = tx_tag;

        // Push inherited HC handlers so signal_condition can walk them.
        // Entity-local handlers (from _primHCRun: inside this dispatch) are
        // pushed on top of these and popped by _primHCRun: itself, so only
        // the inherited entries remain when the dispatch returns.
        //
        // Requires: ev.handler_chain is empty at message dispatch start.
        // Ensures: inherited handlers are visible during this dispatch and are
        //   cleared after dispatch completes regardless of outcome.
        let inherited_len = envelope.inherited_chain.len();
        if inherited_len > 0 {
            let mut chain = ev.handler_chain.borrow_mut();
            for pair in &envelope.inherited_chain {
                chain.push(pair.clone());
            }
        }

        let handle = InstanceHandle(state.clone());

        // Wrap dispatch in a retry loop; also convert ReturnFromBlock → Ok at
        // the entity dispatch level (return: exits handler context to caller).
        let result = loop {
            let r = match &state.kind {
                InstanceKind::Entity(_) => dispatch_envelope(
                    &ev,
                    handle.clone(),
                    &envelope.selector,
                    envelope.args.clone(),
                    &mut env,
                ),
                InstanceKind::ClassEntity(_) => dispatch_class_envelope(
                    &ev,
                    handle.clone(),
                    &envelope.selector,
                    envelope.args.clone(),
                    &mut env,
                ),
                InstanceKind::ValueObject(_) => unreachable!(),
            };
            match r {
                Err(EvalError::Retry) => continue,
                Err(EvalError::ReturnFromBlock(v)) => break Ok(v),
                other => break other,
            }
        };

        // Clear inherited handlers (entity-local handlers have already been
        // popped by _primHCRun:).
        if inherited_len > 0 {
            ev.handler_chain.borrow_mut().clear();
        }

        // Clear the transaction tag; the next message sets its own.
        *ev.current_tx_ctx.borrow_mut() = None;

        match result {
            Ok(v) => {
                if let Some(tx) = ev.current_tx.borrow_mut().take()
                    && let Err(e) = commit_or_warn(tx)
                {
                    handle_commit_error(e, data, &ev, &envelope);
                    break;
                }
                envelope.reply.resolve_ok(v);
            }
            Err(EvalError::Return(v, _)) => {
                // Non-local return from a cross-entity block: resolve with v.
                if let Some(tx) = ev.current_tx.borrow_mut().take()
                    && let Err(e) = commit_or_warn(tx)
                {
                    handle_commit_error(e, data, &ev, &envelope);
                    break;
                }
                envelope.reply.resolve_ok(v);
            }
            Err(EvalError::Signal(cond)) => {
                // Unhandled signal. ClassEntities (e.g. Error class, Condition class) resolve
                // the reply with the error and continue — they must not terminate because they
                // are stateless dispatch targets used for every signal in the system.
                // Instance entities log and terminate (actor "let it crash" policy).
                if let Some(tx) = ev.current_tx.borrow_mut().take() {
                    tx.rollback();
                }
                let is_class_entity = matches!(&state.kind, InstanceKind::ClassEntity(_));
                let class_name = state.class.name.clone();
                let msg = crate::builtins::condition_message(&cond);
                let fault_val = make_error_value(&ev.registry, &msg);
                if is_class_entity {
                    // Class entity: pass the error back to the caller and continue.
                    envelope.reply.resolve_err(EvalError::Signal(fault_val));
                } else {
                    // Instance entity: log, terminate, notify pending waiters.
                    eprintln!(
                        "Entity {} ({}) terminated by unhandled signal: {}",
                        data.id.0, class_name, msg
                    );
                    *data.status.lock().unwrap() = EntityStatus::Faulted(msg.clone());
                    envelope
                        .reply
                        .resolve_err(EvalError::Signal(fault_val.clone()));
                    // TODO C29: notify supervisor entity (EntityData.supervisor not yet present).
                    data.signal_shutdown();
                    let mut mb = data.mailbox.lock().unwrap();
                    for pending in mb.drain(..) {
                        pending
                            .reply
                            .resolve_err(EvalError::Signal(fault_val.clone()));
                    }
                    break;
                }
            }
            Err(e) => {
                if let Some(tx) = ev.current_tx.borrow_mut().take() {
                    tx.rollback();
                }
                let msg = e.to_string();
                *data.status.lock().unwrap() = EntityStatus::Faulted(msg.clone());
                envelope.reply.resolve_err(e);
                // Drain all already-queued messages so their futures resolve immediately
                // rather than parking forever.
                let fault_val = make_error_value(&ev.registry, &msg);
                let mut mb = data.mailbox.lock().unwrap();
                for pending in mb.drain(..) {
                    pending
                        .reply
                        .resolve_err(EvalError::Signal(fault_val.clone()));
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
    crate::builtins::dispatch_object_primitive(ev, handle, selector, args, env)
}

fn dispatch_class_envelope(
    ev: &crate::eval::Evaluator,
    handle: InstanceHandle,
    selector: &Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, crate::eval::EvalError> {
    // Refresh env from the registry before each dispatch so class-side methods can
    // reference classes defined after this actor was spawned (e.g. `ValueHolder new`
    // inside `ValueHolder class >> initialValue:`).
    ev.registry.populate_env(env);
    if let Some((method, def_class)) = handle.0.class.lookup_class_method(selector) {
        return ev.invoke_class_method(handle.clone(), def_class, method, args, env);
    }
    crate::builtins::dispatch_class_builtin(ev, handle, selector.clone(), args, env)
}

#[cfg(test)]
#[path = "tests/entity.rs"]
mod tests;
