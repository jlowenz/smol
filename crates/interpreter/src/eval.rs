use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use crate::entity::{EntityId, InstanceHandle, InstanceKind, InstanceState};
use crate::eval_iterative::{
    Cont, push_block_frame, push_method_call_frame, push_method_frame, step_loop,
};
use triple_store::store::{OwnedStoreTransaction, Store};

use thiserror::Error;

use crate::ast::*;
use crate::class::{Class, Method};
use crate::class_registry::ClassRegistry;
use crate::env::Env;
use crate::namespace::Namespace;
use crate::object::{IndexedStorage, Object};
use crate::os_interface::{DefaultOsInterface, OsInterface};
use crate::parser::Parser;
use crate::value::{Block, Value};

#[derive(Debug, Error, Clone, PartialEq)]
pub enum EvalError {
    #[error("{receiver_kind} does not understand #{selector}")]
    MessageNotUnderstood {
        receiver_kind: &'static str,
        selector: Arc<str>,
    },
    #[error("undefined variable: {0}")]
    UndefinedVariable(Arc<str>),
    #[error("wrong number of arguments: expected {expected}, got {got}")]
    ArityMismatch { expected: usize, got: usize },
    #[error("type error: {0}")]
    TypeError(String),
    /// Smol-raised signal. Carries the condition object (a Condition instance).
    /// `on:do:` catches this by class hierarchy matching.
    #[error("signal")]
    Signal(Value),
    /// Non-local `^`-return carrying the value and the method-stack depth of the
    /// frame that should catch it. Only the `invoke_method` / `invoke_class_method`
    /// whose depth matches will unwrap this; all others propagate it unchanged.
    #[error("non-local return")]
    Return(Value, usize),
    /// `resume: v` from a condition handler — delivers `v` back to the `_primSignalCondition:`
    /// call site. Only meaningful from non-unwinding (HandlerContext) handlers; from
    /// unwinding `on:do:` handlers this is equivalent to `return:`.
    #[error("resume")]
    Resume(Value),
    /// `return: v` from a condition handler — makes the `on:do:` or `_primHCRun:` expression
    /// return `v` immediately.
    #[error("return from handler")]
    ReturnFromBlock(Value),
    /// `retry` from a condition handler — re-executes the protected body.
    #[error("retry")]
    Retry,
}

/// The self-binding for the currently-executing method frame.
#[derive(Clone)]
pub enum SelfBinding {
    Instance {
        handle: InstanceHandle,
        defining_class: Arc<Class>,
    },
    ClassSide {
        handle: InstanceHandle,
        defining_class: Arc<Class>,
    },
    /// Method invoked on a primitive receiver (e.g. `nil`). No `InstanceHandle`;
    /// `self_value` is bound directly as `self` in the method frame.
    Primitive {
        defining_class: Arc<Class>,
        self_value: Value,
    },
}

// Manual `Debug` (the variant name only): `Class` is not `Debug`, and `Block`
// now carries an `Option<SelfBinding>` (`home_binding`) yet derives `Debug`.
impl std::fmt::Debug for SelfBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            SelfBinding::Instance { .. } => "Instance",
            SelfBinding::ClassSide { .. } => "ClassSide",
            SelfBinding::Primitive { .. } => "Primitive",
        };
        write!(f, "SelfBinding::{kind}")
    }
}

/// Snapshot of an evaluator's inherited dynamic context for async continuations.
///
/// All fields are `Send + 'static` so this can be moved into `may::go!` closures.
/// Produced by `Evaluator::fork_context`; consumed by `ContinuationContext::into_evaluator`.
pub struct ContinuationContext {
    pub(crate) registry: Arc<ClassRegistry>,
    pub(crate) handler_chain: Vec<(Arc<Class>, Arc<Block>)>,
    pub(crate) active_namespace: Arc<Namespace>,
    /// Active transaction context, so a `then:` continuation's outgoing entity
    /// sends are tagged with the same transaction as the code that scheduled it.
    pub(crate) current_tx_ctx: Option<crate::shared_tx::GenTx>,
    /// The entity whose coroutine created this continuation. A `then:` block is
    /// created inside that entity, so the floating continuation acts as it: this
    /// makes self-sends in the continuation dispatch synchronously (the sync
    /// self-send path keys on `current_entity_id`), rather than enqueuing tagged
    /// messages back to the entity (which would, e.g., wrongly auto-enroll a
    /// transaction coordinator into its own transaction when commit runs in a
    /// spawned `then:` continuation).
    pub(crate) current_entity_id: Option<EntityId>,
}

impl ContinuationContext {
    /// Create a fresh evaluator initialized with this context's inherited fields.
    ///
    /// Requires: nothing.
    /// Ensures: returned evaluator shares `registry`; inherits `handler_chain` and
    ///   `active_namespace`; all other fields are fresh (empty stacks, no tx, etc.).
    pub fn into_evaluator(self) -> Evaluator {
        let store = self.registry.store.clone_handle();
        Evaluator {
            registry: self.registry,
            store,
            os: Arc::new(DefaultOsInterface),
            handler_chain: RefCell::new(self.handler_chain),
            active_namespace: RefCell::new(self.active_namespace),
            method_stack: RefCell::new(Vec::new()),
            method_depth: Cell::new(0),
            current_block_home: Cell::new(None),
            frames: RefCell::new(Vec::new()),
            current_entity_id: Cell::new(self.current_entity_id),
            value_objects: RefCell::new(HashMap::new()),
            current_tx: RefCell::new(None),
            current_tx_ctx: RefCell::new(self.current_tx_ctx),
            #[cfg(feature = "step_probe")]
            probe: crate::eval_iterative::probe::ProbeState::default(),
        }
    }
}

/// One method/block activation's flat local storage (env-frame redesign).
///
/// Lives on the evaluator's LIFO [`Evaluator::frames`] stack. Both fields are
/// resolver-indexed: `slots[i]` is `Local(i)`, `snapshot[i]` is `Upvalue(i)`.
pub(crate) struct Frame {
    /// Slot values in resolver order: params, then locals, then body temporaries.
    /// `self` is *not* a slot (it resolves via `SelfRef`).
    pub(crate) slots: Vec<Value>,
    /// The block's captured upvalue snapshot, filled at block-creation time.
    /// Empty for a method activation (methods never capture). Shared `Arc` so the
    /// block value and its running frame reference one copy — cheap to clone and
    /// `Send`, which is what lets a block outlive its creating frame.
    pub(crate) snapshot: Arc<[Value]>,
}

