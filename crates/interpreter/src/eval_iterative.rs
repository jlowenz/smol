use std::collections::VecDeque;
use std::sync::Arc;

use crate::ast::{Expr, Message, MethodDef, Stmt};
use crate::builtins::{UserMethodFound, as_block, force_value, lookup_user_method};
use crate::class::{Class, Method};
use crate::entity::{InstanceHandle, InstanceKind};
use crate::env::Env;
use crate::eval::{
    EvalError, Evaluator, SelfBinding, check_prim_sends_in_body, eval_literal, self_sym,
};
use crate::value::Value;

/// One reduction = one completed message dispatch. Matches BEAM's "one reduction
/// per function call" granularity. At ~1M msg/s this yields every ~1 ms.
const YIELD_INTERVAL: u32 = 1024;

/// Debug-only instrumentation for measuring `step_loop` recursion depth.
///
/// `step_loop` recurses indirectly: a block-taking builtin that calls
/// `ev.eval_block(...)` (rather than pushing a `Cont`) enters a fresh
/// `step_loop`. This probe records the maximum nesting reached so the C31.5
/// frame-shrinking work can be measured before/after. Compiled out entirely
/// unless the `step_probe` feature is enabled.
/// Debug-only `step_loop` stack instrumentation (C31.5).
///
/// State lives on the owning [`Evaluator`](crate::eval::Evaluator) (one per
/// coroutine), **not** in thread-locals or globals. Because `may` work-steals
/// coroutines across OS threads, thread-local state would be wrong whenever a
/// measured run migrated; Evaluator-held state follows the computation, so it is
/// migration-correct and naturally isolated per coroutine.
#[cfg(feature = "step_probe")]
pub mod probe {
    use std::cell::{Cell, RefCell};

    /// Per-evaluator `step_loop` stack measurements.
    pub struct ProbeState {
        /// Current `step_loop` recursion depth.
        depth: Cell<usize>,
        /// Maximum recursion depth observed since the last `reset`.
        max_depth: Cell<usize>,
        /// Shallowest (largest-address) stack pointer at a `step_loop` entry.
        max_sp: Cell<usize>,
        /// Deepest (smallest-address) stack pointer at a `step_loop` entry.
        min_sp: Cell<usize>,
        /// Largest Rust-stack delta between a `step_loop` entry and its parent —
        /// the stack consumed by one `step_loop → builtins → eval_block →
        /// step_loop` recursion, i.e. the per-nesting-level growth a flattened
        /// evaluator drives toward zero.
        max_level_delta: Cell<usize>,
        /// Max stack from the enclosing `step_loop` entry down to a
        /// `signal_condition` entry — the dispatch chain above the handler eval.
        max_sigcond_delta: Cell<usize>,
        /// Stack pointer of each currently-active `step_loop` entry, parent-first.
        entry_sps: RefCell<Vec<usize>>,
    }

    impl Default for ProbeState {
        fn default() -> Self {
            ProbeState {
                depth: Cell::new(0),
                max_depth: Cell::new(0),
                max_sp: Cell::new(0),
                min_sp: Cell::new(usize::MAX),
                max_level_delta: Cell::new(0),
                max_sigcond_delta: Cell::new(0),
                entry_sps: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProbeState {
        /// Sample the stack pointer at a `signal_condition` entry to attribute the
        /// per-level delta to the dispatch chain. `sp` is the address of a local
        /// in `signal_condition`'s frame.
        pub fn sample_signal_condition(&self, sp: usize) {
            if let Some(&parent) = self.entry_sps.borrow().last() {
                let d = parent.saturating_sub(sp);
                self.max_sigcond_delta
                    .set(self.max_sigcond_delta.get().max(d));
            }
        }

        /// Reset all recorded marks (call before a measured run).
        #[cfg(test)]
        pub fn reset(&self) {
            self.depth.set(0);
            self.max_depth.set(0);
            self.max_sp.set(0);
            self.min_sp.set(usize::MAX);
            self.max_level_delta.set(0);
            self.max_sigcond_delta.set(0);
            self.entry_sps.borrow_mut().clear();
        }

        /// Maximum `step_loop` recursion depth observed since the last `reset`.
        #[cfg(test)]
        pub fn max_depth(&self) -> usize {
            self.max_depth.get()
        }

        /// Largest Rust-stack delta (bytes) between a `step_loop` entry and its
        /// parent entry — the true per-nesting-level cost (not averaged).
        #[cfg(test)]
        pub fn level_delta(&self) -> usize {
            self.max_level_delta.get()
        }

        /// Max stack from a `step_loop` entry down to a `signal_condition` entry.
        #[cfg(test)]
        pub fn sigcond_delta(&self) -> usize {
            self.max_sigcond_delta.get()
        }

        /// Total Rust stack span (bytes) between the shallowest and deepest
        /// `step_loop` entry. Returns 0 if fewer than two entries were sampled.
        #[cfg(test)]
        pub fn stack_span(&self) -> usize {
            let lo = self.min_sp.get();
            let hi = self.max_sp.get();
            if lo == usize::MAX || hi <= lo {
                0
            } else {
                hi - lo
            }
        }

        /// `size_of::<Cont>()` in bytes — the per-element cost of the `Vec<Cont>`.
        #[cfg(test)]
        pub fn cont_size(&self) -> usize {
            std::mem::size_of::<super::Cont>()
        }
    }

    /// RAII guard borrowing the evaluator's [`ProbeState`]: bumps `step_loop`
    /// depth and the high-water marks on entry, restores depth + entry-SP stack
    /// on drop.
    pub(super) struct DepthGuard<'a> {
        st: &'a ProbeState,
    }

    impl<'a> DepthGuard<'a> {
        /// `sp` is the address of a local in the calling `step_loop` frame (a
        /// proxy for the stack pointer). Stacks grow downward on all supported
        /// targets, so deeper recursion yields smaller `sp`.
        pub(super) fn enter(st: &'a ProbeState, sp: usize) -> Self {
            let n = st.depth.get() + 1;
            st.depth.set(n);
            st.max_depth.set(st.max_depth.get().max(n));
            st.max_sp.set(st.max_sp.get().max(sp));
            st.min_sp.set(st.min_sp.get().min(sp));
            {
                let mut sps = st.entry_sps.borrow_mut();
                if let Some(&parent) = sps.last() {
                    // Parent entered shallower (larger address); child is deeper.
                    let d = parent.saturating_sub(sp);
                    st.max_level_delta.set(st.max_level_delta.get().max(d));
                }
                sps.push(sp);
            }
            DepthGuard { st }
        }
    }

    impl Drop for DepthGuard<'_> {
        fn drop(&mut self) {
            self.st.depth.set(self.st.depth.get().saturating_sub(1));
            self.st.entry_sps.borrow_mut().pop();
        }
    }
}

/// Heap-allocated continuation for the iterative evaluator.
///
/// Each variant represents a suspended computation waiting for `acc` (the
/// result of the previous step) before it can proceed.
pub(crate) enum Cont {
    /// Evaluate an expression; result becomes `acc`.
    EvalExpr { expr: Expr, env: Env },

    /// Receiver just evaluated (`acc`); now dispatch the message.
    AfterReceiver { message: Message, env: Env },

    /// Accumulate keyword/binary arguments left-to-right, then dispatch.
    ///
    /// `pending` holds remaining argument expressions. When exhausted, `done`
    /// (with the last `acc` appended) is the full argument list.
    CollectArgs {
        receiver: Value,
        selector: Arc<str>,
        done: Vec<Value>,
        pending: VecDeque<Expr>,
        env: Env,
    },

    /// Accumulate arguments for a super send left-to-right, then dispatch to superclass.
    ///
    /// Like `CollectArgs` but has no receiver value — the receiver is always `self`,
    /// looked up from `ev.method_stack` at dispatch time.
    SuperCollectArgs {
        selector: Arc<str>,
        done: Vec<Value>,
        pending: VecDeque<Expr>,
        env: Env,
    },

    /// Cascade: all messages sent to the original receiver; step through `pending`.
    ///
    /// `receiver` is `None` on initial fire (acc = newly-evaluated cascade receiver);
    /// `Some(v)` on subsequent fires (acc = result of last send, receiver is constant).
    CascadeStep {
        receiver: Option<Value>,
        pending: VecDeque<Message>,
        env: Env,
    },

    /// Dynamic array: accumulate evaluated elements, then allocate.
    DynArrayElem {
        array_class: InstanceHandle,
        done: Vec<Value>,
        pending: VecDeque<Expr>,
        env: Env,
    },

    /// RHS just evaluated (`acc`); assign to `name` in `env`.
    Assign { name: Arc<str>, env: Env },

    /// Non-local `^`-return: `acc` is the return value; unwind to the
    /// `CallFrame` whose `depth` matches `block.home_depth`.
    ReturnUnwind { target_depth: usize },

    /// An active user-method (instance, primitive, or class-side) invocation.
    ///
    /// Pushed by `push_method_call_frame` / `push_class_method_frame` /
    /// `invoke_*method`. A `SelfBinding` was pushed onto `method_stack` when the
    /// frame was created, so teardown always calls `pop_self`. Exits returning
    /// `implicit_self` (Smalltalk methods answer `self` absent an explicit `^`).
    MethodFrame {
        /// Method-stack depth of this frame (matches `EvalError::Return`).
        depth: usize,
        /// Remaining statements to run.
        pending: VecDeque<Stmt>,
        /// Method environment (always `push_frame`d on setup; popped on teardown).
        env: Env,
        /// The receiver, returned when the body ends without an explicit `^`.
        implicit_self: Value,
        /// `current_block_home` before this frame; restored on teardown.
        saved_block_home: Option<usize>,
    },

    /// An active block invocation (`value`, `ifTrue:` arm, `on:do:` body, …).
    ///
    /// Pushed by `push_block_frame`. Blocks do not push a self binding *unless*
    /// they captured a `home_self` (then a synthetic `SelfBinding::Instance` was
    /// pushed and `block_home_self` records the handle to pop). A block answers
    /// its last expression's value, so there is no implicit-return override.
    BlockFrame {
        /// Remaining statements to run.
        pending: VecDeque<Stmt>,
        /// Block environment (always `push_frame`d on setup; popped on teardown).
        env: Env,
        /// `current_block_home` before this frame; restored on teardown.
        saved_block_home: Option<usize>,
        /// `Some(h)` if a synthetic self binding for the captured home instance
        /// was pushed onto `method_stack` and must be popped on teardown.
        block_home_self: Option<InstanceHandle>,
        /// When `true`, a `ForceAcc` is pushed after each statement so that
        /// `Value::Future` results resolve before the next statement runs.
        /// Used by `syncValue`, `on:do:` body, and `ensure:` body.
        sync: bool,
    },

