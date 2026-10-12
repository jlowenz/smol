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
use crate::resolve::{ResolvedChunk, resolve_chunk};
use crate::value::{Block, Value};
use crate::workspace::Workspace;

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
    /// A block written at top level, running: there is no receiver, so `self` and
    /// `super` are errors and free names resolve through the active namespace (the
    /// block's home namespace) — never through whichever method called the block
    /// (env-frame redesign §11.7c D4).
    NoReceiver,
}

impl SelfBinding {
    /// The class whose method this binding's activation is running.
    ///
    /// Ensures: `None` exactly for `NoReceiver`.
    pub(crate) fn defining_class(&self) -> Option<&Arc<Class>> {
        match self {
            SelfBinding::Instance { defining_class, .. }
            | SelfBinding::ClassSide { defining_class, .. }
            | SelfBinding::Primitive { defining_class, .. } => Some(defining_class),
            SelfBinding::NoReceiver => None,
        }
    }
}

// Manual `Debug` (the variant name only): `Class` is not `Debug`, and `Block`
// now carries an `Option<SelfBinding>` (`home_binding`) yet derives `Debug`.
impl std::fmt::Debug for SelfBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            SelfBinding::Instance { .. } => "Instance",
            SelfBinding::ClassSide { .. } => "ClassSide",
            SelfBinding::Primitive { .. } => "Primitive",
            SelfBinding::NoReceiver => "NoReceiver",
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
            #[cfg(feature = "send_count")]
            sends: Cell::new(0),
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
    /// Message sends dispatched by this evaluator (see `sends_dispatched`).
    #[cfg(feature = "send_count")]
    pub(crate) sends: Cell<u64>,
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
    /// Ensures: after construction, a `Workspace::new(&ev)` sees every kernel
    ///   class. A broken `kernel.smol` is a programmer error
    ///   and panics immediately.
    pub fn new() -> Self {
        let ev = Self::new_without_kernel();
        let mut kernel_workspace = Workspace::kernel(&ev);
        let src = include_str!("kernel.smol");
        let chunk = Parser::parse_source(crate::span::SourceId::KERNEL, src)
            .expect("kernel.smol: parse error");
        ev.eval_chunk(&chunk, &mut kernel_workspace)
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
            #[cfg(feature = "send_count")]
            sends: Cell::new(0),
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
        self.method_stack.borrow().last()?.defining_class().cloned()
    }

    /// Total message sends this evaluator has dispatched: every syntactic send
    /// (including cascade parts and `super` sends) plus each `perform:` target.
    /// Sends handled by other evaluators (entity coroutines, async continuations)
    /// are counted there, not here.
    #[cfg(feature = "send_count")]
    pub fn sends_dispatched(&self) -> u64 {
        self.sends.get()
    }

    /// Reconstruct the receiver (`self`) from the top of `method_stack` — the
    /// resolution for `VarRef::SelfRef` (env-frame redesign P2.1d).
    ///
    /// Ensures: `Instance`/`ClassSide` → `Some(Value::Instance(handle))`;
    ///   `Primitive` → `Some(self_value)`; `NoReceiver` or no active binding
    ///   (top-level code) → `None`. A block re-pushes its captured `home_binding`,
    ///   so a method's block answers its home receiver even after the home frame
    ///   is gone.
    pub(crate) fn self_value(&self) -> Option<Value> {
        match self.method_stack.borrow().last()? {
            SelfBinding::Instance { handle, .. } | SelfBinding::ClassSide { handle, .. } => {
                Some(Value::Instance(handle.clone()))
            }
            SelfBinding::Primitive { self_value, .. } => Some(self_value.clone()),
            SelfBinding::NoReceiver => None,
        }
    }

    // ── Namespace helpers ─────────────────────────────────────────────────────

    pub fn active_namespace(&self) -> Arc<Namespace> {
        self.active_namespace.borrow().clone()
    }

    // ── Variable lookup & assignment ──────────────────────────────────────────

    /// Resolve an `Unresolved` name (top-level code, and blocks created at top
    /// level) by name: the activation's own env frame, then the receiver-scoped
    /// variables of the current `method_stack` binding, then the outer env frames
    /// (workspace temps), then the active namespace
    /// (`resolve_in_active_namespace`).
    ///
    /// Since d2c-c3 chunks are resolved, so no `Unresolved` reference reaches the
    /// evaluator from a chunk or a method; this path remains until d3 drops the env.
    #[inline]
    pub fn lookup_var(&self, name: &str, env: &Env) -> Option<Value> {
        let lexical = if self.method_depth.get() == 0 {
            // Fast path: no active method context — skip the RefCell borrow.
            env.get(name)
        } else {
            // Clone the top binding to avoid holding the RefCell borrow across
            // further ops.
            let top = self.method_stack.borrow().last().cloned();
            match top {
                Some(binding) => env
                    .lookup_top_frame(name)
                    .or_else(|| self.lookup_receiver_scoped(&binding, name))
                    .or_else(|| env.lookup_outer_frames(name)),
                None => env.get(name),
            }
        };
        lexical.or_else(|| self.resolve_in_active_namespace(name))
    }

    /// Resolve a `Dynamic` name — a free name in resolved code. In a method or a
    /// method's block: the receiver's ivars and classvars, then the defining
    /// class's namespace chain (`resolve_global`). In top-level code — a chunk
    /// (no binding) or a top-level block (`NoReceiver`): the active namespace's
    /// chain (`resolve_in_active_namespace`).
    ///
    /// Requires: `method_stack`'s top is the running code's home binding (a block
    ///   re-pushes its `home_binding`; a chunk pushes none).
    /// Ensures: reads are live — a class defined after this activation's entity
    ///   was spawned is visible; no per-entity snapshot of globals is consulted.
    pub(crate) fn lookup_dynamic(&self, name: &str) -> Option<Value> {
        let top = self.method_stack.borrow().last().cloned();
        match top {
            None | Some(SelfBinding::NoReceiver) => self.resolve_in_active_namespace(name),
            Some(binding) => self.lookup_receiver_scoped(&binding, name).or_else(|| {
                binding
                    .defining_class()
                    .and_then(|class| self.resolve_global(class, name))
            }),
        }
    }

    /// Resolve `name` relative to `class`'s defining namespace
    /// (`ClassRegistry::resolve_name`: own entries → imports → Kernel → roots).
    ///
    /// Ensures: a class with no live namespace link resolves from Kernel onward.
    pub(crate) fn resolve_global(&self, class: &Class, name: &str) -> Option<Value> {
        self.registry
            .resolve_name(class.namespace().as_deref(), name)
    }

    /// Resolve `name` relative to the active namespace — top-level code's
    /// lexical namespace (`ClassRegistry::resolve_name` order).
    pub(crate) fn resolve_in_active_namespace(&self, name: &str) -> Option<Value> {
        let ns = self.active_namespace();
        self.registry.resolve_name(Some(&ns), name)
    }

    /// Instance variables (transaction-aware) and class variables visible to the
    /// receiver bound by `binding`. Locals and globals are not this method's concern.
    fn lookup_receiver_scoped(&self, binding: &SelfBinding, name: &str) -> Option<Value> {
        match binding {
            SelfBinding::Instance { handle, .. } => {
                // Instance variables — ctx-aware read-your-writes (C32). Only the
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
                        self.ensure_tx_enrolled(handle, ctx);
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
                if let Some(owner) = handle.0.class.lookup_class_var_owner(name)
                    && let Some(v) = owner.class_var_storage.read().get(name).cloned()
                {
                    return Some(v);
                }
                None
            }
            // Class side and primitive receivers have no instance variables.
            SelfBinding::ClassSide { handle, .. } => {
                let owner = handle.0.class.lookup_class_var_owner(name)?;
                owner.class_var_storage.read().get(name).cloned()
            }
            SelfBinding::Primitive { defining_class, .. } => {
                let owner = defining_class.lookup_class_var_owner(name)?;
                owner.class_var_storage.read().get(name).cloned()
            }
            SelfBinding::NoReceiver => None,
        }
    }

    /// Assign `value` to `name` (resolved as `res`) and return the stored value
    /// for use as the statement's result.
    ///
    /// Requires: `value` is the fully-evaluated right-hand side; `res` is `name`'s
    ///   resolution from the resolve pass.
    /// Ensures: a `Local` target writes the current activation's slot; a `Dynamic`
    ///   target writes an ivar or classvar of the current binding, or is an error
    ///   (a namespace definition is never assignable); an `Unresolved` (top-level)
    ///   target writes an ivar/classvar of the current binding or an existing env
    ///   binding; assigning a name with no binding is an error (never an implicit
    ///   declaration).
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
                // No receiver: no ivars or classvars to assign.
                Some(SelfBinding::NoReceiver) | None => {}
            }
            // A resolved free name that is neither an ivar nor a classvar names a
            // namespace definition (or nothing): never assignable, and never routed
            // to an env binding (env-frame redesign d2a).
            if matches!(res, VarRef::Dynamic(_)) {
                return Err(EvalError::TypeError(format!(
                    "cannot assign to undeclared variable `{name}`; declare it in `| ... |`"
                )));
            }
            // Unresolved code (none since d2c-c3; removed in d3): the target must
            // already exist in the env. Never silently create a new local: that
            // swallowed typos (`perf_env_frame_redesign.md` §2.1 / P1b).
            let ret = value.clone();
            if env.set(name.as_ref(), value) {
                return Ok(ret);
            }
            return Err(EvalError::TypeError(format!(
                "cannot assign to undeclared variable `{name}`; declare it in `| ... |`"
            )));
        }
        // Unresolved code outside any method (none since d2c-c3, whose chunk
        // assignments are `Local`; removed in d3).
        let ret = value.clone();
        if env.set(name.as_ref(), value) {
            return Ok(ret);
        }
        Err(EvalError::TypeError(format!(
            "cannot assign to undeclared variable `{name}`; declare it in `| ... |`"
        )))
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Record that a statement at `span` executed, when coverage is enabled on
    /// the shared registry. A no-op — one atomic load — otherwise.
    pub(crate) fn record_coverage(&self, span: crate::span::Span) {
        if let Some(cov) = self.registry.coverage.get() {
            cov.lock().record(span);
        }
        if let Some(g) = crate::coverage::global_sink() {
            g.lock().record(span);
        }
    }

    /// Evaluate a top-level chunk (a script, or one REPL input) in `workspace`, via
    /// the step-loop evaluator.
    ///
    /// A chunk is a scope nested in the workspace (env-frame redesign §11.7b), and
    /// is resolved like a method body with no `self` (§11.7c D1): its `| ... |`
    /// temps are slots — fresh bindings that shadow any earlier chunk's
    /// same-named temps — each bound once, and the chunk may assign only the
    /// temps it declares itself. Earlier chunks' temps it reads are copied in when
    /// it starts; its own temps are published to `workspace` when it ends.
    ///
    /// Ensures: if any temp — of the chunk or of a block within it — is assigned
    ///   more than once, the chunk assigns a name it does not declare, or a block
    ///   captures a temp before the temp is assigned, returns
    ///   `Err(EvalError::TypeError)` before executing any statement. Otherwise the
    ///   chunk runs; its temps are published to `workspace` with the values they
    ///   held when it ended (normally or with an error), and the result is the
    ///   value of the last statement, or `Value::Nil` if the chunk is empty.
    ///   `EvalError::Return` propagates to the caller.
    pub fn eval_chunk(&self, chunk: &Chunk, workspace: &mut Workspace) -> Result<Value, EvalError> {
        check_chunk_assignments(&chunk.stmts)?;
        let resolved = resolve_chunk(&chunk.stmts, &|name| workspace.temp(name).is_some());
        check_chunk_captures(&resolved)?;
        // The chunk's lexical namespace is its workspace's: definitions register
        // there and free names resolve through it (§11.7c D3).
        let saved_ns = std::mem::replace(
            &mut *self.active_namespace.borrow_mut(),
            workspace.namespace().clone(),
        );
        let result = self.run_chunk(resolved, workspace);
        *self.active_namespace.borrow_mut() = saved_ns;
        result
    }

    /// Run a resolved chunk in its own slot frame: the prologue fills its import
    /// slots from `workspace`, the epilogue publishes its temps there.
    fn run_chunk(
        &self,
        chunk: ResolvedChunk,
        workspace: &mut Workspace,
    ) -> Result<Value, EvalError> {
        let ResolvedChunk {
            body,
            temps,
            imports,
        } = chunk;
        let mut slots = vec![Value::Nil; temps.len()];
        slots.extend(imports.iter().map(|name| {
            workspace
                .temp(name)
                .cloned()
                .expect("resolve_chunk imports only names the workspace holds")
        }));
        let base = self.frames.borrow().len();
        self.frames.borrow_mut().push(Frame {
            slots,
            snapshot: crate::eval_iterative::empty_snapshot(),
        });
        let frame = Cont::ChunkFrame {
            pending: body.into_iter().collect(),
            env: Env::new(),
            saved_block_home: self.current_block_home.get(),
        };
        let result = step_loop(self, frame);
        // The chunk owns its slot frame (`ChunkFrame` teardown leaves it alone), so
        // every exit path — normal, error, a tail call that drained the
        // `ChunkFrame` — publishes from it here.
        let frame = {
            let mut frames = self.frames.borrow_mut();
            debug_assert_eq!(frames.len(), base + 1, "chunk slot frame not on top");
            frames.pop().expect("chunk slot frame missing")
        };
        for (name, value) in temps.into_iter().zip(frame.slots) {
            workspace.publish(name, value);
        }
        result
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
    ///
    /// Ensures: the first segment resolves as a free name (`lookup_dynamic`) — via
    ///   the defining class's namespace chain in a method, the active namespace's
    ///   at top level — never as a local or workspace temp.
    pub(crate) fn eval_namespace_ref(
        &self,
        namespace_path: &[Arc<str>],
        name: &Arc<str>,
    ) -> Result<Value, EvalError> {
        let first = namespace_path
            .first()
            .ok_or_else(|| EvalError::TypeError("empty namespace path".into()))?;
        let found = self.lookup_dynamic(first.as_ref());
        let mut ns = match found {
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
            #[cfg(feature = "send_count")]
            sends: Cell::new(0),
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
pub(crate) fn check_prim_sends_in_body(stmts: &[TaggedStmt]) -> Result<(), EvalError> {
    for stmt in stmts {
        match &stmt.node {
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
    body: &[TaggedStmt],
) -> Result<(), EvalError> {
    let mut declared: HashSet<Arc<str>> = params.iter().cloned().collect();
    declared.extend(locals.iter().cloned());
    // Parameters are *bound on entry* to their argument, so any `:=` to a param is
    // already a second binding — seed `assigned` with them.
    let assigned: HashSet<Arc<str>> = params.iter().cloned().collect();
    check_single_assignment_scope(declared, assigned, body)
}

/// Enforce the workspace scope rules for one top-level chunk (env-frame redesign
/// §11.7b): single-assignment for the chunk's temps and every nested block's
/// locals, and no assignment to a name the chunk does not declare.
///
/// Ensures: `Err(EvalError::TypeError)` on the first violation; otherwise `Ok(())`.
fn check_chunk_assignments(stmts: &[TaggedStmt]) -> Result<(), EvalError> {
    let declared: HashSet<Arc<str>> = stmts
        .iter()
        .filter_map(|s| match &s.node {
            Stmt::LocalDecl(names) => Some(names.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect();
    for s in stmts {
        if let Stmt::Assign { name, .. } = &s.node
            && !declared.contains(name)
        {
            return Err(EvalError::TypeError(format!(
                "cannot assign to undeclared variable `{name}`; declare it in `| ... |`"
            )));
        }
    }
    check_single_assignment_scope(declared, HashSet::new(), stmts)
}

/// Check one scope (method or block body) for a second assignment to any of its
/// own declared locals, then recurse into nested block scopes. `declared` is this
/// scope's binding names; `assigned` is pre-seeded with names already bound on
/// entry (params); mid-body `LocalDecl`s are folded into `declared`.
fn check_single_assignment_scope(
    mut declared: HashSet<Arc<str>>,
    mut assigned: HashSet<Arc<str>>,
    body: &[TaggedStmt],
) -> Result<(), EvalError> {
    for s in body {
        if let Stmt::LocalDecl(names) = &s.node {
            declared.extend(names.iter().cloned());
        }
    }
    for s in body {
        match &s.node {
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
    body: &[TaggedStmt],
) -> Result<(), EvalError> {
    let slots = scope_slot_names(params, locals, body);
    check_captures_scope(&slots, (0..params.len() as u16).collect(), body)
}

/// [`check_capture_before_assignment`] for a resolved chunk.
///
/// Requires: `chunk` came from `resolve::resolve_chunk`.
/// Ensures: as for a method, with the chunk's temps as its locals and its import
///   slots as already assigned (they hold earlier chunks' published values from
///   the chunk's start).
fn check_chunk_captures(chunk: &ResolvedChunk) -> Result<(), EvalError> {
    let slots: Vec<Arc<str>> = chunk.temps.iter().chain(&chunk.imports).cloned().collect();
    let imported = (chunk.temps.len() as u16..slots.len() as u16).collect();
    check_captures_scope(&slots, imported, &chunk.body)
}

/// Slot names in resolver order — params, then declared locals, then mid-body
/// `LocalDecl` names. Mirrors `resolve::scope_slots` so a `ParentLocal(i)` index
/// names the right local in the error.
fn scope_slot_names(
    params: &[Arc<str>],
    locals: &[Arc<str>],
    body: &[TaggedStmt],
) -> Vec<Arc<str>> {
    let mut slots: Vec<Arc<str>> = params.to_vec();
    slots.extend(locals.iter().cloned());
    for s in body {
        if let Stmt::LocalDecl(names) = &s.node {
            slots.extend(names.iter().cloned());
        }
    }
    slots
}

/// Walk one scope's body in source order, tracking which slots are assigned, and
/// validate each nested block's `ParentLocal` captures against that set.
/// `assigned` starts as the slots bound on entry (params; a chunk's imports).
fn check_captures_scope(
    slots: &[Arc<str>],
    mut assigned: HashSet<u16>,
    body: &[TaggedStmt],
) -> Result<(), EvalError> {
    for s in body {
        match &s.node {
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
            check_captures_scope(&inner, (0..params.len() as u16).collect(), body)
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