pub struct Evaluator {
    pub registry: Arc<ClassRegistry>,
    /// Shared store handle. Cloned from the registry's store; all evaluators and
    /// entities sharing the same registry share the same underlying store.
    pub store: Store,
    /// OS/world interface. Provides timer scheduling and future OS primitives.
    /// Defaults to `DefaultOsInterface`; tests may inject a fake.
    pub os: Arc<dyn OsInterface>,
    pub(crate) method_stack: RefCell<Vec<SelfBinding>>,
    /// Content-hash-addressed value objects registered during this evaluator's lifetime.
    /// Used for EntityRef resolution of ValueObject facts.
    pub(crate) value_objects: RefCell<HashMap<EntityId, Arc<InstanceState>>>,
    /// In-flight store transaction for the currently-executing entity handler.
    /// Set to `Some` by `run_entity_loop` before each dispatch; cleared on commit/rollback.
    /// `None` outside entity handler context (e.g. test-thread evaluations).
    pub(crate) current_tx: RefCell<Option<OwnedStoreTransaction>>,
    /// Active multi-entity transaction context (C31), or `None` outside any
    /// `atomic:` block. When `Some`, ivar writes go to the top shadow frame and
    /// outgoing entity sends are tagged with this context; nested `atomic:`
    /// pushes a `Child` whose `outer` is the previous head.
    pub(crate) current_tx_ctx: RefCell<Option<crate::shared_tx::GenTx>>,
    /// Depth of the active method stack. Maintained alongside `method_stack` so
    /// the common case — no active method — can be tested with a plain `Cell`
    /// read instead of a `RefCell` borrow.
    pub(crate) method_depth: Cell<usize>,
    /// Currently active namespace. Defaults to `Kernel`. Temporarily swapped by
    /// `Namespace do: [...]` for the duration of the block.
    ///
    /// Note: this is a per-evaluator (single-threaded) cell. Future multi-fiber
    /// execution will need to move this to a per-frame or per-fiber slot.
    pub(crate) active_namespace: RefCell<Arc<Namespace>>,
    /// The method-stack depth that the innermost active `eval_block` invocation
    /// should target with a `^`-return. `None` when not inside any `eval_block`
    /// (i.e., evaluating a method body directly). Saved and restored by each
    /// `eval_block` call so nesting is handled correctly.
    pub(crate) current_block_home: Cell<Option<usize>>,
    /// Slot frames for closed local scopes (methods, blocks) — the flat frame stack
    /// of the env-frame redesign. One [`Frame`] is pushed per method/block
    /// activation and popped on teardown, so the stack is strictly LIFO and the
    /// currently-executing scope's frame is always the top. `VarRef::Local(i)`
    /// reads/writes `frames.last().slots[i]`; `VarRef::Upvalue(i)` reads
    /// `frames.last().snapshot[i]` (P2.1c).
    ///
    /// Still kept in lockstep with the name-keyed `Env` (dual storage): `SelfRef`
    /// and `Dynamic` resolve by name via `Env` until P2.1d collapses it.
    pub(crate) frames: RefCell<Vec<Frame>>,
    /// The `EntityId` of the entity whose message handler is currently executing
    /// on this evaluator. `None` outside any entity handler. Used for sync
    /// self-send detection. Stored here (not in a thread-local) because may
    /// coroutines multiplex over OS threads with work-stealing, so a TLS value
    /// is not reliably tied to the executing coroutine.
    pub(crate) current_entity_id: Cell<Option<EntityId>>,
    /// Non-unwinding handler chain for `HandlerContext`. Each entry is a
    /// `(condition_class, handler_block)` pair. `_primSignalCondition:` walks this
    /// in reverse (most-recently-pushed first) before raising `EvalError::Signal`.
    /// Pushed/popped by `_primHCRun:`; always empty outside a `HandlerContext run:`.
    pub handler_chain: RefCell<Vec<HandlerPair>>,
    /// Debug-only `step_loop` stack instrumentation (C31.5). Per-evaluator (i.e.
    /// per-coroutine) so it is migration-correct under `may` work-stealing.
    #[cfg(feature = "step_probe")]
    pub(crate) probe: crate::eval_iterative::probe::ProbeState,
}