    /// The top-level program frame pushed by `eval_program`.
    ///
    /// Unlike method/block frames it does not own an env frame (`eval_program`
    /// evaluates in the caller's env) and pushes no self binding, so teardown
    /// only restores `current_block_home`. Answers its last expression's value.
    ProgramFrame {
        /// Remaining statements to run.
        pending: VecDeque<Stmt>,
        /// Program environment (not `push_frame`d; not popped on teardown).
        env: Env,
        /// `current_block_home` before this frame; restored on teardown.
        saved_block_home: Option<usize>,
    },

    /// Force `acc` via `force_value` before the next continuation fires.
    /// Pushed after each statement dispatch in a sync-semantics frame.
    ForceAcc,

    /// Restore `acc` to a saved value, discarding the current one.
    /// Used by `EnsureGuard` on the success path to return the body result
    /// after the ensure block has run.
    RestoreAcc { value: Value },

    /// Re-raise a saved error after an ensure block has finished running.
    /// Pushed below the ensure block frame so the error propagates after cleanup.
    Rethrow { error: EvalError },

    /// Marks the bottom of an `ensure:` body on the stack.
    ///
    /// Success path: fires when body completes; runs the ensure block,
    ///   then `RestoreAcc` restores the body result.
    /// Error path: `handle_step_error` intercepts it, pushes `Rethrow` + ensure frame.
    /// Return (`^`) path: `unwind_stack_to_depth` intercepts it, pushes `ContinueUnwind` + ensure frame.
    EnsureGuard {
        ensure_blk: Arc<crate::value::Block>,
    },

    /// Marks the bottom of an `on:do:` body on the stack.
    ///
    /// Success path: fires and simply passes `acc` through (body result unchanged).
    /// Signal path: `handle_step_error` intercepts it, checks `catch_class`, runs
    ///   handler via `HandleOnDoResult` or re-raises.
    /// Return (`^`) path: passes through — unwind drops it without running handler.
    OnDoCatch {
        catch_class: Arc<crate::class::Class>,
        handler_blk: Arc<crate::value::Block>,
        body_blk: Arc<crate::value::Block>,
    },

    /// Sits below a `then:` receiver-block frame. After the receiver block
    /// evaluates, inspects its result: a `Future` is handed to the async `then:`
    /// path (spawns a continuation coroutine); any other value runs the
    /// continuation block on the step-loop stack. This keeps the synchronous
    /// `then:` arms off the Rust call stack (no recursive `eval_block`).
    ThenCheck {
        cont_blk: Arc<crate::value::Block>,
        env: Env,
    },

    /// Sits below the `on:do:` handler block frame, inspecting the handler's outcome.
    ///
    /// Success path: fires normally → `acc` = handler result, pass through.
    /// `Retry`: `handle_step_error` intercepts → re-runs body with fresh `OnDoCatch`.
    /// `Resume(v)` / `ReturnFromBlock(v)`: `handle_step_error` sets `acc = v`, resumes.
    HandleOnDoResult {
        catch_class: Arc<crate::class::Class>,
        handler_blk: Arc<crate::value::Block>,
        body_blk: Arc<crate::value::Block>,
    },

    /// Resume a `^`-return unwind after an ensure block has run.
    /// Holds the original return value and the target depth to resume unwinding toward.
    ContinueUnwind {
        return_val: Value,
        target_depth: usize,
    },

    /// Marks the bottom of a `_primHCRun:` body on the stack.
    ///
    /// This HandlerContext's handler pairs were pushed onto `ev.handler_chain`,
    /// extending it past `chain_start_len`. The guard truncates the chain back to
    /// `chain_start_len` on every exit path and drives the `retry` loop — the
    /// flattened equivalent of the recursive `_primHCRun:` builtin, so nested
    /// synchronous `run:` does not grow the Rust stack.
    ///
    /// Success path: fires when the body completes; truncates chain, passes `acc`.
    /// `Retry`: `handle_step_error` re-runs `body_blk` with the chain unchanged.
    /// `ReturnFromBlock(v)`: `handle_step_error` truncates the chain, yields `v`.
    /// Other errors / `^`-return: chain truncated as the guard is discarded.
    HCRunGuard {
        chain_start_len: usize,
        body_blk: Arc<crate::value::Block>,
    },

    /// Drives a `to:do:` / `to:by:do:` numeric loop on the heap `Cont` stack so
    /// that nested iteration (and thus all collection iteration, which bottoms
    /// out in `to:do:`) does not grow the Rust stack per nesting level.
    ///
    /// Each fire runs `blk` with the current index `next` (discarding its result)
    /// and re-pushes itself advanced by `step`, until `next` passes `stop`. A
    /// `^`-return or signal from the body unwinds past this `Cont` (it is not a
    /// frame), stopping the loop. Answers `nil` when the range is exhausted.
    ToDoStep {
        blk: Arc<crate::value::Block>,
        next: i64,
        stop: i64,
        step: i64,
    },

    /// Sits below a signal handler's block frame (flattened `_primSignalCondition:`
    /// / `error:`). The handler runs on the heap `Cont` stack with itself and all
    /// inner handlers removed from `handler_chain` (Common-Lisp handler-scoping);
    /// `removed` holds those entries, re-appended whenever the guard is left.
    ///
    /// Success / `resume:`: the handler's value becomes the signal expression's
    ///   value — the continuation below resumes with it (non-unwinding handler).
    /// `return:` / `retry` / re-signal: propagate (to the enclosing `HCRunGuard`
    ///   or an outer handler) after the chain is restored.
    SignalGuard {
        removed: Vec<crate::eval::HandlerPair>,
    },

    /// Sits below a `_primNewValue:` / `_primNewValueIndexed:init:` init block.
    /// The freshly-allocated (unfrozen) `inst` runs its init block on the heap
    /// `Cont` stack; on normal completion this guard freezes `inst` and makes it
    /// the result. A `^`-return / error from the init block abandons the instance
    /// (no freeze), matching the recursive builtin.
    NewValueGuard { inst: InstanceHandle },

