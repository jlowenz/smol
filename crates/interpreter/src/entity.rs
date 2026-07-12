use may::sync::{Condvar, Mutex};
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::class::Class;
use crate::class_registry::ClassRegistry;
use crate::env::Env;
use crate::eval::EvalError;
use crate::object::Object;
use crate::shared_tx::{AtomicFrame, GenTx, StmFrame, TxCtxId, TxFrame};
use crate::value::{Block, Value};
use triple_store::exec_context::ExecContext;
use triple_store::id::TxId;
use triple_store::store::{OwnedStoreTransaction, Store, StoreError};

pub use triple_store::id::EntityId;

/// Coroutine stack size for all entity and continuation coroutines, in `usize`
/// words (× 8 bytes on 64-bit). This is the per-entity stack, so it bounds how
/// many entities can coexist — the smaller, the more entities.
///
/// After C31.5 the evaluator keeps essentially all nesting on the heap `Cont`
/// stack: method/class-method calls, `value`/`on:do:`/`ensure:`/`then:`,
/// `HandlerContext run:`, signals, value construction, and all iteration
/// (`to:do:` and everything built on it) are flattened, so the Rust stack is a
/// small constant independent of Smol nesting depth. The remaining cost is the
/// depth of one builtin-dispatch chain.
///
/// Debug frames are ~8× release (non-overlapping match-arm locals), so the two
/// builds get different sizes: debug keeps generous headroom for development;
/// release is sized for production density. Measured floor: ~5 KB per dispatch
/// chain in release, single-digit depth in normal code.
#[cfg(debug_assertions)]
pub(crate) const COROUTINE_STACK_SIZE: usize = 0x8000; // 256 KB (debug; fat frames)
#[cfg(not(debug_assertions))]
pub(crate) const COROUTINE_STACK_SIZE: usize = 0x2000; // 64 KB (release; 32 KB also passes the suite)

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

/// C31 S5c: how long (ms) an entity may stay blocked on an inadmissible mailbox
/// before the interpreter runs wait-for cycle detection. Conservative default so
/// ordinary slow work is not aborted; tests lower it for a fast check.
static DEADLOCK_TIMEOUT_MS: AtomicU64 = AtomicU64::new(500);

pub(crate) fn set_deadlock_timeout_ms(ms: u64) {
    DEADLOCK_TIMEOUT_MS.store(ms, Ordering::Relaxed);
}