/// A `HandlerContext` handler registration: `(condition class, handler block)`.
pub(crate) type HandlerPair = (Arc<crate::class::Class>, Arc<crate::value::Block>);

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl Evaluator {
    /// Create an evaluator and load `kernel.smol`, defining `Array`, `ByteArray`,
    /// and `OrderedCollection` in the Kernel namespace.
    ///
    /// Ensures: after construction, `ev.registry.populate_env(&mut env)` exposes
    ///   all kernel classes in `env`. A broken `kernel.smol` is a programmer error
    ///   and panics immediately.
    pub fn new() -> Self {
        let ev = Self::new_without_kernel();
        let mut bootstrap_env = Env::new();
        ev.registry.populate_env(&mut bootstrap_env);
        let src = include_str!("kernel.smol");
        let prog = Parser::parse(src).expect("kernel.smol: parse error");
        ev.eval_program(&prog.stmts, &mut bootstrap_env)
            .expect("kernel.smol: eval error");
        ev
    }

    /// Create an evaluator without loading `kernel.smol`. Intended for low-level
    /// primitive tests that do not require the kernel collection classes.
    pub fn new_without_kernel() -> Self {
        let registry = ClassRegistry::new_arc();
        let store = registry.store.clone_handle();
        let kernel = registry.kernel();
        Self {
            registry,
            store,
            os: Arc::new(DefaultOsInterface),
            method_stack: RefCell::new(Vec::new()),
            method_depth: Cell::new(0),
            active_namespace: RefCell::new(kernel),
            current_block_home: Cell::new(None),
            frames: RefCell::new(Vec::new()),
            current_entity_id: Cell::new(None),
            value_objects: RefCell::new(HashMap::new()),
            current_tx: RefCell::new(None),
            current_tx_ctx: RefCell::new(None),
            handler_chain: RefCell::new(Vec::new()),
            #[cfg(feature = "step_probe")]
            probe: crate::eval_iterative::probe::ProbeState::default(),
        }
    }

    /// Register an instance entity so its coroutine is signalled on `shutdown()`.
    ///
    /// Requires: `handle.0.kind` is `InstanceKind::Entity`.
    pub fn track_entity(&self, handle: &InstanceHandle) {
        let id = match &handle.0.kind {
            InstanceKind::Entity(d) => d.id,
            _ => panic!("track_entity called on non-entity"),
        };
        self.registry
            .instance_entities
            .write()
            .insert(id, handle.0.clone());
        // Born-in-tx provenance: an entity created while a transaction context is
        // active is private to that level — record its id in the level's `spawned`
        // set so its first state access skips enrollment (writes go live) rather
        // than shadowing/validating. Recorded here, before the entity processes any
        // message, so its own coroutine sees the entry. See
        // `docs/enrollment_provenance_design.md`.
        if let Some(ctx) = self.current_tx_ctx.borrow().as_ref() {
            ctx.spawned().lock().unwrap().insert(id);
        }
    }

    /// Lazily enroll `handle`'s entity into transaction `ctx` on its first access
    /// to transactional state (ivar read/write under `ctx`'s tag). Idempotent:
    /// no-op if already enrolled at this level. Excluded (left unenrolled, so
    /// writes go live) when the entity was **born in `ctx`** (private to the level)
    /// or is a **transaction coordinator** (operates *on* transactions and writes
    /// its own `status`/`handle` ivars under the tag). After it returns,
    /// `d.has_frame_for(ctx.ctx_id())` is true iff the entity is now a participant.
    /// See `docs/enrollment_provenance_design.md`.
    pub(crate) fn ensure_tx_enrolled(
        &self,
        handle: &InstanceHandle,
        ctx: &crate::shared_tx::GenTx,
    ) {
        use crate::shared_tx::TxMode;
        let InstanceKind::Entity(d) = &handle.0.kind else {
            return;
        };
        let cid = ctx.ctx_id();
        if d.has_frame_for(cid) {
            return; // already a participant of this level
        }
        // Born in this level → private object with no committed baseline, excluded.
        if ctx.spawned().lock().unwrap().contains(&d.id) {
            return;
        }
        // A transaction coordinator is never a participant of the tx it runs.
        // Keyed on Transaction identity (no metaclass yet), matching the former
        // mailbox-loop `is_tx_coordinator` guard.
        if self
            .registry
            .kernel()
            .lookup_class("Transaction")
            .is_some_and(|tc| Class::is_subclass_of(&handle.0.class, &tc.0.class))
        {
            return;
        }
        match ctx.discipline() {
            TxMode::Atomic => d.tx_push_atomic_frame(cid, ctx.tx_id()),
            TxMode::Stm => d.tx_push_stm_frame(cid, ctx.tx_id()),
        }
        ctx.participants().lock().unwrap().push(handle.clone());
    }

    // ── Method receiver stack ─────────────────────────────────────────────────

    pub(crate) fn push_self(&self, binding: SelfBinding) {
        self.method_stack.borrow_mut().push(binding);
        self.method_depth.set(self.method_depth.get() + 1);
    }

    pub(crate) fn pop_self(&self) {
        self.method_stack.borrow_mut().pop();
        self.method_depth.set(self.method_depth.get() - 1);
    }

    /// Returns the `defining_class` of the top method frame (for `super` dispatch).
    pub(crate) fn current_defining_class(&self) -> Option<Arc<Class>> {
        match self.method_stack.borrow().last() {
            Some(SelfBinding::Instance { defining_class, .. }) => Some(defining_class.clone()),
            Some(SelfBinding::ClassSide { defining_class, .. }) => Some(defining_class.clone()),
            Some(SelfBinding::Primitive { defining_class, .. }) => Some(defining_class.clone()),
            None => None,
        }
    }

    /// Reconstruct the receiver (`self`) from the top of `method_stack` — the
    /// resolution for `VarRef::SelfRef` (env-frame redesign P2.1d).
    ///
    /// Requires: called from resolved method/block code, where a `SelfBinding` is
    ///   active — a block re-pushes its captured `home_binding`, so this holds even
    ///   after the block outlives its home frame.
    /// Ensures: `Instance`/`ClassSide` → `Value::Instance(handle)`; `Primitive` →
    ///   its `self_value`; no active binding (only reachable outside any method,
    ///   where `self` is not a `SelfRef`) → `Value::Nil`.
    pub(crate) fn self_value(&self) -> Value {
        match self.method_stack.borrow().last() {
            Some(SelfBinding::Instance { handle, .. } | SelfBinding::ClassSide { handle, .. }) => {
                Value::Instance(handle.clone())
            }
            Some(SelfBinding::Primitive { self_value, .. }) => self_value.clone(),
            None => Value::Nil,
        }
    }

    // ── Namespace helpers ─────────────────────────────────────────────────────

    pub fn active_namespace(&self) -> Arc<Namespace> {
        self.active_namespace.borrow().clone()
    }

    /// Temporarily sets the active namespace to `ns` for the duration of `f`,
    /// restoring the previous namespace on return.
    pub fn with_namespace<F, R>(&self, ns: Arc<Namespace>, f: F) -> R
    where
        F: FnOnce(&Evaluator) -> R,
    {
        let old = self.active_namespace.borrow().clone();
        *self.active_namespace.borrow_mut() = ns;
        let result = f(self);
        *self.active_namespace.borrow_mut() = old;
        result
    }

    // ── Variable lookup & assignment ──────────────────────────────────────────

    #[inline]
    pub fn lookup_var(&self, name: &str, env: &Env) -> Option<Value> {
        // Fast path: no active method context — skip the RefCell borrow entirely.
        if self.method_depth.get() == 0 {
            return env.get(name);
        }
        // Clone the top binding to avoid holding the RefCell borrow across further ops.
        let top = self.method_stack.borrow().last().cloned();
        match top {
            Some(SelfBinding::Instance { handle, .. }) => {
                // 1. local frame (params, method locals, self)
                if let Some(v) = env.lookup_top_frame(name) {
                    return Some(v);
                }
                // 2. instance variables — ctx-aware read-your-writes (C32). Only the
                //    shadow frames belonging to the CURRENT transaction's ancestry
                //    mask the live slot; unrelated interleaved frames (other txns,
                //    possible because `#stm` frames don't lock) are never consulted.
                //    Walk current_tx_ctx → parent → root; first shadow hit wins.
                if let InstanceKind::Entity(d) = &handle.0.kind {
                    let cur_ctx = self.current_tx_ctx.borrow().clone();
                    // Lazy read-enrollment (C32): reading one of this entity's own
                    // ivars under a tx adds it to the validated read-set (snapshots
                    // its version). Gated on the name being a declared ivar so
                    // classvar/global reads never spuriously enroll; born-in-tx and
                    // coordinator entities are excluded by `ensure_tx_enrolled`.
                    if let Some(ctx) = cur_ctx.as_ref()
                        && handle.ivar_storage().has_slot(name)
                    {
                        self.ensure_tx_enrolled(&handle, ctx);
                    }
                    let mut cur = cur_ctx;
                    while let Some(c) = cur {
                        if let Some(v) = d.tx_shadow_get_for(c.ctx_id(), name) {
                            return Some(v);
                        }
                        cur = c.parent();
                    }
                }
                if let Some(v) = handle.ivar_storage().get_slot(name) {
                    return Some(v);
                }
                // 3. class variables
                if let Some(owner) = handle.0.class.lookup_class_var_owner(name)
                    && let Some(v) = owner.class_var_storage.read().get(name).cloned()
                {
                    return Some(v);
                }
                // 4. outer env (globals)
                env.lookup_outer_frames(name)
            }
            Some(SelfBinding::ClassSide { handle, .. }) => {
                let class = &handle.0.class;
                // 1. local frame
                if let Some(v) = env.lookup_top_frame(name) {
                    return Some(v);
                }
                // 2. class variables (no instance variables — class-side has none)
                if let Some(owner) = class.lookup_class_var_owner(name)
                    && let Some(v) = owner.class_var_storage.read().get(name).cloned()
                {
                    return Some(v);
                }
                // 3. outer env (globals)
                env.lookup_outer_frames(name)
            }
            Some(SelfBinding::Primitive { defining_class, .. }) => {
                // 1. local frame (params, method locals, self)
                if let Some(v) = env.lookup_top_frame(name) {
                    return Some(v);
                }
                // 2. class variables of the primitive's class (no instance variables)
                if let Some(owner) = defining_class.lookup_class_var_owner(name)
                    && let Some(v) = owner.class_var_storage.read().get(name).cloned()
                {
                    return Some(v);
                }
                // 3. outer env (globals)
                // Ivar access for blocks with an instance home is handled by
                // push_block_frame re-pushing the block's `home_binding` before the
                // block frame, so lookup_var sees Instance at top, not Primitive.
                env.lookup_outer_frames(name)
            }
            None => env.get(name),
        }
    }

    /// Assign `value` to `name` (resolved as `res`) and return the stored value
    /// for use as the statement's result.
    ///
    /// Requires: `value` is the fully-evaluated right-hand side; `res` is `name`'s
    ///   resolution from the resolve pass.
    /// Ensures: a `Local` target writes the current activation's slot and the
    ///   name-keyed env in lockstep (dual storage, P2.1b — two clones until P2.1c/d
    ///   drop the env); every other target follows the method-context lookup order
    ///   with a single clone.
    #[inline]
    pub(crate) fn assign_var(
        &self,
        name: &Arc<str>,
        res: &VarRef,
        value: Value,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
        // Scope-local: write the slot directly (no ivar/classvar hash walk). Since
        // P2.1c, no reader consults the env's local entries — `Local` reads come from
        // slots, `Upvalue` from the block snapshot, and `Dynamic`/`SelfRef` never
        // name a local — so there is no env mirror to maintain here (one clone).
        // `Local` shadows any same-named ivar, consistent with `lookup_var`, whose
        // local frame is checked first.
        if let VarRef::Local(i) = res {
            let mut frames = self.frames.borrow_mut();
            let slot = frames
                .last_mut()
                .expect("Local assign with no active frame")
                .slots
                .get_mut(*i as usize)
                .expect("Local slot index out of range");
            *slot = value;
            return Ok(slot.clone());
        }
        if self.method_depth.get() > 0 {
            let top = self.method_stack.borrow().last().cloned();
            match top {
                Some(SelfBinding::Instance { handle, .. }) => {
                    let obj = handle.ivar_storage();
                    // Extract bool before any other access to avoid holding the Mutex
                    // guard across a re-entrant lock on the same object.
                    let has_ivar = obj.slots.lock().contains_key(name.as_ref());
                    if has_ivar {
                        if matches!(&handle.0.kind, InstanceKind::ValueObject(_)) && obj.is_frozen()
                        {
                            return Err(EvalError::TypeError(format!(
                                "cannot assign to ivar '{}': Value object is frozen",
                                name
                            )));
                        }
                        if let InstanceKind::Entity(d) = &handle.0.kind {
                            // Lazy enrollment (C32): the first write to one of this
                            // entity's ivars under a tx tag enrolls it as a
                            // participant, creating the frame the durable + shadow
                            // writes route into. Born-in-tx and coordinator entities
                            // are excluded by `ensure_tx_enrolled` and fall through
                            // to the live write below.
                            let cur_ctx = self.current_tx_ctx.borrow().clone();
                            if let Some(ctx) = cur_ctx.as_ref() {
                                self.ensure_tx_enrolled(&handle, ctx);
                            }
                            // Durable intent flows to the frame buffer (if enrolled)
                            // or the private current_tx (otherwise).
                            crate::store_bridge::buffer_ivar(self, d.id, name, &value);
                            // Ctx-aware speculative write (C32): a write is shadowed
                            // only when this entity is enrolled in the current tx.
                            // It is directed to *that* tx's frame — not the stack top,
                            // which under interleaving may be another tx's frame. An
                            // external (untagged) write, or a write by an entity not
                            // enrolled in the current tx (e.g. a coordinator or a
                            // born-in-tx object), falls through to the live slot below.
                            if let Some(ctx) = cur_ctx.as_ref()
                                && d.has_frame_for(ctx.ctx_id())
                            {
                                let ret = value.clone();
                                d.tx_shadow_set_for(ctx.ctx_id(), name.clone(), value);
                                return Ok(ret);
                            }
                        }
                        // Live write (external/non-tx entity, or a value object).
                        // Apply once, then — for an entity — bump the version (C32;
                        // gated on STM being active) so a concurrent STM transaction
                        // detects it.
                        let ret = value.clone();
                        obj.set_slot(name.clone(), value);
                        if let InstanceKind::Entity(d) = &handle.0.kind {
                            d.bump_version_if_stm();
                        }
                        return Ok(ret);
                    }
                    // Check class var.
                    if let Some(owner) = handle.0.class.lookup_class_var_owner(name.as_ref()) {
                        let ret = value.clone();
                        owner.class_var_storage.write().insert(name.clone(), value);
                        return Ok(ret);
                    }
                }
                Some(SelfBinding::ClassSide { handle, .. }) => {
                    // No ivar check for class-side.
                    if let Some(owner) = handle.0.class.lookup_class_var_owner(name.as_ref()) {
                        let ret = value.clone();
                        owner.class_var_storage.write().insert(name.clone(), value);
                        return Ok(ret);
                    }
                }
                Some(SelfBinding::Primitive { defining_class, .. }) => {
                    // Primitive receivers have no ivars. Ivar writes from blocks
                    // with an instance home are handled by push_block_frame
                    // re-pushing the block's `home_binding`, so assign_var sees
                    // Instance at top, not Primitive.
                    if let Some(owner) = defining_class.lookup_class_var_owner(name.as_ref()) {
                        let ret = value.clone();
                        owner.class_var_storage.write().insert(name.clone(), value);
                        return Ok(ret);
                    }
                }
                None => {}
            }
            // Method/block scope: the target must already exist — a declared local
            // (defined `Nil` by `push_*_frame`) or an existing outer binding.
            // ivars and classvars were handled by the match arms above. Never
            // silently create a new local: that swallowed typos (the implicit-
            // declaration wart; `perf_env_frame_redesign.md` §2.1 / P1b).
            let ret = value.clone();
            if env.set(name.as_ref(), value) {
                return Ok(ret);
            }
            return Err(EvalError::TypeError(format!(
                "cannot assign to undeclared variable `{name}`; declare it in `| ... |`"
            )));
        }
        // Top-level (REPL/script) scope: create-or-update a binding.
        env.update_or_define(name.clone(), value.clone());
        Ok(value)
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Evaluate a sequence of statements in `env`.
    ///
    /// Requires: `env` has at least one live frame.
    /// Ensures: returns the value of the last statement, or `Value::Nil` if
    ///   `stmts` is empty. `EvalError::Return` propagates to the caller; a
    ///   method-frame caller should catch and unwrap it.
    /// Iterative entry point: runs `stmts` via the step-loop evaluator.
    ///
    /// Requires: `env` has at least one live frame.
    /// Ensures: semantically identical to `eval_program_recursive`; Smol method calls
    ///   use heap-allocated `CallFrame`s rather than Rust stack frames.
    pub fn eval_program(&self, stmts: &[Stmt], env: &mut Env) -> Result<Value, EvalError> {
        for stmt in stmts {
            if let Stmt::LocalDecl(names) = stmt {
                for name in names {
                    env.define(name.clone(), Value::Nil);
                }
            }
        }
        let frame = Cont::ProgramFrame {
            pending: stmts.iter().cloned().collect(),
            env: env.clone(),
            saved_block_home: self.current_block_home.get(),
        };
        step_loop(self, frame)
    }

    fn eval_array_literal(&self, elements: &[Literal], env: &mut Env) -> Result<Value, EvalError> {
        use crate::entity::{InstanceHandle, InstanceKind, InstanceState};
        let array_handle = self
            .registry
            .kernel()
            .lookup_class("Array")
            .ok_or_else(|| EvalError::UndefinedVariable(Arc::from("Array")))?;
        let array_class = array_handle.0.class.clone();
        let obj = Arc::new(Object::new_with_indexed(
            array_class.clone(),
            elements.len(),
        ));
        for (i, elem) in elements.iter().enumerate() {
            let v = self.eval_literal_as_value(elem, env)?;
            if let Some(IndexedStorage::Pointer(slots)) = obj.indexed.as_ref() {
                slots.lock()[i] = v;
            }
        }
        Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
            class: array_class,
            kind: InstanceKind::ValueObject(obj),
        }))))
    }

    /// Evaluate a `Literal` into a runtime `Value`. For `Literal::Array` this
    /// requires the evaluator and environment to construct the `Array` object;
    /// all other variants are pure data and do not need either.
    pub(crate) fn eval_literal_as_value(
        &self,
        lit: &Literal,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
        match lit {
            Literal::Array(elems) => self.eval_array_literal(elems, env),
            _ => Ok(eval_literal(lit)),
        }
    }

    /// Evaluate `Expr::NamespaceRef`: resolve a `Ns::ClassName` path.
    pub(crate) fn eval_namespace_ref(
        &self,
        namespace_path: &[Arc<str>],
        name: &Arc<str>,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
        let first = namespace_path
            .first()
            .ok_or_else(|| EvalError::TypeError("empty namespace path".into()))?;
        let mut ns = match self.lookup_var(first.as_ref(), env) {
            Some(Value::Namespace(ns)) => ns,
            Some(other) => {
                return Err(EvalError::TypeError(format!(
                    "{first} is not a namespace, got {}",
                    other.kind()
                )));
            }
            None => return Err(EvalError::UndefinedVariable(first.clone())),
        };
        for segment in &namespace_path[1..] {
            ns = ns.lookup_sub(segment.as_ref()).ok_or_else(|| {
                EvalError::TypeError(format!(
                    "namespace {} has no sub-namespace {segment}",
                    ns.name
                ))
            })?;
        }
        ns.lookup_class(name.as_ref())
            .map(Value::Instance)
            .ok_or_else(|| {
                EvalError::TypeError(format!("class {name} not found in namespace {}", ns.name))
            })
    }

    /// Iterative `Block` evaluation: runs via the step-loop evaluator.
    ///
    /// Requires: `args.len() == block.params.len()`.
    /// Ensures: semantically identical to `eval_block_recursive`; method calls within
    ///   the body use heap-allocated `CallFrame`s rather than Rust stack frames.
    ///   Does NOT catch `EvalError::Return` — it propagates to the matching frame.
    pub fn eval_block(
        &self,
        block: &Block,
        args: Vec<Value>,
        _outer_env: &mut Env,
    ) -> Result<Value, EvalError> {
        let mut stack = Vec::new();
        push_block_frame(self, block, args, false, &mut stack)?;
        let initial = stack
            .pop()
            .expect("push_block_frame must push exactly one frame");
        step_loop(self, initial)
    }

    /// Iterative method invocation: runs via the step-loop evaluator.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: semantically identical to `invoke_method_recursive`; Smol calls
    ///   within the body use heap-allocated `CallFrame`s rather than Rust stack frames.
    pub fn invoke_method(
        &self,
        handle: InstanceHandle,
        defining_class: Arc<Class>,
        method: Arc<Method>,
        args: Vec<Value>,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
        let found = crate::builtins::UserMethodFound::Instance {
            handle,
            method,
            def_class: defining_class,
        };
        let mut stack = Vec::new();
        push_method_call_frame(self, found, args, env, &mut stack)?;
        let initial = stack
            .pop()
            .expect("push_method_call_frame must push exactly one frame");
        step_loop(self, initial)
    }

    /// Iterative class-side method invocation: runs via the step-loop evaluator.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: semantically identical to `invoke_class_method_recursive`; Smol calls
    ///   within the body use heap-allocated `CallFrame`s rather than Rust stack frames.
    pub fn invoke_class_method(
        &self,
        handle: crate::entity::InstanceHandle,
        defining_class: Arc<Class>,
        method: Arc<Method>,
        args: Vec<Value>,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
        let self_val = Value::Instance(handle.clone());
        let binding = SelfBinding::ClassSide {
            handle,
            defining_class,
        };
        let mut stack = Vec::new();
        push_method_frame(
            self,
            env.global_scope(),
            binding,
            self_val,
            &method,
            args,
            &mut stack,
        )?;
        let initial = stack
            .pop()
            .expect("push_method_frame must push exactly one frame");
        step_loop(self, initial)
    }

    /// Create an evaluator from an existing `ClassRegistry`, loading `kernel.smol`.
    ///
    /// Ensures: same post-conditions as `new()` but shares the given registry
    ///   instead of creating a fresh one. Intended for entity message loops so
    ///   that all entities share a single registry and kernel class set.
    pub fn with_registry(registry: Arc<ClassRegistry>) -> Self {
        let store = registry.store.clone_handle();
        let kernel = registry.kernel();
        Self {
            registry,
            store,
            os: Arc::new(DefaultOsInterface),
            method_stack: RefCell::new(Vec::new()),
            method_depth: Cell::new(0),
            active_namespace: RefCell::new(kernel),
            current_block_home: Cell::new(None),
            frames: RefCell::new(Vec::new()),
            current_entity_id: Cell::new(None),
            value_objects: RefCell::new(HashMap::new()),
            current_tx: RefCell::new(None),
            current_tx_ctx: RefCell::new(None),
            handler_chain: RefCell::new(Vec::new()),
            #[cfg(feature = "step_probe")]
            probe: crate::eval_iterative::probe::ProbeState::default(),
        }
    }

    /// Snapshot this evaluator's inherited dynamic context for an async continuation.
    ///
    /// Requires: nothing.
    /// Ensures: returns a `Send` snapshot of `handler_chain` and `active_namespace`.
    ///   Call `ContinuationContext::into_evaluator()` inside the continuation closure
    ///   to restore inherited context in the new coroutine.
    pub fn fork_context(&self) -> ContinuationContext {
        ContinuationContext {
            registry: self.registry.clone(),
            handler_chain: self.handler_chain.borrow().clone(),
            active_namespace: self.active_namespace.borrow().clone(),
            current_tx_ctx: self.current_tx_ctx.borrow().clone(),
            current_entity_id: self.current_entity_id.get(),
        }
    }

    /// Signal all tracked entities to terminate.
    ///
    /// Ensures: each tracked class entity and instance entity receives a shutdown
    ///   signal; their coroutines will exit on their next mailbox-wait cycle.
    pub fn shutdown(&self) {
        // Instance entities are tracked in the shared registry — signals all of them.
        self.registry.shutdown_class_entities();
    }
}