    /// Sits below a `Namespace do:` block. The active namespace was swapped to the
    /// receiver for the block's duration; this guard restores `saved_ns` on every
    /// exit path (success, error, `^`-return) — the flattened equivalent of
    /// `Evaluator::with_namespace`, so nested `Namespace do:` doesn't grow the
    /// Rust stack. The block's value passes through as the result.
    NamespaceGuard {
        saved_ns: Arc<crate::namespace::Namespace>,
    },
}

/// Outcome of a `^`-return unwind scan.
enum UnwindOutcome {
    /// The target frame was found; stack is unwound to that point.
    Reached,
    /// An `EnsureGuard` was hit before the target frame; `ContinueUnwind` + ensure
    /// frame have been pushed onto the stack. `step_loop` should continue running.
    Interrupted,
    /// Stack exhausted without finding the target frame; stack is now empty.
    Exhausted,
}

/// Run the evaluator-side teardown for a popped frame continuation.
///
/// Requires: `frame` was just popped from the step-loop stack.
/// Ensures: if `frame` is a `MethodFrame` / `BlockFrame` / `ProgramFrame`, its
///   self binding (if any) is popped, `current_block_home` is restored, and its
///   owned env frame (if any) is popped; returns `Some(depth)` where `depth` is
///   the frame's `^`-return match depth (`0` for block/program frames). For any
///   other continuation `frame` is dropped and `None` is returned.
fn teardown_frame(ev: &Evaluator, frame: Cont) -> Option<usize> {
    match frame {
        Cont::MethodFrame {
            depth,
            mut env,
            saved_block_home,
            ..
        } => {
            ev.pop_self();
            ev.current_block_home.set(saved_block_home);
            env.pop_frame();
            Some(depth)
        }
        Cont::BlockFrame {
            mut env,
            saved_block_home,
            block_home_self,
            ..
        } => {
            if block_home_self.is_some() {
                ev.pop_self();
            }
            ev.current_block_home.set(saved_block_home);
            env.pop_frame();
            Some(0)
        }
        Cont::ProgramFrame {
            saved_block_home, ..
        } => {
            ev.current_block_home.set(saved_block_home);
            Some(0)
        }
        // Not a frame, but discarding an HCRun boundary (on `^`-return unwind or
        // terminal cleanup) must still restore the handler_chain.
        Cont::HCRunGuard {
            chain_start_len, ..
        } => {
            ev.handler_chain.borrow_mut().truncate(chain_start_len);
            None
        }
        // Discarding a signal boundary re-enables the handlers it had disabled.
        Cont::SignalGuard { removed } => {
            ev.handler_chain.borrow_mut().extend(removed);
            None
        }
        // Discarding a `Namespace do:` boundary restores the active namespace.
        Cont::NamespaceGuard { saved_ns } => {
            *ev.active_namespace.borrow_mut() = saved_ns;
            None
        }
        _ => None,
    }
}

/// Unwind heap call frames until one with `depth == target_depth` is found,
/// intercepting any `EnsureGuard` encountered along the way.
///
/// Requires: `return_val` is the value being returned; `target_depth` is the
///   method-stack depth of the frame that should catch the `^`-return.
/// Ensures:
///   `Reached`: matching frame found and cleaned up; frames above it cleaned up.
///   `Interrupted`: `EnsureGuard` was hit; `ContinueUnwind{return_val, target_depth}`
///     pushed below the ensure block frame; `step_loop` must continue.
///   `Exhausted`: stack consumed without match; all `CallFrame`s cleaned up.
fn unwind_stack_to_depth(
    ev: &Evaluator,
    stack: &mut Vec<Cont>,
    return_val: Value,
    target_depth: usize,
) -> Result<UnwindOutcome, EvalError> {
    while let Some(frame) = stack.pop() {
        match frame {
            Cont::EnsureGuard { ensure_blk } => {
                // Intercept: run ensure block, then resume the unwind.
                stack.push(Cont::ContinueUnwind {
                    return_val,
                    target_depth,
                });
                // ensure blocks always take 0 args; ArityMismatch cannot occur here.
                push_block_frame(ev, &ensure_blk, vec![], false, stack)?;
                return Ok(UnwindOutcome::Interrupted);
            }
            // Frames are torn down; OnDoCatch and other entries are discarded.
            other => {
                if let Some(depth) = teardown_frame(ev, other)
                    && depth == target_depth
                {
                    return Ok(UnwindOutcome::Reached);
                }
            }
        }
    }
    Ok(UnwindOutcome::Exhausted)
}

/// Pop and clean up all remaining frames on the stack (for terminal error propagation).
fn cleanup_frames(ev: &Evaluator, stack: &mut Vec<Cont>) {
    while let Some(frame) = stack.pop() {
        teardown_frame(ev, frame);
    }
}

/// Handle an error that escaped from a dispatch call inside `step_loop`.
///
/// `EvalError::Return`: stack is unwound via `unwind_stack_to_depth`; if the
///   target frame is found `acc` is set and `Ok(())` returned; if an `EnsureGuard`
///   is encountered the ensure block is queued and `Ok(())` returned for `step_loop`
///   to continue; if the stack is exhausted, `EvalError::Return` propagates.
///
/// All other errors: the stack is scanned for the nearest `EnsureGuard` (cleanup)
///   or `OnDoCatch` / `HandleOnDoResult` (condition handling). `CallFrame`s
///   encountered during the scan are cleaned up. If the scan exhausts the stack
///   the error propagates.
fn handle_step_error(
    e: EvalError,
    ev: &Evaluator,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    match e {
        EvalError::Return(v, depth) => {
            match unwind_stack_to_depth(ev, stack, v.clone(), depth)? {
                UnwindOutcome::Reached => {
                    *acc = v;
                    Ok(())
                }
                UnwindOutcome::Interrupted => {
                    // EnsureGuard pushed ContinueUnwind + ensure frame; step_loop continues.
                    Ok(())
                }
                UnwindOutcome::Exhausted => {
                    cleanup_frames(ev, stack);
                    Err(EvalError::Return(v, depth))
                }
            }
        }
        other => {
            // Scan the stack for the nearest EnsureGuard (cleanup) or OnDoCatch/
            // HandleOnDoResult (condition handling), cleaning up frames along the way.
            while let Some(cont) = stack.pop() {
                match cont {
                    Cont::EnsureGuard { ensure_blk } => {
                        // Run ensure block, then re-raise the original error.
                        stack.push(Cont::Rethrow { error: other });
                        // ensure blocks take 0 args; ArityMismatch cannot occur here.
                        push_block_frame(ev, &ensure_blk, vec![], false, stack)?;
                        return Ok(());
                    }
                    Cont::OnDoCatch {
                        catch_class,
                        handler_blk,
                        body_blk,
                    } => {
                        if let EvalError::Signal(ref cond) = other {
                            let signal_class = match cond {
                                Value::Instance(h) => h.0.class.clone(),
                                _ => continue,
                            };
                            if Class::is_subclass_of(&signal_class, &catch_class) {
                                let cond_val = cond.clone();
                                stack.push(Cont::HandleOnDoResult {
                                    catch_class,
                                    handler_blk: handler_blk.clone(),
                                    body_blk,
                                });
                                push_block_frame(ev, &handler_blk, vec![cond_val], false, stack)?;
                                return Ok(());
                            }
                        }
                        // Signal class doesn't match, or error isn't a Signal; keep scanning.
                    }
                    Cont::HandleOnDoResult {
                        catch_class,
                        handler_blk,
                        body_blk,
                    } => {
                        match &other {
                            EvalError::Retry => {
                                stack.push(Cont::OnDoCatch {
                                    catch_class,
                                    handler_blk: handler_blk.clone(),
                                    body_blk: body_blk.clone(),
                                });
                                push_block_frame(ev, &body_blk, vec![], true, stack)?;
                                return Ok(());
                            }
                            EvalError::Resume(v) => {
                                *acc = v.clone();
                                return Ok(());
                            }
                            EvalError::ReturnFromBlock(v) => {
                                *acc = v.clone();
                                return Ok(());
                            }
                            _ => {
                                // Not Retry/Resume/ReturnFromBlock; keep scanning for outer handler.
                            }
                        }
                    }
                    Cont::HCRunGuard {
                        chain_start_len,
                        body_blk,
                    } => match &other {
                        // Retry re-runs the body with this HC's handlers still
                        // registered (chain unchanged), mirroring the recursive loop.
                        EvalError::Retry => {
                            stack.push(Cont::HCRunGuard {
                                chain_start_len,
                                body_blk: body_blk.clone(),
                            });
                            push_block_frame(ev, &body_blk, vec![], false, stack)?;
                            return Ok(());
                        }
                        // A value returned from run:; truncate chain, deliver it.
                        EvalError::ReturnFromBlock(v) => {
                            ev.handler_chain.borrow_mut().truncate(chain_start_len);
                            *acc = v.clone();
                            return Ok(());
                        }
                        // Any other error escapes this boundary: drop the HC's
                        // handlers, then keep propagating.
                        _ => {
                            ev.handler_chain.borrow_mut().truncate(chain_start_len);
                        }
                    },
                    Cont::SignalGuard { removed } => {
                        // Re-enable the handlers this signal had disabled.
                        ev.handler_chain.borrow_mut().extend(removed);
                        // `resume:` delivers a value to the signal point; return:/
                        // retry/re-signal propagate to the enclosing HCRunGuard /
                        // outer handler (keep scanning).
                        if let EvalError::Resume(v) = &other {
                            *acc = v.clone();
                            return Ok(());
                        }
                    }
                    // Frames are torn down; all other continuations (ForceAcc,
                    // RestoreAcc, CollectArgs, …) are discarded during propagation.
                    other_cont => {
                        teardown_frame(ev, other_cont);
                    }
                }
            }
            // Stack exhausted — no handler found; propagate.
            Err(other)
        }
    }
}

/// Handle a `Cont::EvalExpr`: evaluate `expr`, updating `acc` or pushing
/// follow-up continuations onto `stack`.
///
/// Extracted from `step_loop`'s match as `#[inline(never)]` so this arm's
/// (large) locals live in this frame — created and freed within one loop
/// iteration — rather than being summed into `step_loop`'s per-call frame.
#[inline(never)]
fn handle_eval_expr(
    ev: &Evaluator,
    expr: Expr,
    mut env: Env,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    match expr {
        Expr::Lit(lit) => {
            *acc = eval_literal(&lit);
        }
        Expr::Var(name) => {
            *acc = ev
                .lookup_var(&name, &env)
                .ok_or_else(|| EvalError::UndefinedVariable(name.clone()))?;
        }
        Expr::Block {
            params,
            locals,
            body,
        } => {
            let home_depth = ev
                .current_block_home
                .get()
                .unwrap_or_else(|| ev.method_depth.get());
            let home_self = {
                let ms = ev.method_stack.borrow();
                ms.iter().rev().find_map(|b| match b {
                    SelfBinding::Instance { handle, .. } => Some(handle.clone()),
                    _ => None,
                })
            };
            *acc = Value::Block(Arc::new(crate::value::Block {
                params,
                locals,
                body,
                env: env.clone(),
                home_depth,
                home_entity: ev.current_entity_id.get(),
                home_self,
            }));
        }
        Expr::NamespaceRef {
            namespace_path,
            name,
        } => {
            *acc = ev.eval_namespace_ref(&namespace_path, &name, &mut env)?;
        }
        Expr::Super => {
            return Err(EvalError::TypeError(
                "super may only appear as a message receiver".into(),
            ));
        }
        Expr::Send { receiver, message } => {
            if matches!(receiver.as_ref(), Expr::Super) {
                match message {
                    Message::Unary(sel) => {
                        if let Err(e) =
                            dispatch_super_to_stack(ev, sel, vec![], &mut env, stack, acc)
                        {
                            handle_step_error(e, ev, stack, acc)?;
                        }
                    }
                    Message::Binary { op, arg } => {
                        stack.push(Cont::SuperCollectArgs {
                            selector: op,
                            done: vec![],
                            pending: VecDeque::new(),
                            env: env.clone(),
                        });
                        stack.push(Cont::EvalExpr { expr: *arg, env });
                    }
                    Message::Keyword { parts } => {
                        let selector: Arc<str> = parts
                            .iter()
                            .map(|(k, _)| k.as_ref())
                            .collect::<String>()
                            .into();
                        let mut exprs: VecDeque<Expr> = parts.into_iter().map(|(_, e)| e).collect();
                        let first = exprs.pop_front().expect("keyword message has no parts");
                        stack.push(Cont::SuperCollectArgs {
                            selector,
                            done: vec![],
                            pending: exprs,
                            env: env.clone(),
                        });
                        stack.push(Cont::EvalExpr { expr: first, env });
                    }
                }
            } else {
                // Push AfterReceiver first (below in stack), then receiver eval.
                stack.push(Cont::AfterReceiver {
                    message,
                    env: env.clone(),
                });
                stack.push(Cont::EvalExpr {
                    expr: *receiver,
                    env,
                });
            }
        }
        Expr::Cascade { receiver, messages } => {
            stack.push(Cont::CascadeStep {
                receiver: None,
                pending: messages.into_iter().collect(),
                env: env.clone(),
            });
            stack.push(Cont::EvalExpr {
                expr: *receiver,
                env,
            });
        }
        Expr::ArrayLiteral(elements) => {
            *acc = eval_array_literal_iterative(ev, &elements, &mut env)?;
        }
        Expr::DynamicArray(elements) => {
            let array_handle = ev
                .registry
                .kernel()
                .lookup_class("Array")
                .ok_or_else(|| EvalError::UndefinedVariable(Arc::from("Array")))?;
            if elements.is_empty() {
                *acc = build_array(array_handle.0.class.clone(), vec![]);
            } else {
                let mut exprs: VecDeque<Expr> = elements.into_iter().collect();
                let first = exprs.pop_front().unwrap();
                stack.push(Cont::DynArrayElem {
                    array_class: array_handle,
                    done: vec![],
                    pending: exprs,
                    env: env.clone(),
                });
                stack.push(Cont::EvalExpr { expr: first, env });
            }
        }
    }
    Ok(())
}

/// Drive the iterative evaluator to completion starting from `initial_cont`.
///
/// Requires: any frame in `initial_cont` has already had its env
///   frame pushed and, for method frames, its self binding pushed to
///   `ev.method_stack`.
/// Ensures: returns the value accumulated when the continuation stack
///   is empty, or propagates the first terminal error.
pub(crate) fn step_loop(ev: &Evaluator, initial_cont: Cont) -> Result<Value, EvalError> {
    #[cfg(feature = "step_probe")]
    let _depth_guard = {
        // Address of a local in this frame — a proxy for the current stack pointer.
        let sp_probe = 0u8;
        probe::DepthGuard::enter(&ev.probe, &sp_probe as *const u8 as usize)
    };

    let mut stack: Vec<Cont> = vec![initial_cont];
    let mut acc: Value = Value::Nil;
    let mut reductions: u32 = 0;

    loop {
        let cont = match stack.pop() {
            Some(c) => c,
            None => return Ok(acc),
        };

        match cont {
            // ── Leaf expressions ─────────────────────────────────────────
            Cont::EvalExpr { expr, env } => handle_eval_expr(ev, expr, env, &mut stack, &mut acc)?,

            // ── Assignment ───────────────────────────────────────────────
            Cont::Assign { name, mut env } => {
                acc = ev.assign_var(&name, acc.clone(), &mut env)?;
            }

            // ── Non-local return ─────────────────────────────────────────
            Cont::ReturnUnwind { target_depth } => {
                let return_val = acc.clone();
                match unwind_stack_to_depth(ev, &mut stack, return_val.clone(), target_depth)? {
                    UnwindOutcome::Reached => {
                        acc = return_val;
                    }
                    UnwindOutcome::Interrupted => {
                        // EnsureGuard hit; ContinueUnwind + ensure frame pushed; step_loop continues.
                    }
                    UnwindOutcome::Exhausted => {
                        return Err(EvalError::Return(return_val, target_depth));
                    }
                }
            }

            // ── MethodFrame dispatch ──────────────────────────────────────
            Cont::MethodFrame {
                depth,
                pending,
                env,
                implicit_self,
                saved_block_home,
            } => {
                handle_method_frame(
                    ev,
                    depth,
                    pending,
                    env,
                    implicit_self,
                    saved_block_home,
                    &mut stack,
                    &mut acc,
                )?;
            }

            // ── BlockFrame dispatch ───────────────────────────────────────
            Cont::BlockFrame {
                pending,
                env,
                saved_block_home,
                block_home_self,
                sync,
            } => {
                handle_block_frame(
                    ev,
                    pending,
                    env,
                    saved_block_home,
                    block_home_self,
                    sync,
                    &mut stack,
                    &mut acc,
                )?;
            }

            // ── ProgramFrame dispatch ─────────────────────────────────────
            Cont::ProgramFrame {
                pending,
                env,
                saved_block_home,
            } => {
                handle_program_frame(ev, pending, env, saved_block_home, &mut stack, &mut acc)?;
            }

            // ── ForceAcc ──────────────────────────────────────────────────
            Cont::ForceAcc => {
                let v = std::mem::replace(&mut acc, Value::Nil);
                match force_value(v) {
                    Ok(forced) => acc = forced,
                    // A forced Future may resolve to an error (e.g. a signal raised in an
                    // entity). Route through handle_step_error so OnDoCatch/EnsureGuard
                    // on the stack can intercept it.
                    Err(e) => handle_step_error(e, ev, &mut stack, &mut acc)?,
                }
            }

            // ── RestoreAcc ────────────────────────────────────────────────
            Cont::RestoreAcc { value } => {
                acc = value;
            }

            // ── Rethrow ───────────────────────────────────────────────────
            Cont::Rethrow { error } => {
                handle_step_error(error, ev, &mut stack, &mut acc)?;
            }

            // ── EnsureGuard (success path) ────────────────────────────────
            Cont::EnsureGuard { ensure_blk } => {
                let body_result = acc.clone();
                stack.push(Cont::RestoreAcc { value: body_result });
                push_block_frame(ev, &ensure_blk, vec![], false, &mut stack)?;
            }

            // ── ThenCheck (then: receiver-block completed) ────────────────
            Cont::ThenCheck { cont_blk, mut env } => {
                let recv_result = std::mem::replace(&mut acc, Value::Nil);
                match recv_result {
                    // Receiver yielded a future: hand off to the async `then:`
                    // path, which spawns a continuation coroutine (fresh stack).
                    Value::Future(_) => {
                        acc = crate::builtins::dispatch(
                            ev,
                            recv_result,
                            Arc::from("then:"),
                            vec![Value::Block(cont_blk)],
                            &mut env,
                        )?;
                    }
                    // Non-future: run the continuation on the step-loop stack;
                    // its result becomes the `then:` result.
                    _ => {
                        push_block_frame(ev, &cont_blk, vec![], false, &mut stack)?;
                    }
                }
            }

            // ── OnDoCatch (success path) ──────────────────────────────────
            Cont::OnDoCatch { .. } => {
                // Body completed without raising a signal; acc = body result, pass through.
            }

            // ── HandleOnDoResult (success path) ───────────────────────────
            Cont::HandleOnDoResult { .. } => {
                // Handler completed normally; acc = handler result, pass through.
            }

            // ── HCRunGuard (success path) ─────────────────────────────────
            Cont::HCRunGuard {
                chain_start_len, ..
            } => {
                // Body completed normally; drop this HC's handlers, pass acc through.
                ev.handler_chain.borrow_mut().truncate(chain_start_len);
            }

            // ── SignalGuard (handler completed normally) ──────────────────
            Cont::SignalGuard { removed } => {
                // Re-enable the disabled handlers; acc (handler result) becomes
                // the signal expression's value and the continuation resumes.
                ev.handler_chain.borrow_mut().extend(removed);
            }

            // ── NewValueGuard (init block completed) ──────────────────────
            Cont::NewValueGuard { inst } => {
                // Init block finished; freeze the instance and answer it.
                crate::builtins_iterative::freeze_value_instance(&inst);
                acc = Value::Instance(inst);
            }

            // ── NamespaceGuard (Namespace do: block completed) ────────────
            Cont::NamespaceGuard { saved_ns } => {
                // Restore the active namespace; acc (block result) passes through.
                *ev.active_namespace.borrow_mut() = saved_ns;
            }

            // ── ToDoStep (numeric loop iteration) ─────────────────────────
            Cont::ToDoStep {
                blk,
                next,
                stop,
                step,
            } => {
                let cont = if step > 0 { next <= stop } else { next >= stop };
                if cont {
                    bump_reductions(&mut reductions);
                    stack.push(Cont::ToDoStep {
                        blk: blk.clone(),
                        next: next + step,
                        stop,
                        step,
                    });
                    // Per-iteration result is discarded; the next ToDoStep (or the
                    // final acc=nil) overwrites it.
                    push_block_frame(ev, &blk, vec![Value::Int(next)], false, &mut stack)?;
                } else {
                    acc = Value::Nil; // to:do: / to:by:do: answer nil
                }
            }

            // ── ContinueUnwind ────────────────────────────────────────────
            Cont::ContinueUnwind {
                return_val,
                target_depth,
            } => {
                // Ensure block just ran (acc = ensure result, discard it); resume unwind.
                match unwind_stack_to_depth(ev, &mut stack, return_val.clone(), target_depth)? {
                    UnwindOutcome::Reached => {
                        acc = return_val;
                    }
                    UnwindOutcome::Interrupted => {
                        // Another EnsureGuard was hit; step_loop continues.
                    }
                    UnwindOutcome::Exhausted => {
                        return Err(EvalError::Return(return_val, target_depth));
                    }
                }
            }

            // ── AfterReceiver ─────────────────────────────────────────────
            Cont::AfterReceiver { message, env } => {
                handle_after_receiver(ev, message, env, &mut stack, &mut acc)?;
            }

            // ── CollectArgs ───────────────────────────────────────────────
            Cont::CollectArgs {
                receiver,
                selector,
                done,
                pending,
                env,
            } => {
                handle_collect_args(
                    ev,
                    receiver,
                    selector,
                    done,
                    pending,
                    env,
                    &mut stack,
                    &mut acc,
                    &mut reductions,
                )?;
            }

            // ── SuperCollectArgs ──────────────────────────────────────────
            Cont::SuperCollectArgs {
                selector,
                done,
                pending,
                env,
            } => {
                handle_super_collect_args(
                    ev,
                    selector,
                    done,
                    pending,
                    env,
                    &mut stack,
                    &mut acc,
                    &mut reductions,
                )?;
            }

            // ── CascadeStep ───────────────────────────────────────────────
            Cont::CascadeStep {
                receiver,
                pending,
                env,
            } => {
                handle_cascade_step(ev, receiver, pending, env, &mut stack, &mut acc)?;
            }

            // ── DynArrayElem ──────────────────────────────────────────────
            Cont::DynArrayElem {
                array_class,
                mut done,
                mut pending,
                env,
            } => {
                done.push(acc.clone());
                if let Some(next) = pending.pop_front() {
                    stack.push(Cont::DynArrayElem {
                        array_class,
                        done,
                        pending,
                        env: env.clone(),
                    });
                    stack.push(Cont::EvalExpr { expr: next, env });
                } else {
                    acc = build_array(array_class.0.class.clone(), done);
                }
            }
        }
    }
}

/// `Cont::MethodFrame` handler — see the variant docs. Extracted `#[inline(never)]`.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn handle_method_frame(
    ev: &Evaluator,
    depth: usize,
    mut pending: VecDeque<Stmt>,
    mut env: Env,
    implicit_self: Value,
    saved_block_home: Option<usize>,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    if pending.is_empty() {
        // Frame exhausted — pop self binding, restore home, pop env.
        ev.pop_self();
        ev.current_block_home.set(saved_block_home);
        env.pop_frame();
        // Methods answer `self` absent an explicit `^`.
        *acc = implicit_self;
    } else {
        let stmt = pending.pop_front().unwrap();
        // Push the frame back first (continuing frame is below new work).
        stack.push(Cont::MethodFrame {
            depth,
            pending,
            env: env.clone(),
            implicit_self,
            saved_block_home,
        });
        dispatch_stmt(ev, stmt, &env, depth, stack, acc)?;
    }
    Ok(())
}