pub(crate) fn deadlock_timeout_ms() -> u64 {
    DEADLOCK_TIMEOUT_MS.load(Ordering::Relaxed)
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

    /// Race-safe single-winner resolution: resolve successfully with `v` iff the
    /// future is not yet resolved. Intended for the explicit `Future` Value, which
    /// is freely shareable and may have resolve/reject raced from many threads.
    ///
    /// Ensures: returns `true` iff this call set the result (the winner); `false`
    ///   if the future was already resolved (the existing result is left intact).
    ///   The check-and-set is atomic under `inner`.
    pub fn try_resolve_ok(&self, v: Value) -> bool {
        let mut guard = self.0.inner.lock().unwrap();
        if guard.result.is_some() {
            return false;
        }
        guard.result = Some(Ok(v));
        self.0.resolved.notify_all();
        true
    }

    /// Race-safe single-winner rejection; see [`FutureHandle::try_resolve_ok`].
    ///
    /// Ensures: returns `true` iff this call set the error (the winner); `false`
    ///   if the future was already resolved.
    pub fn try_resolve_err(&self, e: EvalError) -> bool {
        let mut guard = self.0.inner.lock().unwrap();
        if guard.result.is_some() {
            return false;
        }
        guard.result = Some(Err(e));
        self.0.resolved.notify_all();
        true
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
    pub tx_tag: Option<crate::shared_tx::GenTx>,
}

/// Outcome of committing one transaction frame on an entity (see
/// `EntityData::tx_commit_frame`). Tells the caller how to finalize.
pub enum FrameCommitOutcome {
    /// The frame's writes merged into an existing ancestor frame; the entity was
    /// already a participant of that ancestor. Nothing more to do.
    MergedIntoAncestor,
    /// The entity's only frame was retagged to the given parent context (lock
    /// passed up). The caller must enroll the entity as that parent's participant.
    PassedUpTo(TxCtxId),
    /// Root-level commit: shadow applied to live slots, frame popped (entity
    /// unlocked). The durable buffer is handed back for the caller to absorb into
    /// the coordinator's context buffer before the store commit.
    RootCommitted(OwnedStoreTransaction),
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
    /// Speculative shadow stack for multi-entity transactions (C31/C32). Empty
    /// when the entity is in no transaction. Each frame is owned by the context
    /// that pushed it and holds that level's speculative named-ivar writes; the
    /// variant records its concurrency discipline:
    ///   - `Atomic` (`#atomic`) — **locking**: holding it locks the mailbox.
    ///   - `Stm` (`#stm`, C32) — **non-locking** during the body; unrelated
    ///     handlers interleave. Promoted to `Atomic` at commit phase 1.
    ///
    /// The mailbox is locked iff the stack contains any locking (`Atomic`) frame
    /// (see `tx_holder`). Because `#stm` frames don't lock, frames from unrelated
    /// transaction trees can interleave here; the locking (`Atomic`) frames among
    /// them still form a single ancestor→descendant chain (atomic siblings
    /// serialize), which `tx_commit_frame`'s ancestry-aware merge relies on.
    pub tx_frames: Mutex<Vec<TxFrame>>,
    /// C32: monotonic version of this entity's *live* slot state. Bumped on any
    /// live-slot write while an STM transaction is in flight (see
    /// `bump_version_if_stm`). `#stm` transactions snapshot it at first touch and
    /// re-check it at commit; a change means a concurrent handler mutated the
    /// entity underneath the transaction (a conflict). Single-writer: only the
    /// entity's own coroutine writes it. Speculative (shadow) writes never bump it.
    pub version: AtomicU64,
}

/// C32: number of STM transactions currently in flight. While positive, live-slot
/// writes bump the owning entity's `version` so optimistic conflict checks can
/// detect concurrent mutation; at zero the bump is skipped, so entities pay only a
/// single relaxed load per live write outside any STM transaction.
static STM_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Enter an STM transaction: arm version bumping for its lifetime. Paired with
/// [`stm_active_exit`] at commit/abort.
pub(crate) fn stm_active_enter() {
    STM_ACTIVE.fetch_add(1, Ordering::Relaxed);
}

/// Leave an STM transaction: disarm version bumping if this was the last one.
pub(crate) fn stm_active_exit() {
    STM_ACTIVE.fetch_sub(1, Ordering::Relaxed);
}

/// Whether any STM transaction is currently in flight (the version-bump gate).
pub(crate) fn stm_active() -> bool {
    STM_ACTIVE.load(Ordering::Relaxed) != 0
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

    /// Current live-state version. Acquire load so a cross-thread validator that
    /// observes a bump also observes the writes that preceded it.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Bump the live-state version, but only while some STM transaction is in
    /// flight (the `STM_ACTIVE` gate). Single-writer — only the entity's own
    /// coroutine writes its version — so a plain load + `Release` store suffices
    /// (no atomic RMW); the `Release` publishes the new value to cross-thread
    /// conflict validators.
    ///
    /// Requires: called from the entity's own coroutine (single-writer invariant).
    /// Ensures: after a live-slot change, a concurrent `#stm` transaction that
    ///   snapshotted an earlier version will see a different value at commit.
    pub fn bump_version_if_stm(&self) {
        if stm_active() {
            self.version
                .store(self.version.load(Ordering::Relaxed) + 1, Ordering::Release);
        }
    }

    /// `ctx_id` of the context currently *locking* the entity's mailbox, if any.
    ///
    /// Only `#atomic` frames lock (C32); `#stm` frames are skipped, so an entity
    /// held solely by STM transactions reports no holder and admits all messages.
    /// The topmost locking frame is the innermost atomic lock along the ancestry
    /// chain (atomic siblings serialize, so locking frames form a single chain).
    pub fn tx_holder(&self) -> Option<TxCtxId> {
        self.tx_frames
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|f| f.is_locking())
            .map(|f| f.ctx_id())
    }

    /// Whether this entity already holds a frame for `ctx_id`. Used to dedup
    /// enrollment for `#stm` levels, which do not lock and so cannot rely on
    /// `tx_holder` (that reports only the top *locking* frame).
    pub fn has_frame_for(&self, ctx_id: TxCtxId) -> bool {
        self.tx_frames
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.ctx_id() == ctx_id)
    }

    /// Enroll at a new level: push a fresh frame owned by `ctx_id`, opening this
    /// entity's durable buffer under the shared `tx_id`.
    ///
    /// Ensures: durable ops buffered while this frame is on top commit under
    ///   `tx_id`, so the whole transaction tree commits atomically.
    pub fn tx_push_atomic_frame(&self, ctx_id: TxCtxId, tx_id: TxId) {
        let tx = self
            .store
            .begin_owned_with_id(ExecContext::Entity(self.id), tx_id);
        self.tx_frames
            .lock()
            .unwrap()
            .push(TxFrame::Atomic(AtomicFrame {
                ctx_id,
                shadow: HashMap::new(),
                tx,
            }));
    }

    /// Enroll at a new `#stm` level (C32): push a non-locking `Stm` frame that
    /// snapshots the entity's current `version` (this transaction's first touch of
    /// the entity). The frame does not lock the mailbox, so unrelated handlers
    /// keep running; the snapshot is re-checked at the two-phase commit.
    ///
    /// Ensures: the frame's `snapshot_version` equals the entity's version at
    ///   enrollment; durable ops buffered while it is on top commit under `tx_id`.
    pub fn tx_push_stm_frame(&self, ctx_id: TxCtxId, tx_id: TxId) {
        let snapshot_version = self.version();
        let tx = self
            .store
            .begin_owned_with_id(ExecContext::Entity(self.id), tx_id);
        self.tx_frames.lock().unwrap().push(TxFrame::Stm(StmFrame {
            ctx_id,
            shadow: HashMap::new(),
            tx,
            snapshot_version,
        }));
    }

    /// Phase 1 of the STM two-phase commit (C32): try-lock and validate this
    /// participant for context `ctx_id`.
    ///
    /// - **Try-lock:** if another context already locks the mailbox, vote abort
    ///   (returns `false`) rather than blocking — so commit can never deadlock.
    /// - **Validate:** compare the entity's live `version` to the snapshot the
    ///   `#stm` frame captured at enrollment; a change means a concurrent handler
    ///   mutated the entity, so vote abort.
    /// - **On a commit vote:** promote the `#stm` frame to a locking `#atomic`
    ///   frame, so the mailbox stays locked through phase 2 (apply) — closing the
    ///   validate→apply TOCTOU window.
    ///
    /// Ensures: returns `true` iff the participant is now locked-and-validated for
    ///   `ctx_id` (caller must drive phase 2 commit/release); `false` leaves the
    ///   frame untouched (caller aborts the whole transaction).
    pub fn stm_try_prepare(&self, ctx_id: TxCtxId) -> bool {
        let mut frames = self.tx_frames.lock().unwrap();
        // Try-lock: any *other* context holding a lock means we cannot proceed.
        let locked_by_other = frames
            .iter()
            .any(|f| f.is_locking() && f.ctx_id() != ctx_id);
        if locked_by_other {
            return false;
        }
        let Some(i) = frames.iter().position(|f| f.ctx_id() == ctx_id) else {
            return false; // defensive: no frame for this context
        };
        // Validate: live version must match the enrollment snapshot.
        if frames[i].snapshot_version() != Some(self.version()) {
            return false;
        }
        // Promote to a locking frame, preserving stack position.
        let frame = frames.remove(i);
        frames.insert(i, frame.into_locking());
        true
    }

    /// Run `f` with the durable buffer of the frame owned by `ctx_id` (C32:
    /// ctx-aware — routes durable ops to the tx the write is made *under*, not the
    /// stack top, which under interleaving may belong to another tx).
    ///
    /// Requires: the entity holds a frame for `ctx_id` (`has_frame_for`). Used by
    ///   `store_bridge` to route durable ops to the active level's buffer instead
    ///   of the private `current_tx`.
    pub fn with_frame_tx_for<R>(
        &self,
        ctx_id: TxCtxId,
        f: impl FnOnce(&mut OwnedStoreTransaction) -> R,
    ) -> R {
        let mut frames = self.tx_frames.lock().unwrap();
        let frame = frames
            .iter_mut()
            .find(|f2| f2.ctx_id() == ctx_id)
            .expect("with_frame_tx_for: entity has no frame for ctx");
        f(frame.tx_mut())
    }

    /// Speculatively write a named ivar into the frame owned by `ctx_id` (C32:
    /// ctx-aware — the write is directed to the *transaction it is made under*, not
    /// the top of the stack, which under interleaving may belong to another tx).
    ///
    /// Requires: the entity holds a frame for `ctx_id` (`has_frame_for`).
    pub fn tx_shadow_set_for(&self, ctx_id: TxCtxId, name: Arc<str>, value: Value) {
        let mut frames = self.tx_frames.lock().unwrap();
        let frame = frames
            .iter_mut()
            .find(|f| f.ctx_id() == ctx_id)
            .expect("tx_shadow_set_for: entity has no frame for ctx");
        frame.shadow_set(name, value);
    }

    /// Read a named ivar from the frame owned by `ctx_id`, if that frame holds a
    /// speculative value for it (C32: ctx-aware — consulted only for frames in the
    /// current transaction's ancestry, never unrelated interleaved frames).
    ///
    /// Ensures: `Some(v)` iff that ctx's frame has a speculative value for `name`.
    ///   The caller walks the ancestry chain (nested read-your-writes) and falls
    ///   through to the live slot on no hit.
    pub fn tx_shadow_get_for(&self, ctx_id: TxCtxId, name: &str) -> Option<Value> {
        self.tx_frames
            .lock()
            .unwrap()
            .iter()
            .find(|f| f.ctx_id() == ctx_id)
            .and_then(|f| f.shadow_get(name).cloned())
    }

    /// Commit the top transaction level, merging its speculative writes into the
    /// committing context's *genuine* parent. The returned `FrameCommitOutcome`
    /// tells the caller what happened so it can finalize correctly (see variants).
    ///
    /// `parent_ctx_id`:
    ///   - `None` → committing the root level for this entity.
    ///   - `Some(parent)` → committing a child level, `parent` = the enclosing
    ///     context's id.
    ///
    /// Ancestry-aware (C32 S1): the merge target is chosen by *ancestry*, not by
    /// stack adjacency. Because `#stm` frames do not lock the mailbox, an
    /// unrelated frame from an interleaved transaction may sit between this frame
    /// and its parent (or below a root frame). We therefore:
    ///   - merge into the parent frame wherever it sits in the stack (searching
    ///     downward), never into an unrelated interleaved frame;
    ///   - for a root commit, apply to live slots regardless of any unrelated
    ///     frame below — an unrelated frame is *not* an ancestor to merge into.
    ///
    /// Applying a root commit to live slots bumps the entity's version (C32 S2),
    /// which correctly conflicts any interleaved `#stm` transaction that read it.
    /// For atomic-only stacks the parent is always directly below, so behavior is
    /// identical to the pre-C32 adjacency rule.
    ///
    /// `ctx_id` is the committing level; `parent_ctx_id` its enclosing context
    /// (`None` at the root). The frame is located by `ctx_id` (not the stack top),
    /// so nested/interleaved commits target the right level.
    ///
    /// Requires: the entity holds a frame for `ctx_id`.
    pub fn tx_commit_frame(
        &self,
        ctx_id: TxCtxId,
        parent_ctx_id: Option<TxCtxId>,
    ) -> FrameCommitOutcome {
        let mut frames = self.tx_frames.lock().unwrap();
        let i = frames
            .iter()
            .position(|f| f.ctx_id() == ctx_id)
            .expect("tx_commit_frame: entity has no frame for ctx");
        let (_top_ctx, top_shadow, top_tx) = frames.remove(i).into_parts();
        match parent_ctx_id {
            // Child level: merge into the genuine parent frame wherever it sits
            // (an interleaved unrelated frame may lie above it). Child writes
            // supersede the parent's for shared keys (read-your-writes) and
            // durable ops merge up; the entity is already the parent's participant.
            Some(parent) => {
                if let Some(pf) = frames.iter_mut().rev().find(|f| f.ctx_id() == parent) {
                    pf.absorb_child(top_shadow, top_tx);
                    FrameCommitOutcome::MergedIntoAncestor
                } else {
                    // Parent never separately enrolled this entity: retag this
                    // frame to the parent and pass the lock up (reinserting at the
                    // committing frame's position to preserve stack ordering). The
                    // caller must enroll it as the parent's participant.
                    frames.insert(
                        i,
                        TxFrame::Atomic(AtomicFrame {
                            ctx_id: parent,
                            shadow: top_shadow,
                            tx: top_tx,
                        }),
                    );
                    FrameCommitOutcome::PassedUpTo(parent)
                }
            }
            // Root level for this entity: speculative writes become live; hand the
            // durable buffer to the caller to absorb into the context buffer. Never
            // merge into an unrelated interleaved frame that may sit below.
            None => {
                let wrote = !top_shadow.is_empty();
                for (name, value) in top_shadow {
                    self.data.set_slot(name, value);
                }
                // Applying speculative writes to live slots is a live-state change
                // (C32): bump the version once (gated) so a concurrent STM
                // transaction that read this entity conflicts at its commit check.
                if wrote {
                    self.bump_version_if_stm();
                }
                FrameCommitOutcome::RootCommitted(top_tx)
            }
        }
    }

    /// Abort the level owned by `ctx_id`: remove and discard its speculative
    /// writes and durable buffer (dropping the buffer rolls back its ops — they
    /// were never absorbed into the context, so they never commit). Located by
    /// `ctx_id` (not the stack top) for nested/interleaved correctness. Live slots
    /// are untouched.
    ///
    /// Requires: the entity holds a frame for `ctx_id`.
    pub fn tx_abort_frame(&self, ctx_id: TxCtxId) {
        let mut frames = self.tx_frames.lock().unwrap();
        let i = frames
            .iter()
            .position(|f| f.ctx_id() == ctx_id)
            .expect("tx_abort_frame: entity has no frame for ctx");
        frames.remove(i);
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
        version: AtomicU64::new(0),
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
            version: AtomicU64::new(0),
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
    tx_tag: Option<crate::shared_tx::GenTx>,
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

/// C31 S5c: schedule a repeating deadlock check while an entity stays blocked.
///
/// One-shot OS timers can't cover a *silent* deadlock (no condvar wakes to
/// re-arm on), so the check re-schedules itself after each run. The chain is
/// gated by `active`: the blocked entity's loop clears it the instant it admits a
/// message (or shuts down), so the next callback stops. The detection body runs
/// in a `may` coroutine (it takes coroutine-aware mailbox locks).
fn arm_deadlock_check(
    os: Arc<dyn crate::os_interface::OsInterface>,
    anchor: Arc<InstanceState>,
    reg: Arc<ClassRegistry>,
    active: Arc<std::sync::atomic::AtomicBool>,
    ms: u64,
) {
    let os_next = os.clone();
    let cb: Box<dyn FnOnce() + Send> = Box::new(move || {
        if !active.load(Ordering::Relaxed) {
            return;
        }
        may::go!(move || {
            if !active.load(Ordering::Relaxed) {
                return;
            }
            detect_and_resolve_deadlock(&anchor, &reg);
            if active.load(Ordering::Relaxed) {
                arm_deadlock_check(os_next, anchor, reg, active, ms);
            }
        });
    });
    os.schedule_timer(ms, cb);
}

/// C31 S5c: detect a transaction deadlock (a cycle in the wait-for graph) and, if
/// found, break it by aborting one victim.
///
/// The graph is built by traversing *transactions*, not by scanning the entity
/// registry (see [[project_tx_design]]). It is anchored on the blocked `anchor`
/// entity whose check fired: the transactions parked in its mailbox
/// (`envelope.tx_tag` is a `GenTx`) seed a worklist, and each
/// transaction's `participants()` — the entities it holds — are inspected for
/// their own parked waiters, enqueuing those transactions in turn. So only the
/// transactions and entities in the wait-for component are visited.
///
/// An entity locked to holder ctx `H` with a parked message tagged ctx `W` (i.e.
/// `W` is *not* admissible under `H`) contributes an edge `W → H`. Nodes are
/// `TxCtxId`s, so sibling children can deadlock. The holder tx of `anchor` is
/// discovered as a waiter parked on some other transaction's participant, so a
/// cycle through it still closes.
///
/// On detecting a cycle, one member's parked message is rejected with a `Deadlock`
/// condition and removed from its entity's mailbox; the victim coordinator's
/// `await` then raises `Deadlock`, its `on: Error do:` handler rolls back and
/// releases its locks, and the rest of the cycle proceeds.
///
/// Ensures: returns `true` iff a cycle was found and a victim was aborted. Locks
/// at most one entity mailbox at a time, so the detector cannot itself deadlock.
pub(crate) fn detect_and_resolve_deadlock(
    anchor: &Arc<InstanceState>,
    registry: &ClassRegistry,
) -> bool {
    // Serialize detection+resolution across all blocked entities' timers. Each
    // waiting entity arms its own repeating check, so for a single cycle multiple
    // timers fire ~simultaneously; without this lock two runs can independently
    // select and abort *different* victims (HashMap/HashSet iteration is
    // nondeterministic), collapsing the whole cycle instead of breaking it once.
    // Holding this lock, the first run rejects one victim — synchronously removing
    // its parked message (`reject_parked_message_for_ctx`) — so any subsequent run
    // rebuilds an acyclic graph and finds nothing. Acquired before any mailbox
    // lock and taken nowhere else, so it introduces no lock-ordering cycle.
    static RESOLVE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _resolve_guard = RESOLVE.lock().unwrap();

    // waiter ctx → the holder ctxs it is blocked behind.
    let mut edges: HashMap<TxCtxId, HashSet<TxCtxId>> = HashMap::new();
    // waiter ctx → an entity where its message is parked (for victim rejection).
    let mut target_entity: HashMap<TxCtxId, Arc<InstanceState>> = HashMap::new();
    // Transactions still to inspect; `visited` dedupes by ctx_id.
    let mut worklist: Vec<GenTx> = Vec::new();
    let mut visited: HashSet<TxCtxId> = HashSet::new();

    // Seed from the anchor's own parked waiters.
    collect_entity_edges(anchor, &mut edges, &mut target_entity, &mut worklist);

    while let Some(tx) = worklist.pop() {
        if !visited.insert(tx.ctx_id()) {
            continue;
        }
        let participants: Vec<Arc<InstanceState>> = tx
            .participants()
            .lock()
            .unwrap()
            .iter()
            .map(|h| h.0.clone())
            .collect();
        for st in &participants {
            collect_entity_edges(st, &mut edges, &mut target_entity, &mut worklist);
        }
    }

    let Some(victim) = find_cycle_victim(&edges) else {
        return false;
    };
    match target_entity.get(&victim) {
        Some(st) => reject_parked_message_for_ctx(st, victim, registry),
        None => false,
    }
}

/// Add the wait-for edges contributed by one entity `st`: for each message parked
/// in its mailbox (tagged with a transaction that is *not* admissible under the
/// entity's current holder), record `waiter → holder`, remember where the waiter's
/// message is parked (for rejection), and enqueue the waiter transaction so its
/// own participants are traversed. No-op for a value object or an entity not
/// currently holding a lock.
fn collect_entity_edges(
    st: &Arc<InstanceState>,
    edges: &mut HashMap<TxCtxId, HashSet<TxCtxId>>,
    target_entity: &mut HashMap<TxCtxId, Arc<InstanceState>>,
    worklist: &mut Vec<GenTx>,
) {
    let data = match &st.kind {
        InstanceKind::Entity(d) | InstanceKind::ClassEntity(d) => d.as_ref(),
        InstanceKind::ValueObject(_) => return,
    };
    let Some(holder) = data.tx_holder() else {
        return;
    };
    let mb = data.mailbox.lock().unwrap();
    for env in mb.iter() {
        if let Some(tag) = &env.tx_tag
            && !tag.has_ancestor_or_self(holder)
        {
            let w = tag.ctx_id();
            edges.entry(w).or_default().insert(holder);
            target_entity.entry(w).or_insert_with(|| st.clone());
            worklist.push(tag.clone());
        }
    }
}

/// Return a `TxCtxId` lying on a directed cycle of `edges`, or `None` if the graph
/// is acyclic. Standard three-colour DFS; the node closing a back edge is on the
/// cycle (and, being a graph key, is a waiter with a parked message to reject).
/// Graphs here are tiny (one node per blocked tx context), so recursion is safe.
fn find_cycle_victim(edges: &HashMap<TxCtxId, HashSet<TxCtxId>>) -> Option<TxCtxId> {
    // 0 = white (unvisited), 1 = grey (on stack), 2 = black (done).
    fn dfs(
        node: TxCtxId,
        edges: &HashMap<TxCtxId, HashSet<TxCtxId>>,
        color: &mut HashMap<TxCtxId, u8>,
    ) -> Option<TxCtxId> {
        color.insert(node, 1);
        if let Some(neighbors) = edges.get(&node) {
            for &next in neighbors {
                match color.get(&next).copied().unwrap_or(0) {
                    0 => {
                        if let Some(v) = dfs(next, edges, color) {
                            return Some(v);
                        }
                    }
                    1 => return Some(next), // back edge → `next` is on the cycle
                    _ => {}
                }
            }
        }
        color.insert(node, 2);
        None
    }

    let mut color: HashMap<TxCtxId, u8> = HashMap::new();
    let starts: Vec<TxCtxId> = edges.keys().copied().collect();
    for start in starts {
        if color.get(&start).copied().unwrap_or(0) == 0
            && let Some(v) = dfs(start, edges, &mut color)
        {
            return Some(v);
        }
    }
    None
}

/// Reject and remove the message tagged with `victim_ctx` parked in `st`'s
/// mailbox, resolving its reply with a `Deadlock` condition. Returns `true` iff a
/// matching message was found (a concurrent change may have already cleared it).
fn reject_parked_message_for_ctx(
    st: &Arc<InstanceState>,
    victim_ctx: TxCtxId,
    registry: &ClassRegistry,
) -> bool {
    let data = match &st.kind {
        InstanceKind::Entity(d) | InstanceKind::ClassEntity(d) => d.as_ref(),
        InstanceKind::ValueObject(_) => return false,
    };
    let mut mb = data.mailbox.lock().unwrap();
    let Some(pos) = mb
        .iter()
        .position(|e| e.tx_tag.as_ref().is_some_and(|t| t.ctx_id() == victim_ctx))
    else {
        return false;
    };
    let env = mb.remove(pos).expect("position is valid");
    drop(mb);
    let err = crate::builtins::make_condition_value(
        registry,
        "Deadlock",
        "transaction deadlock detected",
    );
    env.reply.try_resolve_err(EvalError::Signal(err));
    data.mailbox_ready.notify_all();
    true
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

    // C31 S5b / C32: a transaction coordinator (any entity that is-a `Transaction`)
    // must NOT enroll as a participant of the tx it runs — it writes its own
    // `status`/`handle` ivars under the tag. That exclusion now lives in
    // `Evaluator::ensure_tx_enrolled` (lazy enrollment), keyed on Transaction
    // identity, alongside the born-in-tx exclusion. Tag propagation still happens
    // below so nested coordinators see the outer ctx.

    let mut env = global_env;

    loop {
        // Wait for an envelope (or shutdown signal).
        let envelope = {
            let mut mb = data.mailbox.lock().unwrap();
            // C31 S5c: while blocked behind a lock (messages present but none
            // admissible), run global wait-for cycle detection periodically. A
            // genuine deadlock produces no notifications, so the check must
            // re-arm itself rather than wait for a wake — a self-rescheduling
            // timer chain gated by `dl_active`, which we clear the moment we
            // admit a message (or shut down), stopping the chain.
            let mut dl_active: Option<Arc<AtomicBool>> = None;
            loop {
                if data.shutdown.load(Ordering::Acquire) {
                    if let Some(a) = dl_active.take() {
                        a.store(false, Ordering::Relaxed);
                    }
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
                    if let Some(a) = dl_active.take() {
                        a.store(false, Ordering::Relaxed);
                    }
                    break mb
                        .remove(i)
                        .expect("admissible_index returns a valid index");
                }
                if dl_active.is_none() && !mb.is_empty() {
                    let active = Arc::new(AtomicBool::new(true));
                    arm_deadlock_check(
                        ev.os.clone(),
                        state.clone(),
                        ev.registry.clone(),
                        active.clone(),
                        deadlock_timeout_ms(),
                    );
                    dl_active = Some(active);
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

        // C32: enrollment is *lazy* — an entity becomes a participant of a tagged
        // transaction on first access to its own transactional state (ivar
        // read/write), handled in `Evaluator::ensure_tx_enrolled` via
        // `assign_var`/`lookup_var`. Merely receiving a tagged message no longer
        // enrolls (that over-enrolled class entities and pure-computation
        // helpers). The tag is still propagated below so writes route to the frame
        // buffer and outgoing sends inherit it. See
        // `docs/enrollment_provenance_design.md`.
        let tx_tag = envelope.tx_tag.clone();

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
    // On a primitive miss, route through `doesNotUnderstand:` (reified-message
    // forwarding) if the class defines one. This is the entity/actor counterpart
    // of the synchronous DNU hook in `dispatch_to_stack`: an incoming mailbox
    // message the entity does not understand is reified and handled in Smol. The
    // default `Object >> doesNotUnderstand:` signals a `MessageNotUnderstood`
    // condition, so an unhandled miss still surfaces as an error.
    let dnu = handle.0.class.lookup("doesNotUnderstand:");
    if dnu.is_none() {
        return crate::builtins::dispatch_object_primitive(ev, handle, selector, args, env);
    }
    match crate::builtins::dispatch_object_primitive(
        ev,
        handle.clone(),
        selector,
        args.clone(),
        env,
    ) {
        Err(crate::eval::EvalError::MessageNotUnderstood { .. }) => {
            let (dnu_method, dnu_class) = dnu.expect("dnu is Some");
            let msg = crate::builtins::build_message(ev, selector.clone(), args)?;
            ev.invoke_method(handle, dnu_class, dnu_method, vec![msg], env)
        }
        other => other,
    }
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