/// Returns a pre-allocated `Arc<str>` for the identifier `"self"`.
/// Allocated once per process via `OnceLock`; subsequent calls are a refcount bump.
pub(crate) fn self_sym() -> Arc<str> {
    static SELF: OnceLock<Arc<str>> = OnceLock::new();
    SELF.get_or_init(|| Arc::from("self")).clone()
}

pub(crate) fn eval_literal(lit: &Literal) -> Value {
    match lit {
        Literal::Int(n) => Value::Int(*n),
        Literal::Float(f) => Value::Float(*f),
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Nil => Value::Nil,
        Literal::Str(s) => Value::Str(s.clone()),
        Literal::Symbol(s) => Value::Symbol(s.clone()),
        Literal::LargeInt(n) => Value::LargeInt(n.clone()),
        // Array literals are always reached via Expr::ArrayLiteral -> eval_array_literal,
        // never via this path. Reaching here is an evaluator bug.
        Literal::Array(_) => {
            unreachable!("Literal::Array must be evaluated via eval_array_literal")
        }
    }
}

// ── _prim* self-check ─────────────────────────────────────────────────────────

fn selector_starts_with_prim(msg: &Message) -> bool {
    match msg {
        Message::Unary(s) => s.starts_with("_prim"),
        Message::Binary { op, .. } => op.starts_with("_prim"),
        // Keyword selector starts with _prim if its first fragment does.
        Message::Keyword { parts } => parts
            .first()
            .map(|(k, _)| k.starts_with("_prim"))
            .unwrap_or(false),
    }
}