/// `Cont::BlockFrame` handler — see the variant docs. Extracted `#[inline(never)]`.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn handle_block_frame(
    ev: &Evaluator,
    mut pending: VecDeque<Stmt>,
    mut env: Env,
    saved_block_home: Option<usize>,
    block_home_self: Option<InstanceHandle>,
    sync: bool,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    if pending.is_empty() {
        // Frame exhausted — pop synthetic self (if any), restore home, pop env.
        if block_home_self.is_some() {
            ev.pop_self();
        }
        ev.current_block_home.set(saved_block_home);
        env.pop_frame();
        // acc stays as the last expression result.
    } else {
        let stmt = pending.pop_front().unwrap();
        stack.push(Cont::BlockFrame {
            pending,
            env: env.clone(),
            saved_block_home,
            block_home_self,
            sync,
        });
        // For sync frames, force the statement result before the frame resumes.
        if sync {
            stack.push(Cont::ForceAcc);
        }
        // Blocks match `^`-returns against `current_block_home` (always set during a
        // block), so the frame depth here is irrelevant.
        dispatch_stmt(ev, stmt, &env, 0, stack, acc)?;
    }
    Ok(())
}

/// `Cont::ProgramFrame` handler — see the variant docs. Extracted `#[inline(never)]`.
#[inline(never)]
fn handle_program_frame(
    ev: &Evaluator,
    mut pending: VecDeque<Stmt>,
    env: Env,
    saved_block_home: Option<usize>,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    if pending.is_empty() {
        // Top-level frame: restore home only (no self binding, no owned env).
        ev.current_block_home.set(saved_block_home);
        // acc stays as the last expression result.
    } else {
        let stmt = pending.pop_front().unwrap();
        stack.push(Cont::ProgramFrame {
            pending,
            env: env.clone(),
            saved_block_home,
        });
        dispatch_stmt(ev, stmt, &env, 0, stack, acc)?;
    }
    Ok(())
}

