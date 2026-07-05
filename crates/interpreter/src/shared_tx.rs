//! Transaction context types for multi-entity atomic transactions (C31).
//!
//! A `TxContext` is the coordinator's view of an in-flight transaction at one
//! nesting level. The root level holds the shared store `TxId`; each nested
//! `atomic:` call creates a `Child` that links to its parent via `outer` and
//! owns its own op buffer. The whole tree commits under the root's `TxId`, so a
//! child commit merges (absorbs) its buffer into its parent and a child abort
//! simply drops its own buffer — independently of any sibling.
//!
//! The handle is opaque to Smol: it is carried as `Value::Handle(Arc<GenTx>)`
//! and downcast by the primitives that created it. There is deliberately no new
//! `Value` variant.
//!
//! See `docs/tx_design.md` and `docs/chunk_31_plan.md` (S1) for the full design.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use may::sync::Mutex;

use crate::entity::InstanceHandle;
use crate::value::Value;
use triple_store::id::{EntityId, TxId};
use triple_store::store::OwnedStoreTransaction;

/// Identifier for the shared store transaction (equal to `TxId.0`).
pub type SharedTxId = u64;

/// Process-unique identity for one transaction context (root or child). Used for
/// lock admission (ancestry checks) and as the node key in deadlock detection.
pub type TxCtxId = u64;

static NEXT_CTX_ID: AtomicU64 = AtomicU64::new(1);

/// Mint a fresh, process-unique `TxCtxId`.
pub(crate) fn next_ctx_id() -> TxCtxId {
    NEXT_CTX_ID.fetch_add(1, Ordering::Relaxed)
}

/// Concurrency discipline of a transaction tree (C32). Set at the root and shared
/// by every child level; decides how participants enroll and commit.
///
///   - `Atomic` — pessimistic (`#atomic`, C31): enrollment locks the participant's
///     mailbox; no unrelated handler interleaves; commit applies shadow directly.
///   - `Stm` — optimistic (`#stm`, C32): enrollment does *not* lock; unrelated
///     handlers interleave; each participant snapshots its version at enrollment
///     and a two-phase commit validates versions before applying.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxMode {
    Atomic,
    Stm,
}

/// State common to every transaction level, regardless of discipline. A level is
/// a *root* iff its owner's `parent` is `None`, else a nested child; either way it
/// owns its own op/value buffer and participant list (children merge up on commit,
/// drop on abort — independently of siblings).
pub struct TxCommon {
    pub ctx_id: TxCtxId,
    /// The shared store `TxId` for the whole tree. Copied into every level (equal
    /// throughout a tree) so `tx_id()` is O(1) with no walk to the root.
    pub tx_id: TxId,
    /// This level's op/value buffer. `None` after commit or rollback.
    pub tx: Mutex<Option<OwnedStoreTransaction>>,
    /// Participants enrolled at this level (in enrollment order).
    pub participants: Mutex<Vec<InstanceHandle>>,
    /// Ids of entities *created* under this level (born-in-tx). Such an entity is
    /// private to this level — it has no committed baseline and is unreachable by
    /// any other transaction — so on first state access it is *excluded* from
    /// enrollment (writes go live) rather than shadowed/validated. Holds bare
    /// `EntityId`s (not handles) so it never pins a transient alive. Ephemeral:
    /// dropped with the level. See `docs/enrollment_provenance_design.md`.
    pub spawned: Mutex<HashSet<EntityId>>,
}

/// An optimistic (`#stm`) transaction level. Its `parent` is typed
/// `Option<Arc<StmTx>>` — it can *only* be another `StmTx` (or none) — which makes
/// the illegal "`#stm` nested under `#atomic`" state **unrepresentable**: the
/// contagious-atomic rule (any atomic ancestor forces atomic) is enforced by the
/// type system rather than a runtime invariant. `GenTx::begin` is the sole
/// constructor and applies the coercion.
pub struct StmTx {
    pub common: TxCommon,
    pub parent: Option<Arc<StmTx>>,
}

/// A pessimistic (`#atomic`) transaction level. Its `parent` may be either
/// discipline (`Option<GenTx>`): atomic nests freely under anything.
pub struct AtomicTx {
    pub common: TxCommon,
    pub parent: Option<GenTx>,
}

/// Coordinator handle for one in-flight transaction level. The variant *is* the
/// level's effective concurrency discipline (`discipline()`), so no mode field or
/// ancestry walk is needed. Root vs. child is `parent().is_none()`.
///
/// Carried as `Value::Handle(Arc<GenTx>)`; opaque to Smol.
#[derive(Clone)]
pub enum GenTx {
    Stm(Arc<StmTx>),
    Atomic(Arc<AtomicTx>),
}