fn is_self_or_super(expr: &Expr) -> bool {
    matches!(expr, Expr::Super) || matches!(expr, Expr::Var { name, .. } if name.as_ref() == "self")
}

/// Walk `expr` and return an error if any `_prim*` message is sent to a
/// receiver that is not `self` or `super`.
fn check_prim_sends_in_expr(expr: &Expr) -> Result<(), EvalError> {
    match expr {
        Expr::Send { receiver, message } => {
            if selector_starts_with_prim(message) && !is_self_or_super(receiver) {
                return Err(EvalError::TypeError(
                    "'_prim*' may only be sent to self".into(),
                ));
            }
            check_prim_sends_in_expr(receiver)?;
            check_prim_sends_in_message(message)
        }
        Expr::Cascade { receiver, messages } => {
            for msg in messages {
                if selector_starts_with_prim(msg) && !is_self_or_super(receiver) {
                    return Err(EvalError::TypeError(
                        "'_prim*' may only be sent to self".into(),
                    ));
                }
            }
            check_prim_sends_in_expr(receiver)?;
            for msg in messages {
                check_prim_sends_in_message(msg)?;
            }
            Ok(())
        }
        Expr::Block { body, .. } => check_prim_sends_in_body(body),
        Expr::DynamicArray(elements) => {
            for elem in elements {
                check_prim_sends_in_expr(elem)?;
            }
            Ok(())
        }
        Expr::Lit(_)
        | Expr::Var { .. }
        | Expr::Super
        | Expr::ArrayLiteral(_)
        | Expr::NamespaceRef { .. } => Ok(()),
    }
}