/// `Cont::AfterReceiver` handler: receiver is in `acc`; dispatch the message
/// (unary) or begin argument collection (binary/keyword). Extracted `#[inline(never)]`.
#[inline(never)]
fn handle_after_receiver(
    ev: &Evaluator,
    message: Message,
    mut env: Env,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    let receiver = acc.clone();
    match message {
        Message::Unary(sel) => {
            if let Err(e) = dispatch_to_stack(ev, receiver, sel, vec![], &mut env, stack, acc) {
                handle_step_error(e, ev, stack, acc)?;
            }
        }
        Message::Binary { op, arg } => {
            stack.push(Cont::CollectArgs {
                receiver,
                selector: op,
                done: vec![],
                pending: VecDeque::new(), // first arg already being pushed
                env: env.clone(),
            });
            stack.push(Cont::EvalExpr { expr: *arg, env });
        }
        Message::Keyword { parts } => {
            let selector: Arc<str> = parts
                .iter()
                .map(|(k, _)| k.as_ref())
                .collect::<String>()
                .into();
            let mut exprs: VecDeque<Expr> = parts.into_iter().map(|(_, e)| e).collect();
            let first = exprs.pop_front().expect("keyword message has no parts");
            stack.push(Cont::CollectArgs {
                receiver,
                selector,
                done: vec![],
                pending: exprs,
                env: env.clone(),
            });
            stack.push(Cont::EvalExpr { expr: first, env });
        }
    }
    Ok(())
}

/// `Cont::CollectArgs` handler: append `acc` to `done`, evaluate the next
/// argument, or dispatch once arguments are exhausted. Extracted `#[inline(never)]`.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn handle_collect_args(
    ev: &Evaluator,
    receiver: Value,
    selector: Arc<str>,
    mut done: Vec<Value>,
    mut pending: VecDeque<Expr>,
    mut env: Env,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
    reductions: &mut u32,
) -> Result<(), EvalError> {
    done.push(acc.clone());
    if let Some(next) = pending.pop_front() {
        stack.push(Cont::CollectArgs {
            receiver,
            selector,
            done,
            pending,
            env: env.clone(),
        });
        stack.push(Cont::EvalExpr { expr: next, env });
    } else {
        bump_reductions(reductions);
        if let Err(e) = dispatch_to_stack(ev, receiver, selector, done, &mut env, stack, acc) {
            handle_step_error(e, ev, stack, acc)?;
        }
    }
    Ok(())
}

/// `Cont::SuperCollectArgs` handler — like `handle_collect_args` but dispatches
/// to the superclass (`self` receiver). Extracted `#[inline(never)]`.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn handle_super_collect_args(
    ev: &Evaluator,
    selector: Arc<str>,
    mut done: Vec<Value>,
    mut pending: VecDeque<Expr>,
    mut env: Env,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
    reductions: &mut u32,
) -> Result<(), EvalError> {
    done.push(acc.clone());
    if let Some(next) = pending.pop_front() {
        stack.push(Cont::SuperCollectArgs {
            selector,
            done,
            pending,
            env: env.clone(),
        });
        stack.push(Cont::EvalExpr { expr: next, env });
    } else {
        bump_reductions(reductions);
        if let Err(e) = dispatch_super_to_stack(ev, selector, done, &mut env, stack, acc) {
            handle_step_error(e, ev, stack, acc)?;
        }
    }
    Ok(())
}

/// `Cont::CascadeStep` handler: send the next cascaded message to the shared
/// receiver, then continue with the remaining messages. Extracted `#[inline(never)]`.
#[inline(never)]
fn handle_cascade_step(
    ev: &Evaluator,
    receiver: Option<Value>,
    mut pending: VecDeque<Message>,
    mut env: Env,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    // On first fire, acc = evaluated receiver; on subsequent fires, acc = send result.
    let recv = receiver.unwrap_or_else(|| acc.clone());
    if let Some(msg) = pending.pop_front() {
        // Send this message to recv, then continue with remaining pending.
        stack.push(Cont::CascadeStep {
            receiver: Some(recv.clone()),
            pending,
            env: env.clone(),
        });
        match msg {
            Message::Unary(sel) => {
                if let Err(e) = dispatch_to_stack(ev, recv, sel, vec![], &mut env, stack, acc) {
                    handle_step_error(e, ev, stack, acc)?;
                }
            }
            Message::Binary { op, arg } => {
                stack.push(Cont::CollectArgs {
                    receiver: recv,
                    selector: op,
                    done: vec![],
                    pending: VecDeque::new(),
                    env: env.clone(),
                });
                stack.push(Cont::EvalExpr { expr: *arg, env });
            }
            Message::Keyword { parts } => {
                let selector: Arc<str> = parts
                    .iter()
                    .map(|(k, _)| k.as_ref())
                    .collect::<String>()
                    .into();
                let mut exprs: VecDeque<Expr> = parts.into_iter().map(|(_, e)| e).collect();
                let first = exprs.pop_front().expect("keyword message has no parts");
                stack.push(Cont::CollectArgs {
                    receiver: recv,
                    selector,
                    done: vec![],
                    pending: exprs,
                    env: env.clone(),
                });
                stack.push(Cont::EvalExpr { expr: first, env });
            }
        }
    }
    // else: pending empty — acc already holds last send result, nothing to push.
    Ok(())
}

/// Increment the cooperative-scheduling reduction counter, yielding to the
/// `may` scheduler once `YIELD_INTERVAL` is reached.
#[inline]
fn bump_reductions(reductions: &mut u32) {
    *reductions += 1;
    if *reductions >= YIELD_INTERVAL {
        *reductions = 0;
        may::coroutine::yield_now();
    }
}

/// Process one statement, pushing continuations onto `stack`.
///
/// Requires: `stack` already contains the enclosing `CallFrame` at the top.
fn dispatch_stmt(
    ev: &Evaluator,
    stmt: Stmt,
    env: &Env,
    frame_depth: usize,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    match stmt {
        Stmt::LocalDecl(_) => {
            // Pre-bound during frame setup; nothing to do at runtime.
        }
        Stmt::Expr(expr) => {
            stack.push(Cont::EvalExpr {
                expr,
                env: env.clone(),
            });
        }
        Stmt::Assign { name, value } => {
            stack.push(Cont::Assign {
                name,
                env: env.clone(),
            });
            stack.push(Cont::EvalExpr {
                expr: value,
                env: env.clone(),
            });
        }
        Stmt::Return(expr) => {
            // The target depth for a ^-return is the block home, or this frame's
            // depth when not inside any block.
            let target_depth = ev.current_block_home.get().unwrap_or(frame_depth);
            stack.push(Cont::ReturnUnwind { target_depth });
            stack.push(Cont::EvalExpr {
                expr,
                env: env.clone(),
            });
        }
        Stmt::MethodDef(def) => {
            install_method_def(ev, def, env)?;
            *acc = Value::Nil;
        }
    }
    Ok(())
}

/// Push a block evaluation frame onto the iterative stack.
///
/// Requires: `args.len() == block.params.len()`.
/// Ensures: equivalent to `Evaluator::eval_block` but reuses the caller's continuation
///   stack; no nested `step_loop` call is made. When `sync=true`, a `ForceAcc`
///   continuation is inserted after each statement so `Value::Future` results are
///   resolved before the next statement runs.
pub(crate) fn push_block_frame(
    ev: &Evaluator,
    block: &Arc<crate::value::Block>,
    args: Vec<Value>,
    sync: bool,
    stack: &mut Vec<Cont>,
) -> Result<(), EvalError> {
    use crate::ast::Stmt;
    if args.len() != block.params.len() {
        return Err(EvalError::ArityMismatch {
            expected: block.params.len(),
            got: args.len(),
        });
    }
    let mut block_env = block.env.clone();
    block_env.push_frame();
    for name in &block.locals {
        block_env.define(name.clone(), Value::Nil);
    }
    for (name, val) in block.params.iter().zip(args) {
        block_env.define(name.clone(), val);
    }
    for stmt in &block.body {
        if let Stmt::LocalDecl(names) = stmt {
            for name in names {
                block_env.define(name.clone(), Value::Nil);
            }
        }
    }
    // If the block captured a home instance, push it onto method_stack so
    // lookup_var finds ivars even after TCO has removed the enclosing method frame.
    if let Some(ref handle) = block.home_self {
        ev.push_self(SelfBinding::Instance {
            handle: handle.clone(),
            defining_class: handle.0.class.clone(),
        });
    }
    let saved_home = ev.current_block_home.get();
    ev.current_block_home.set(Some(block.home_depth));
    stack.push(Cont::BlockFrame {
        pending: block.body.iter().cloned().collect(),
        env: block_env,
        saved_block_home: saved_home,
        block_home_self: block.home_self.clone(),
        sync,
    });
    Ok(())
}

/// `true` if the top of `stack` is an empty `BlockFrame` or `ProgramFrame`.
fn top_is_empty_nonmethod(stack: &[Cont]) -> bool {
    matches!(
        stack.last(),
        Some(Cont::BlockFrame { pending, .. } | Cont::ProgramFrame { pending, .. })
            if pending.is_empty()
    )
}