/// One frame of an entity's transaction stack. Each frame is owned by the
/// context that pushed it and holds that level's speculative state for this
/// entity:
///   - `shadow` — in-memory named-ivar writes (mask the live slot; applied on
///     commit, discarded on abort);
///   - `tx` — durable store ops, buffered under the shared `TxId`. On commit it
///     merges into the frame below (or is handed to the coordinator's context
///     buffer at the last frame); on abort it is dropped (its ops never commit).
///
/// Keeping both in one frame makes the per-level push/merge/discard atomic — the
/// in-memory and durable speculative state can never desync.
///
/// The variant records the *concurrency discipline* of the level:
///   - `Atomic` — pessimistic (`#atomic`): pushing the frame locks the entity's
///     mailbox (see `admissible_index`), so no unrelated handler interleaves.
///   - `Stm` — optimistic (`#stm`, C32): the frame does *not* lock the mailbox;
///     unrelated handlers interleave freely and a version check at commit
///     detects conflicts. (Added in C32 S3.)
pub enum TxFrame {
    Atomic(AtomicFrame),
    Stm(StmFrame),
}

/// A pessimistic (`#atomic`) transaction frame: locking + shadow.
pub struct AtomicFrame {
    pub ctx_id: TxCtxId,
    pub shadow: HashMap<Arc<str>, Value>,
    pub tx: OwnedStoreTransaction,
}

/// An optimistic (`#stm`, C32) transaction frame: shadow + version snapshot.
/// Unlike `AtomicFrame`, it does *not* lock the mailbox **during the transaction
/// body** — unrelated handlers interleave freely. At commit phase 1 the frame is
/// promoted to a locking `AtomicFrame` (see `TxFrame::into_locking`) so the
/// validated state is held through phase-2 apply. `snapshot_version` is the
/// entity's `version` captured at enrollment (the transaction's first touch of
/// this entity); the two-phase commit re-checks it before promoting.
pub struct StmFrame {
    pub ctx_id: TxCtxId,
    pub shadow: HashMap<Arc<str>, Value>,
    pub tx: OwnedStoreTransaction,
    pub snapshot_version: u64,
}

impl TxFrame {
    /// The context that owns this frame.
    pub fn ctx_id(&self) -> TxCtxId {
        match self {
            TxFrame::Atomic(f) => f.ctx_id,
            TxFrame::Stm(f) => f.ctx_id,
        }
    }

    /// Whether holding this frame locks the entity's mailbox. `#atomic` frames
    /// lock; `#stm` frames do not (unrelated handlers interleave freely).
    pub fn is_locking(&self) -> bool {
        match self {
            TxFrame::Atomic(_) => true,
            TxFrame::Stm(_) => false,
        }
    }

    /// Read a speculative ivar value from this frame, if present.
    pub fn shadow_get(&self, name: &str) -> Option<&Value> {
        match self {
            TxFrame::Atomic(f) => f.shadow.get(name),
            TxFrame::Stm(f) => f.shadow.get(name),
        }
    }

    /// Insert a speculative ivar value into this frame.
    pub fn shadow_set(&mut self, name: Arc<str>, value: Value) {
        match self {
            TxFrame::Atomic(f) => {
                f.shadow.insert(name, value);
            }
            TxFrame::Stm(f) => {
                f.shadow.insert(name, value);
            }
        }
    }

    /// This frame's durable op buffer (mutable). Used by `store_bridge` to route
    /// durable ops to the active level.
    pub fn tx_mut(&mut self) -> &mut OwnedStoreTransaction {
        match self {
            TxFrame::Atomic(f) => &mut f.tx,
            TxFrame::Stm(f) => &mut f.tx,
        }
    }

    /// Merge a child frame's speculative writes into this (parent) frame: the
    /// child's shadow supersedes this frame's for shared keys (read-your-writes),
    /// and the child's durable ops are absorbed.
    pub fn absorb_child(
        &mut self,
        child_shadow: HashMap<Arc<str>, Value>,
        child_tx: OwnedStoreTransaction,
    ) {
        match self {
            TxFrame::Atomic(f) => {
                f.shadow.extend(child_shadow);
                f.tx.absorb(child_tx);
            }
            TxFrame::Stm(f) => {
                f.shadow.extend(child_shadow);
                f.tx.absorb(child_tx);
            }
        }
    }

    /// This frame's version snapshot, if it is an `#stm` frame (the entity's
    /// version at enrollment). `None` for `#atomic` frames, which do not validate.
    pub fn snapshot_version(&self) -> Option<u64> {
        match self {
            TxFrame::Atomic(_) => None,
            TxFrame::Stm(f) => Some(f.snapshot_version),
        }
    }

    /// Promote an `#stm` frame to a locking `#atomic` frame (C32 commit phase 1),
    /// preserving its shadow and durable buffer. Once promoted, holding the frame
    /// locks the mailbox, so the validated state cannot change before phase 2
    /// applies it. A no-op for an already-locking `#atomic` frame.
    pub fn into_locking(self) -> TxFrame {
        match self {
            TxFrame::Stm(f) => TxFrame::Atomic(AtomicFrame {
                ctx_id: f.ctx_id,
                shadow: f.shadow,
                tx: f.tx,
            }),
            atomic => atomic,
        }
    }