fn check_prim_sends_in_message(msg: &Message) -> Result<(), EvalError> {
    match msg {
        Message::Unary(_) => Ok(()),
        Message::Binary { arg, .. } => check_prim_sends_in_expr(arg),
        Message::Keyword { parts } => {
            for (_, e) in parts {
                check_prim_sends_in_expr(e)?;
            }
            Ok(())
        }
    }
}

/// Scan a method body for `_prim*` sends to non-`self`/non-`super` receivers.
///
/// Requires: `stmts` is the body of a method being installed.
/// Ensures: returns `Err(EvalError::TypeError)` if any such send is found;
///   otherwise `Ok(())`.
pub(crate) fn check_prim_sends_in_body(stmts: &[Stmt]) -> Result<(), EvalError> {
    for stmt in stmts {
        match stmt {
            Stmt::Expr(e) | Stmt::Return(e) => check_prim_sends_in_expr(e)?,
            Stmt::Assign { value, .. } => check_prim_sends_in_expr(value)?,
            Stmt::LocalDecl(_) => {}
            // Nested MethodDef bodies are checked when they are installed.
            Stmt::MethodDef(_) => {}
        }
    }
    Ok(())
}

/// Enforce single-assignment (SSA) locals — `docs/perf_env_frame_redesign.md` §2.1.
///
/// Requires: `params`/`locals` are a method's parameters and top-level temporaries;
///   `body` is its statement list.
/// Ensures: returns `Err(EvalError::TypeError)` if any local — of the method scope
///   or of any nested block scope — is assigned more than once. Assignments to
///   names not declared in their scope (ivars/classvars/globals) are not this
///   check's concern (undeclared-assignment enforcement is separate). Otherwise
///   `Ok(())`.
pub(crate) fn check_single_assignment_in_method(
    params: &[Arc<str>],
    locals: &[Arc<str>],
    body: &[Stmt],
) -> Result<(), EvalError> {
    let mut declared: HashSet<Arc<str>> = params.iter().cloned().collect();
    declared.extend(locals.iter().cloned());
    // Parameters are *bound on entry* to their argument, so any `:=` to a param is
    // already a second binding — seed `assigned` with them.
    let assigned: HashSet<Arc<str>> = params.iter().cloned().collect();
    check_single_assignment_scope(declared, assigned, body)
}