/// Drain tail-call frame patterns from the stack before pushing a new callee.
///
/// Requires: none (safe to call on any stack state; no-op if no pattern matches).
/// Ensures:
///   (1) All empty block/program frames at the top are drained (teardown run).
///   (2) If the top is then `ReturnUnwind(D)` and, scanning past any further empty
///       block/program frames, the next is an empty `MethodFrame` at depth D, the
///       entire sequence (ReturnUnwind + intervening frames + method frame) is drained.
///   (3) Steps (1)–(2) repeat until no further pattern applies.
///   Safe: empty block/program frames return `acc` unchanged; `ReturnUnwind(D)` would
///   have overridden the method's implicit self-return with the same value the new
///   callee will produce.
fn drain_tail_frames(ev: &Evaluator, stack: &mut Vec<Cont>) {
    loop {
        // Phase 1: drain leading empty block/program frames.
        while top_is_empty_nonmethod(stack) {
            let frame = stack.pop().unwrap();
            teardown_frame(ev, frame);
        }

        // Phase 2: check for ReturnUnwind(D) followed (past empty block/program
        // frames) by an empty MethodFrame at depth D.
        let target = match stack.last() {
            Some(Cont::ReturnUnwind { target_depth }) => *target_depth,
            _ => break,
        };

        // Scan downward past empty block/program frames to find the matching method frame.
        let mut scan_idx = stack.len().checked_sub(2);
        let mut intermediate = 0usize;
        let found_method = loop {
            match scan_idx {
                None => break false,
                Some(idx) => match stack.get(idx) {
                    Some(Cont::BlockFrame { pending, .. } | Cont::ProgramFrame { pending, .. })
                        if pending.is_empty() =>
                    {
                        intermediate += 1;
                        scan_idx = idx.checked_sub(1);
                    }
                    Some(Cont::MethodFrame { depth, pending, .. })
                        if *depth == target && pending.is_empty() =>
                    {
                        break true;
                    }
                    _ => break false,
                },
            }
        };

        if !found_method {
            break;
        }

        // Drain: ReturnUnwind, then intermediate empty frames, then the method frame.
        stack.pop(); // ReturnUnwind
        for _ in 0..intermediate {
            if let Some(frame) = stack.pop() {
                teardown_frame(ev, frame);
            }
        }
        if let Some(frame) = stack.pop() {
            debug_assert!(matches!(frame, Cont::MethodFrame { .. }));
            teardown_frame(ev, frame);
        }
        // Repeat: there may be another drainable layer above.
    }
}

/// Dispatch a message send to the iterative stack.
///
/// Requires: `args` are fully evaluated in left-to-right order.
/// Ensures:
///   (1) User-defined methods for Block/Int/Nil/ValueObject/Entity-self-send found
///       via `lookup_user_method` → TCO drain + `push_method_call_frame`.
///   (2) `Block >> value` family → `push_block_frame(sync=false)`.
///   (3) `Block >> syncValue` → `push_block_frame(sync=true)`.
///   (4) `Block >> ensure:` → push `EnsureGuard`, then `push_block_frame(sync=true)` for body.
///   (5) `Block >> on:do:` → push `OnDoCatch`, then `push_block_frame(sync=true)` for body.
///   (6) `Bool >> ifTrue:` family → `push_block_frame(false)` for selected branch.
///   (7) Universal nil-guard family (`ifNil:` / `ifNotNil:` / combined) → inline.
///   (8) All other selectors → `builtins::dispatch`.
/// Push a `CallFrame` for a class-side method invocation.
///
/// Requires: `args.len() == method.pattern.params().len()`.
/// Ensures: a `CallFrame` with `SelfBinding::ClassSide` is pushed onto `stack`.
fn push_class_method_frame(
    ev: &Evaluator,
    handle: InstanceHandle,
    def_class: Arc<Class>,
    method: Arc<Method>,
    args: Vec<Value>,
    env: &Env,
    stack: &mut Vec<Cont>,
) -> Result<(), EvalError> {
    let params = method.pattern.params();
    if args.len() != params.len() {
        return Err(EvalError::ArityMismatch {
            expected: params.len(),
            got: args.len(),
        });
    }
    let self_val = Value::Instance(handle.clone());
    let mut method_env = env.clone();
    method_env.push_frame();
    method_env.define(self_sym(), self_val.clone());
    for (param, val) in params.iter().zip(args.iter()) {
        method_env.define(param.clone(), val.clone());
    }
    for local in &method.locals {
        method_env.define(local.clone(), Value::Nil);
    }
    for stmt in &method.body {
        if let Stmt::LocalDecl(names) = stmt {
            for name in names {
                method_env.define(name.clone(), Value::Nil);
            }
        }
    }
    let binding = SelfBinding::ClassSide {
        handle,
        defining_class: def_class.clone(),
    };
    ev.push_self(binding);
    let depth = ev.method_depth.get();
    let saved_block_home = ev.current_block_home.get();
    ev.current_block_home.set(None);
    stack.push(Cont::MethodFrame {
        depth,
        pending: method.body.iter().cloned().collect(),
        env: method_env,
        implicit_self: self_val,
        saved_block_home,
    });
    Ok(())
}

/// Dispatch a super send iteratively: look up `selector` starting from the
/// superclass of the current defining class and push the method frame.
///
/// Requires: called from within a method context (`ev.method_stack` non-empty).
/// Ensures: for a found user method, pushes a `CallFrame` and returns `Ok(())`.
///   For a class-side miss, falls through to `dispatch_class_builtin` and sets `*acc`.
///   For an instance/primitive miss, returns `MessageNotUnderstood`.
fn dispatch_super_to_stack(
    ev: &Evaluator,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    let defining_class = ev
        .current_defining_class()
        .ok_or_else(|| EvalError::TypeError("super used outside method context".into()))?;
    let superclass = defining_class
        .superclass
        .as_ref()
        .ok_or_else(|| {
            EvalError::TypeError(format!(
                "super sent in {} which has no superclass",
                defining_class.name
            ))
        })?
        .clone();

    let top = ev.method_stack.borrow().last().cloned();
    match top {
        Some(SelfBinding::Instance { handle, .. }) => match superclass.lookup(&selector) {
            Some((method, def_class)) => {
                drain_tail_frames(ev, stack);
                push_method_call_frame(
                    ev,
                    UserMethodFound::Instance {
                        handle,
                        method,
                        def_class,
                    },
                    args,
                    env,
                    stack,
                )?;
            }
            None => {
                return Err(EvalError::MessageNotUnderstood {
                    receiver_kind: "Object",
                    selector,
                });
            }
        },
        Some(SelfBinding::ClassSide { handle, .. }) => {
            match superclass.lookup_class_method(&selector) {
                Some((method, def_class)) => {
                    drain_tail_frames(ev, stack);
                    push_class_method_frame(ev, handle, def_class, method, args, env, stack)?;
                }
                None => {
                    *acc =
                        crate::builtins::dispatch_class_builtin(ev, handle, selector, args, env)?;
                }
            }
        }
        Some(SelfBinding::Primitive { self_value, .. }) => match superclass.lookup(&selector) {
            Some((method, def_class)) => {
                drain_tail_frames(ev, stack);
                push_method_call_frame(
                    ev,
                    UserMethodFound::Primitive {
                        self_value,
                        method,
                        def_class,
                    },
                    args,
                    env,
                    stack,
                )?;
            }
            None => {
                return Err(EvalError::MessageNotUnderstood {
                    receiver_kind: "nil",
                    selector,
                });
            }
        },
        None => {
            return Err(EvalError::TypeError(
                "super used outside method context".into(),
            ));
        }
    }
    Ok(())
}

/// Flattened signal dispatch for `_primSignalCondition:` / `error:`.
///
/// Requires: `condition` is forced.
/// Ensures: if a handler in `ev.handler_chain` matches, it runs on the heap
///   `Cont` stack behind a `SignalGuard`, with itself and all inner handlers
///   removed from the chain for its duration (Common-Lisp handler-scoping) so a
///   re-signal defers to an outer handler. Its result becomes the signal
///   expression's value (resume). If no handler matches, the unhandled policy is
///   applied to `*acc` (Error → propagated `EvalError::Signal`). No recursive
///   `step_loop` is entered.
fn dispatch_signal(
    ev: &Evaluator,
    condition: Value,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    let cond_class = match &condition {
        Value::Instance(h) => h.0.class.clone(),
        _ => return Err(EvalError::Signal(condition)),
    };
    let idx = ev
        .handler_chain
        .borrow()
        .iter()
        .rposition(|(hc, _)| Class::is_subclass_of(&cond_class, hc));
    match idx {
        Some(i) => {
            let (handler_blk, removed) = {
                let mut chain = ev.handler_chain.borrow_mut();
                let removed = chain.split_off(i); // removed[0] = matched pair
                (removed[0].1.clone(), removed)
            };
            stack.push(Cont::SignalGuard { removed });
            push_block_frame(ev, &handler_blk, vec![condition], false, stack)?;
            Ok(())
        }
        None => {
            *acc = crate::builtins_iterative::apply_unhandled_policy(ev, condition)?;
            Ok(())
        }
    }
}