    /// Consume the frame into its common parts (`ctx_id`, shadow, durable buffer),
    /// discarding any variant-specific state. Used by commit paths that move the
    /// frame's speculative state up to a parent or out to the coordinator.
    pub fn into_parts(self) -> (TxCtxId, HashMap<Arc<str>, Value>, OwnedStoreTransaction) {
        match self {
            TxFrame::Atomic(f) => (f.ctx_id, f.shadow, f.tx),
            TxFrame::Stm(f) => (f.ctx_id, f.shadow, f.tx),
        }
    }
}

impl GenTx {
    /// Sole constructor. Enforces **contagious-atomic**: a requested `#stm` level
    /// whose parent is an `AtomicTx` is coerced to atomic — the type of
    /// `StmTx::parent` makes any other outcome unrepresentable. Mints a fresh
    /// `ctx_id` and takes ownership of this level's durable buffer.
    ///
    /// Ensures: `discipline() == Stm` ⟹ every ancestor is also `Stm`.
    pub fn begin(
        requested: TxMode,
        tx_id: TxId,
        tx: OwnedStoreTransaction,
        parent: Option<GenTx>,
    ) -> GenTx {
        let common = TxCommon {
            ctx_id: next_ctx_id(),
            tx_id,
            tx: Mutex::new(Some(tx)),
            participants: Mutex::new(Vec::new()),
            spawned: Mutex::new(HashSet::new()),
        };
        match (requested, parent) {
            (TxMode::Stm, None) => GenTx::Stm(Arc::new(StmTx {
                common,
                parent: None,
            })),
            (TxMode::Stm, Some(GenTx::Stm(p))) => GenTx::Stm(Arc::new(StmTx {
                common,
                parent: Some(p),
            })),
            // Coercion: #stm under an #atomic ancestor becomes #atomic.
            (TxMode::Stm, Some(atomic_parent @ GenTx::Atomic(_))) => {
                GenTx::Atomic(Arc::new(AtomicTx {
                    common,
                    parent: Some(atomic_parent),
                }))
            }
            (TxMode::Atomic, parent) => GenTx::Atomic(Arc::new(AtomicTx { common, parent })),
        }
    }

    /// State shared by every discipline.
    pub fn common(&self) -> &TxCommon {
        match self {
            GenTx::Stm(s) => &s.common,
            GenTx::Atomic(a) => &a.common,
        }
    }

    /// This level's own identity.
    pub fn ctx_id(&self) -> TxCtxId {
        self.common().ctx_id
    }

    /// The shared store `TxId` for the whole transaction tree.
    pub fn tx_id(&self) -> TxId {
        self.common().tx_id
    }

    /// This level's effective concurrency discipline — the variant itself
    /// (contagious-atomic already applied at construction).
    pub fn discipline(&self) -> TxMode {
        match self {
            GenTx::Stm(_) => TxMode::Stm,
            GenTx::Atomic(_) => TxMode::Atomic,
        }
    }

    /// This level's op/value buffer slot.
    pub fn buffer(&self) -> &Mutex<Option<OwnedStoreTransaction>> {
        &self.common().tx
    }

    /// This level's participant list.
    pub fn participants(&self) -> &Mutex<Vec<InstanceHandle>> {
        &self.common().participants
    }

    /// Ids of entities born under this level (excluded from enrollment).
    pub fn spawned(&self) -> &Mutex<HashSet<EntityId>> {
        &self.common().spawned
    }

    /// The enclosing level, lifted to `GenTx` (an `StmTx` parent becomes
    /// `GenTx::Stm`). `None` at the root.
    pub fn parent(&self) -> Option<GenTx> {
        match self {
            GenTx::Stm(s) => s.parent.clone().map(GenTx::Stm),
            GenTx::Atomic(a) => a.parent.clone(),
        }
    }

    /// Whether this level is a tree root (no enclosing level).
    pub fn is_root(&self) -> bool {
        self.parent().is_none()
    }

    /// Whether `ctx_id` names this level or any of its ancestors.
    ///
    /// Ensures: `true` iff walking `self → parent → … → root` encounters `ctx_id`.
    ///   Used for lock admission: a tagged message from level `self` is admitted by
    ///   an entity whose lock holder is `ctx_id` exactly when this returns `true`
    ///   (the holder is an ancestor-or-equal of the requester).
    pub fn has_ancestor_or_self(&self, ctx_id: TxCtxId) -> bool {
        let mut cur = self.clone();
        loop {
            if cur.ctx_id() == ctx_id {
                return true;
            }
            match cur.parent() {
                Some(p) => cur = p,
                None => return false,
            }
        }
    }
}

impl std::fmt::Debug for GenTx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GenTx::{:?}(ctx_id={}, tx_id={:?}, root={})",
            self.discipline(),
            self.ctx_id(),
            self.tx_id(),
            self.is_root()
        )
    }
}