/// Check one scope (method or block body) for a second assignment to any of its
/// own declared locals, then recurse into nested block scopes. `declared` is this
/// scope's binding names; `assigned` is pre-seeded with names already bound on
/// entry (params); mid-body `LocalDecl`s are folded into `declared`.
fn check_single_assignment_scope(
    mut declared: HashSet<Arc<str>>,
    mut assigned: HashSet<Arc<str>>,
    body: &[Stmt],
) -> Result<(), EvalError> {
    for s in body {
        if let Stmt::LocalDecl(names) = s {
            declared.extend(names.iter().cloned());
        }
    }
    for s in body {
        match s {
            Stmt::Assign { name, value, .. } => {
                if declared.contains(name) && !assigned.insert(name.clone()) {
                    return Err(EvalError::TypeError(format!(
                        "cannot reassign local `{name}`: locals are single-assignment \
                         (bind once); introduce a new binding instead"
                    )));
                }
                check_single_assignment_expr(value)?;
            }
            Stmt::Expr(e) | Stmt::Return(e) => check_single_assignment_expr(e)?,
            Stmt::LocalDecl(_) => {}
            // Nested method defs are checked when they are installed.
            Stmt::MethodDef(_) => {}
        }
    }
    Ok(())
}

/// Recurse into an expression, treating each block literal as its own scope.
fn check_single_assignment_expr(e: &Expr) -> Result<(), EvalError> {
    match e {
        Expr::Block {
            params,
            locals,
            body,
            ..
        } => {
            let mut declared: HashSet<Arc<str>> = params.iter().cloned().collect();
            declared.extend(locals.iter().cloned());
            // Block params are bound when the block is invoked — pre-seed them.
            let assigned: HashSet<Arc<str>> = params.iter().cloned().collect();
            check_single_assignment_scope(declared, assigned, body)
        }
        Expr::Send { receiver, message } => {
            check_single_assignment_expr(receiver)?;
            check_single_assignment_message(message)
        }
        Expr::Cascade { receiver, messages } => {
            check_single_assignment_expr(receiver)?;
            for m in messages {
                check_single_assignment_message(m)?;
            }
            Ok(())
        }
        Expr::DynamicArray(es) => {
            for x in es {
                check_single_assignment_expr(x)?;
            }
            Ok(())
        }
        Expr::Lit(_)
        | Expr::Var { .. }
        | Expr::Super
        | Expr::NamespaceRef { .. }
        | Expr::ArrayLiteral(_) => Ok(()),
    }
}