fn dispatch_to_stack(
    ev: &Evaluator,
    receiver: Value,
    selector: Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    // `then:` must see the raw receiver (including Futures) so that
    // `Future then: [blk]` chains asynchronously without eager resolution.
    //
    // The synchronous arms (resolved-value receiver, or block receiver that
    // yields a non-future) run the continuation on the step-loop `Cont` stack
    // rather than via a recursive `eval_block`, so deep `then:` chains do not
    // grow the Rust coroutine stack. The genuine-Future arm spawns a
    // continuation coroutine (fresh stack), delegated to the builtins handler.
    //
    // This early interception (before `force_value` below) exists *only* to avoid
    // eagerly forcing a raw `Value::Future` receiver — chaining is the whole point.
    // A `Value::Instance` (e.g. the explicit `Future`) is never force-blocked, so it
    // falls through to ordinary user-method lookup, letting `Future >> then:`
    // delegate to its handle. That keeps `then:` symmetric with `await`.
    if selector.as_ref() == "then:" && !matches!(&receiver, Value::Instance(_)) {
        if args.len() != 1 {
            return Err(EvalError::ArityMismatch {
                expected: 1,
                got: args.len(),
            });
        }
        let cont_blk = as_block(args.into_iter().next().unwrap(), &selector)?;
        match receiver {
            // Genuine future: async spawn (no Rust stack growth).
            Value::Future(_) => {
                *acc = crate::builtins::dispatch(
                    ev,
                    receiver,
                    selector,
                    vec![Value::Block(cont_blk)],
                    env,
                )?;
            }
            // Block receiver: evaluate it on the step-loop stack (unforced, so a
            // future result is visible), then `ThenCheck` decides.
            Value::Block(ref blk) => {
                stack.push(Cont::ThenCheck {
                    cont_blk,
                    env: env.clone(),
                });
                push_block_frame(ev, blk, vec![], false, stack)?;
            }
            // Already-resolved value: run the continuation on the step-loop stack.
            _ => {
                push_block_frame(ev, &cont_blk, vec![], false, stack)?;
            }
        }
        return Ok(());
    }

    // Force a Future receiver before dispatch. We must know the actual type to
    // look up a method. Blocking here is a cooperative yield in the may scheduler
    // — other coroutines (entities) run while this one waits for resolution.
    let receiver = force_value(receiver)?;

    // (0) `perform:` / `perform:withArguments:` — reflective send. Re-dispatch the
    // target selector on the *same* Cont stack so forwarding stays O(1) in Rust
    // stack (no `step_loop`/`eval_block` recursion). Placed after the receiver
    // force (so a Future receiver forwards to its resolved value) and before user
    // lookup so it is authoritative. Arity of the target selector is validated by
    // the re-dispatch itself.
    if selector.as_ref() == "perform:" || selector.as_ref() == "perform:withArguments:" {
        let is_with_args = selector.as_ref() == "perform:withArguments:";
        let expected = if is_with_args { 2 } else { 1 };
        if args.len() != expected {
            return Err(EvalError::ArityMismatch {
                expected,
                got: args.len(),
            });
        }
        let target_sel = match force_value(args.remove(0))? {
            Value::Symbol(s) => s,
            _ => {
                return Err(EvalError::TypeError(
                    "perform:: selector must be a Symbol".into(),
                ));
            }
        };
        let target_args = if is_with_args {
            crate::builtins_iterative::cons_to_vec(args.remove(0))?
        } else {
            vec![]
        };
        return dispatch_to_stack(ev, receiver, target_sel, target_args, env, stack, acc);
    }

    // (1) User-defined method lookup (covers Bool, Block, Int, Nil, ValueObject, entity self-send).
    if let Some(found) = lookup_user_method(ev, &receiver, &selector) {
        drain_tail_frames(ev, stack);
        push_method_call_frame(ev, found, args, env, stack)?;
        return Ok(());
    }

    // (1b) `_primHCRun:` flattened: register this HandlerContext's handler pairs
    // on the handler_chain and run the body on the heap `Cont` stack (via
    // HCRunGuard) instead of recursing `step_loop` through `eval_block`. This is
    // what makes nested synchronous `run:` O(1) in Rust stack.
    if selector.as_ref() == "_primHCRun:"
        && let Value::Instance(ref handle) = receiver
    {
        if args.len() != 1 {
            return Err(EvalError::ArityMismatch {
                expected: 1,
                got: args.len(),
            });
        }
        let body = as_block(args.into_iter().next().unwrap(), &selector)?;
        let pairs = crate::builtins_iterative::extract_handler_pairs(handle)?;
        let chain_start_len = ev.handler_chain.borrow().len();
        {
            let mut chain = ev.handler_chain.borrow_mut();
            for (cls, blk) in &pairs {
                chain.push((cls.clone(), blk.clone()));
            }
        }
        stack.push(Cont::HCRunGuard {
            chain_start_len,
            body_blk: body.clone(),
        });
        push_block_frame(ev, &body, vec![], false, stack)?;
        return Ok(());
    }

    // (1c) `to:do:` / `to:by:do:` flattened: drive the numeric loop on the heap
    // `Cont` stack (ToDoStep) instead of recursing `step_loop` via `eval_block`
    // per iteration. All collection iteration bottoms out here, so this makes
    // nested iteration O(1) in Rust stack.
    if let Value::Int(n) = receiver {
        match selector.as_ref() {
            "to:do:" => {
                if args.len() != 2 {
                    return Err(EvalError::ArityMismatch {
                        expected: 2,
                        got: args.len(),
                    });
                }
                let stop = crate::builtins_iterative::as_int(args.remove(0), &selector)?;
                let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
                stack.push(Cont::ToDoStep {
                    blk,
                    next: n,
                    stop,
                    step: 1,
                });
                return Ok(());
            }
            "to:by:do:" => {
                if args.len() != 3 {
                    return Err(EvalError::ArityMismatch {
                        expected: 3,
                        got: args.len(),
                    });
                }
                let stop = crate::builtins_iterative::as_int(args.remove(0), &selector)?;
                let step = crate::builtins_iterative::as_int(args.remove(0), &selector)?;
                if step == 0 {
                    return Err(EvalError::TypeError(
                        "to:by:do: step must be non-zero".into(),
                    ));
                }
                let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
                stack.push(Cont::ToDoStep {
                    blk,
                    next: n,
                    stop,
                    step,
                });
                return Ok(());
            }
            _ => {}
        }
    }

    // (1d) Signal dispatch flattened: run the matching handler on the heap `Cont`
    // stack (SignalGuard) with Common-Lisp handler-scoping instead of recursing
    // `step_loop` via `eval_block`. Covers the canonical `signal` →
    // `self _primSignalCondition: self` path and object-receiver `error:`.
    //
    // Gated on an Instance receiver to preserve the primitive path exactly:
    // `error:` on a primitive uses `dispatch_primitive_fallback` (a direct
    // `EvalError::Signal` that bypasses the handler chain, with a different
    // message). `_primSignalCondition:` is only ever sent to a condition instance.
    if matches!(receiver, Value::Instance(_)) {
        if selector.as_ref() == "_primSignalCondition:" {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    expected: 1,
                    got: args.len(),
                });
            }
            let condition = force_value(args.into_iter().next().unwrap())?;
            if !matches!(&condition, Value::Instance(_)) {
                return Err(EvalError::TypeError(
                    "_primSignalCondition: requires a Condition instance".into(),
                ));
            }
            return dispatch_signal(ev, condition, stack, acc);
        }
        if selector.as_ref() == "error:" {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    expected: 1,
                    got: args.len(),
                });
            }
            let msg = match force_value(args.into_iter().next().unwrap())? {
                Value::Str(s) => s,
                other => Arc::from(format!("error: argument was {}", other.kind()).as_str()),
            };
            let condition = crate::builtins_iterative::make_error_value(&ev.registry, &msg);
            return dispatch_signal(ev, condition, stack, acc);
        }
    }

    // (1e) `_primNewValue:` / `_primNewValueIndexed:init:` flattened: run the init
    // block on the heap `Cont` stack (NewValueGuard) instead of recursing
    // `step_loop` via `eval_block`, so object construction is O(1) Rust stack.
    if let Value::Instance(ref handle) = receiver
        && matches!(handle.0.kind, InstanceKind::ClassEntity(_))
    {
        match selector.as_ref() {
            "_primNewValue:" => {
                if args.len() != 1 {
                    return Err(EvalError::ArityMismatch {
                        expected: 1,
                        got: args.len(),
                    });
                }
                let class = handle.0.class.clone();
                if !class.inherits_is_value() {
                    return Err(EvalError::TypeError(
                        "_primNewValue: requires a Value subclass".into(),
                    ));
                }
                let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
                let indexed = if class.indexed_slot_kind.is_some() {
                    Some(0)
                } else {
                    None
                };
                let inst = crate::builtins_iterative::make_value_instance(class, indexed);
                stack.push(Cont::NewValueGuard { inst: inst.clone() });
                push_block_frame(ev, &blk, vec![Value::Instance(inst)], false, stack)?;
                return Ok(());
            }
            "_primNewValueIndexed:init:" => {
                if args.len() != 2 {
                    return Err(EvalError::ArityMismatch {
                        expected: 2,
                        got: args.len(),
                    });
                }
                let class = handle.0.class.clone();
                if !class.inherits_is_value() {
                    return Err(EvalError::TypeError(
                        "_primNewValueIndexed:init: requires a Value subclass".into(),
                    ));
                }
                if class.indexed_slot_kind.is_none() {
                    return Err(EvalError::TypeError(
                        "_primNewValueIndexed:init: requires class to declare indexedSlots:".into(),
                    ));
                }
                let n = crate::builtins_iterative::as_int(args.remove(0), &selector)?;
                let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
                let inst =
                    crate::builtins_iterative::make_value_instance(class, Some(n.max(0) as usize));
                stack.push(Cont::NewValueGuard { inst: inst.clone() });
                push_block_frame(ev, &blk, vec![Value::Instance(inst)], false, stack)?;
                return Ok(());
            }
            _ => {}
        }
    }

    // (1f) Class-side user method (synchronous case) flattened: push a class
    // method `CallFrame` instead of recursing `step_loop` via
    // `invoke_class_method`. Mirrors the instance self-send path and replicates
    // `dispatch_class`'s sync condition; asynchronous (cross-entity behavioral)
    // class sends fall through to `builtins::dispatch` → `dispatch_class`, which
    // enqueues and returns a `Future`. Class-side builtins (no user method) also
    // fall through to `dispatch_class_builtin`.
    if let Value::Instance(ref handle) = receiver
        && let InstanceKind::ClassEntity(d) = &handle.0.kind
    {
        let sync = ev.current_entity_id.get() == Some(d.id)
            || selector.starts_with("subclass:")
            || handle.0.class.inherits_is_value()
            || matches!(selector.as_ref(), "=" | "~=");
        if sync && let Some((method, def_class)) = handle.0.class.lookup_class_method(&selector) {
            drain_tail_frames(ev, stack);
            push_class_method_frame(ev, handle.clone(), def_class, method, args, env, stack)?;
            return Ok(());
        }
    }

    // (1g) `Namespace do:` flattened: swap the active namespace and run the block
    // on the heap `Cont` stack (NamespaceGuard restores it on every exit path)
    // instead of recursing `step_loop` via `with_namespace` → `eval_block`.
    if let Value::Namespace(ref ns) = receiver
        && selector.as_ref() == "do:"
    {
        if args.len() != 1 {
            return Err(EvalError::ArityMismatch {
                expected: 1,
                got: args.len(),
            });
        }
        let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
        let saved_ns = ev.active_namespace.borrow().clone();
        *ev.active_namespace.borrow_mut() = ns.clone();
        stack.push(Cont::NamespaceGuard { saved_ns });
        push_block_frame(ev, &blk, vec![], false, stack)?;
        return Ok(());
    }

    // (2-5) Block dispatch.
    if let Value::Block(ref blk) = receiver {
        match selector.as_ref() {
            "value" | "value:" | "value:value:" | "value:value:value:" => {
                let expected: usize = match selector.as_ref() {
                    "value" => 0,
                    "value:" => 1,
                    "value:value:" => 2,
                    _ => 3,
                };
                if args.len() != expected {
                    return Err(EvalError::ArityMismatch {
                        expected,
                        got: args.len(),
                    });
                }
                push_block_frame(ev, blk, args, false, stack)?;
                return Ok(());
            }
            "syncValue" => {
                if !args.is_empty() {
                    return Err(EvalError::ArityMismatch {
                        expected: 0,
                        got: args.len(),
                    });
                }
                push_block_frame(ev, blk, vec![], true, stack)?;
                return Ok(());
            }
            "ensure:" => {
                if args.len() != 1 {
                    return Err(EvalError::ArityMismatch {
                        expected: 1,
                        got: args.len(),
                    });
                }
                let ensure_blk_val = args.into_iter().next().unwrap();
                let ensure_blk = as_block(ensure_blk_val, &selector)?;
                // EnsureGuard is pushed below body frame; fires when body completes.
                stack.push(Cont::EnsureGuard { ensure_blk });
                push_block_frame(ev, blk, vec![], true, stack)?;
                return Ok(());
            }
            "on:do:" => {
                if args.len() != 2 {
                    return Err(EvalError::ArityMismatch {
                        expected: 2,
                        got: args.len(),
                    });
                }
                let mut iter = args.into_iter();
                let catch_class_val = force_value(iter.next().unwrap())?;
                let catch_class = match &catch_class_val {
                    Value::Instance(h) => h.0.class.clone(),
                    _ => {
                        return Err(EvalError::TypeError(
                            "on:do: first argument must be a class".into(),
                        ));
                    }
                };
                let handler_blk = as_block(iter.next().unwrap(), &selector)?;
                let body_blk = blk.clone();
                // OnDoCatch is pushed below body frame; fires when body completes.
                stack.push(Cont::OnDoCatch {
                    catch_class,
                    handler_blk,
                    body_blk,
                });
                push_block_frame(ev, blk, vec![], true, stack)?;
                return Ok(());
            }
            _ => {}
        }
    }

    // (6) Universal nil-guard family — checked after user methods so user overrides win.
    match selector.as_ref() {
        "ifNil:" => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    expected: 1,
                    got: args.len(),
                });
            }
            let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
            let forced = force_value(receiver)?;
            if matches!(forced, Value::Nil) {
                push_block_frame(ev, &blk, vec![], false, stack)?;
            } else {
                *acc = Value::Nil;
            }
            return Ok(());
        }
        "ifNotNil:" => {
            if args.len() != 1 {
                return Err(EvalError::ArityMismatch {
                    expected: 1,
                    got: args.len(),
                });
            }
            let blk = as_block(args.into_iter().next().unwrap(), &selector)?;
            let forced = force_value(receiver)?;
            if !matches!(forced, Value::Nil) {
                push_block_frame(ev, &blk, vec![forced], false, stack)?;
            } else {
                *acc = Value::Nil;
            }
            return Ok(());
        }
        "ifNil:ifNotNil:" => {
            if args.len() != 2 {
                return Err(EvalError::ArityMismatch {
                    expected: 2,
                    got: args.len(),
                });
            }
            let mut iter = args.into_iter();
            let nil_blk = as_block(iter.next().unwrap(), &selector)?;
            let not_nil_blk = as_block(iter.next().unwrap(), &selector)?;
            let forced = force_value(receiver)?;
            if matches!(forced, Value::Nil) {
                push_block_frame(ev, &nil_blk, vec![], false, stack)?;
            } else {
                push_block_frame(ev, &not_nil_blk, vec![forced], false, stack)?;
            }
            return Ok(());
        }
        "ifNotNil:ifNil:" => {
            if args.len() != 2 {
                return Err(EvalError::ArityMismatch {
                    expected: 2,
                    got: args.len(),
                });
            }
            let mut iter = args.into_iter();
            let not_nil_blk = as_block(iter.next().unwrap(), &selector)?;
            let nil_blk = as_block(iter.next().unwrap(), &selector)?;
            let forced = force_value(receiver)?;
            if matches!(forced, Value::Nil) {
                push_block_frame(ev, &nil_blk, vec![], false, stack)?;
            } else {
                push_block_frame(ev, &not_nil_blk, vec![forced], false, stack)?;
            }
            return Ok(());
        }
        _ => {}
    }

    // (7) All other selectors fall through to builtins. On a
    // `MessageNotUnderstood` miss, give the receiver a chance to handle the send
    // via `doesNotUnderstand:` (reified-message forwarding): reify the failed
    // send as a `Message` and invoke the method on the same Cont stack (no Rust
    // recursion). The default `Object >> doesNotUnderstand:` signals a
    // `MessageNotUnderstood` condition, so an unhandled miss still surfaces as an
    // error. Only receivers that route through `lookup_user_method`
    // (ValueObject/Entity/Int/Bool/Nil/Block) get the hook; primitive types that
    // return `None` there (Str/Symbol/Float/LargeInt) re-raise the raw miss.
    match crate::builtins::dispatch(ev, receiver.clone(), selector.clone(), args.clone(), env) {
        Ok(v) => {
            *acc = v;
            Ok(())
        }
        Err(e @ EvalError::MessageNotUnderstood { .. }) => {
            let dnu: Arc<str> = Arc::from("doesNotUnderstand:");
            if let Some(found) = lookup_user_method(ev, &receiver, &dnu) {
                let msg = crate::builtins_iterative::build_message(ev, selector, args)?;
                drain_tail_frames(ev, stack);
                push_method_call_frame(ev, found, vec![msg], env, stack)?;
                Ok(())
            } else {
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}

/// Build and push a `CallFrame` for a user-defined method call.
///
/// Requires: `args.len() == method.pattern.params().len()`.
/// Ensures: a `CallFrame` is pushed onto `stack` with the method env set up;
///   `ev.method_stack` is updated (push_self called).
pub(crate) fn push_method_call_frame(
    ev: &Evaluator,
    found: UserMethodFound,
    args: Vec<Value>,
    env: &Env,
    stack: &mut Vec<Cont>,
) -> Result<(), EvalError> {
    match found {
        UserMethodFound::Instance {
            handle,
            method,
            def_class,
        } => {
            let params = method.pattern.params();
            if args.len() != params.len() {
                return Err(EvalError::ArityMismatch {
                    expected: params.len(),
                    got: args.len(),
                });
            }
            let self_value = Value::Instance(handle.clone());
            let mut method_env = env.clone();
            method_env.push_frame();
            method_env.define(self_sym(), self_value.clone());
            for (param, val) in params.iter().zip(args.iter()) {
                method_env.define(param.clone(), val.clone());
            }
            for local in &method.locals {
                method_env.define(local.clone(), Value::Nil);
            }
            // Pre-bind LocalDecl names (eval_program pre-binding equivalent).
            for stmt in &method.body {
                if let Stmt::LocalDecl(names) = stmt {
                    for name in names {
                        method_env.define(name.clone(), Value::Nil);
                    }
                }
            }
            let binding = SelfBinding::Instance {
                handle,
                defining_class: def_class,
            };
            ev.push_self(binding);
            let depth = ev.method_depth.get();
            let saved_block_home = ev.current_block_home.get();
            ev.current_block_home.set(None);
            stack.push(Cont::MethodFrame {
                depth,
                pending: method.body.iter().cloned().collect(),
                env: method_env,
                implicit_self: self_value,
                saved_block_home,
            });
        }
        UserMethodFound::Primitive {
            self_value,
            method,
            def_class,
        } => {
            let params = method.pattern.params();
            if args.len() != params.len() {
                return Err(EvalError::ArityMismatch {
                    expected: params.len(),
                    got: args.len(),
                });
            }
            let mut method_env = env.clone();
            method_env.push_frame();
            method_env.define(self_sym(), self_value.clone());
            for (param, val) in params.iter().zip(args.iter()) {
                method_env.define(param.clone(), val.clone());
            }
            for local in &method.locals {
                method_env.define(local.clone(), Value::Nil);
            }
            for stmt in &method.body {
                if let Stmt::LocalDecl(names) = stmt {
                    for name in names {
                        method_env.define(name.clone(), Value::Nil);
                    }
                }
            }
            let binding = SelfBinding::Primitive {
                defining_class: def_class,
                self_value: self_value.clone(),
            };
            ev.push_self(binding);
            let depth = ev.method_depth.get();
            let saved_block_home = ev.current_block_home.get();
            ev.current_block_home.set(None);
            stack.push(Cont::MethodFrame {
                depth,
                pending: method.body.iter().cloned().collect(),
                env: method_env,
                implicit_self: self_value,
                saved_block_home,
            });
        }
    }
    Ok(())
}

/// Install a method definition into its class, mirroring `eval_stmt` in eval.rs.
fn install_method_def(ev: &Evaluator, def: MethodDef, env: &Env) -> Result<(), EvalError> {
    use crate::entity::InstanceKind;
    let class = match ev.lookup_var(&def.class_name, env) {
        Some(Value::Instance(h)) if matches!(h.0.kind, InstanceKind::ClassEntity(_)) => {
            h.0.class.clone()
        }
        Some(_) => {
            return Err(EvalError::TypeError(format!(
                "{} is not a class",
                def.class_name
            )));
        }
        None => return Err(EvalError::UndefinedVariable(def.class_name.clone())),
    };
    check_prim_sends_in_body(&def.body)?;
    let method = Arc::new(Method {
        pattern: def.pattern.clone(),
        locals: def.locals.clone(),
        body: def.body.clone(),
    });
    let selector = def.pattern.selector();
    if def.is_class_side {
        class.class_methods.write().insert(selector, method);
    } else {
        class.methods.write().insert(selector, method);
    }
    Ok(())
}

/// Allocate an Array ValueObject with `elements` as indexed slots.
fn build_array(array_class: Arc<Class>, elements: Vec<Value>) -> Value {
    use crate::entity::{InstanceHandle, InstanceKind, InstanceState};
    use crate::object::{IndexedStorage, Object};
    use std::sync::Arc as StdArc;
    let obj = StdArc::new(Object::new_with_indexed(
        array_class.clone(),
        elements.len(),
    ));
    if let Some(IndexedStorage::Pointer(slots)) = obj.indexed.as_ref() {
        let mut guard = slots.lock();
        for (i, v) in elements.into_iter().enumerate() {
            guard[i] = v;
        }
    }
    Value::Instance(InstanceHandle(StdArc::new(InstanceState {
        class: array_class,
        kind: InstanceKind::ValueObject(obj),
    })))
}

/// Evaluate an `ArrayLiteral` (all elements are `Literal`s) synchronously.
///
/// Delegates to `ev.eval_literal_as_value` for each element, which handles
/// nested array literals recursively (bounded by the literal structure).
fn eval_array_literal_iterative(
    ev: &Evaluator,
    elements: &[crate::ast::Literal],
    env: &mut Env,
) -> Result<Value, EvalError> {
    let array_handle = ev
        .registry
        .kernel()
        .lookup_class("Array")
        .ok_or_else(|| EvalError::UndefinedVariable(Arc::from("Array")))?;
    let array_class = array_handle.0.class.clone();
    let mut values = Vec::with_capacity(elements.len());
    for lit in elements {
        values.push(ev.eval_literal_as_value(lit, env)?);
    }
    Ok(build_array(array_class, values))
}

#[cfg(test)]
#[path = "tests/eval_iterative.rs"]
mod tests;