fn check_single_assignment_message(msg: &Message) -> Result<(), EvalError> {
    match msg {
        Message::Unary(_) => Ok(()),
        Message::Binary { arg, .. } => check_single_assignment_expr(arg),
        Message::Keyword { parts } => {
            for (_, e) in parts {
                check_single_assignment_expr(e)?;
            }
            Ok(())
        }
    }
}

// ── Capture-before-assignment check (env-frame redesign P2.1c) ───────────────

/// Reject a block that captures a local before that local's (single) assignment.
///
/// Requires: `params`/`locals` are the scope's parameters and declared temporaries;
///   `body` is the scope's **resolved** statement list (every `Expr::Block` carries
///   its `captures` plan and every `Stmt::Assign` its `res`). Run at install time,
///   *after* `resolve_and_rewrite`.
/// Ensures: returns `Err(EvalError::TypeError)` if any block literal captures an
///   enclosing declared local (`CaptureSource::ParentLocal`) whose assignment does
///   not textually precede the block. Under single-assignment (`§2.1`) such a
///   capture can only ever snapshot the nil placeholder, so it is always a
///   programming error — and, unlike Smalltalk's cell capture, it is silent
///   (`docs/perf_env_frame_redesign.md §2.2`, `open_questions.md`). Otherwise `Ok(())`.
pub(crate) fn check_capture_before_assignment(
    params: &[Arc<str>],
    locals: &[Arc<str>],
    body: &[Stmt],
) -> Result<(), EvalError> {
    let slots = scope_slot_names(params, locals, body);
    check_captures_scope(&slots, params.len(), body)
}

/// Slot names in resolver order — params, then declared locals, then mid-body
/// `LocalDecl` names. Mirrors `resolve::scope_slots` so a `ParentLocal(i)` index
/// names the right local in the error.
fn scope_slot_names(params: &[Arc<str>], locals: &[Arc<str>], body: &[Stmt]) -> Vec<Arc<str>> {
    let mut slots: Vec<Arc<str>> = params.to_vec();
    slots.extend(locals.iter().cloned());
    for s in body {
        if let Stmt::LocalDecl(names) = s {
            slots.extend(names.iter().cloned());
        }
    }
    slots
}

/// Walk one scope's body in source order, tracking which slots are assigned, and
/// validate each nested block's `ParentLocal` captures against that set.
fn check_captures_scope(
    slots: &[Arc<str>],
    nparams: usize,
    body: &[Stmt],
) -> Result<(), EvalError> {
    // Params are bound on entry → assigned from the start. Slot indices align with
    // `slots` (params occupy `0..nparams`).
    let mut assigned: HashSet<u16> = (0..nparams as u16).collect();
    for s in body {
        match s {
            Stmt::Assign { res, value, .. } => {
                // The RHS is evaluated *before* the target is bound, so a block in
                // the RHS captures with the target still unassigned.
                check_captures_expr(slots, &assigned, value)?;
                if let VarRef::Local(i) = res {
                    assigned.insert(*i);
                }
            }
            Stmt::Expr(e) | Stmt::Return(e) => check_captures_expr(slots, &assigned, e)?,
            Stmt::LocalDecl(_) | Stmt::MethodDef(_) => {}
        }
    }
    Ok(())
}

fn check_captures_expr(
    slots: &[Arc<str>],
    assigned: &HashSet<u16>,
    e: &Expr,
) -> Result<(), EvalError> {
    match e {
        Expr::Block {
            params,
            locals,
            body,
            captures,
        } => {
            // A `ParentLocal` capture reads this (enclosing) scope's slot directly,
            // so it must be assigned by now. `ParentUpvalue` captures descend from an
            // outer `ParentLocal` already validated at its own level, and params are
            // always in `assigned`, so neither is checked here.
            for cap in captures {
                if let CaptureSource::ParentLocal(i) = cap
                    && !assigned.contains(i)
                {
                    let name = slots.get(*i as usize).map(|s| s.as_ref()).unwrap_or("?");
                    return Err(EvalError::TypeError(format!(
                        "block captures local `{name}` before it is assigned: locals are \
                         single-assignment, so this would capture nil — assign `{name}` \
                         before the block that captures it"
                    )));
                }
            }
            // Recurse into the block's own scope (fresh `assigned` = its params).
            let inner = scope_slot_names(params, locals, body);
            check_captures_scope(&inner, params.len(), body)
        }
        Expr::Send { receiver, message } => {
            check_captures_expr(slots, assigned, receiver)?;
            check_captures_message(slots, assigned, message)
        }
        Expr::Cascade { receiver, messages } => {
            check_captures_expr(slots, assigned, receiver)?;
            for m in messages {
                check_captures_message(slots, assigned, m)?;
            }
            Ok(())
        }
        Expr::DynamicArray(es) => {
            for x in es {
                check_captures_expr(slots, assigned, x)?;
            }
            Ok(())
        }
        Expr::Lit(_)
        | Expr::Var { .. }
        | Expr::Super
        | Expr::NamespaceRef { .. }
        | Expr::ArrayLiteral(_) => Ok(()),
    }
}

fn check_captures_message(
    slots: &[Arc<str>],
    assigned: &HashSet<u16>,
    msg: &Message,
) -> Result<(), EvalError> {
    match msg {
        Message::Unary(_) => Ok(()),
        Message::Binary { arg, .. } => check_captures_expr(slots, assigned, arg),
        Message::Keyword { parts } => {
            for (_, e) in parts {
                check_captures_expr(slots, assigned, e)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "tests/eval.rs"]
mod tests;
