// C30 will promote IEvaluator to the default evaluator; until then all items
// here are only reachable via test_helpers and are flagged as dead by clippy.
#![allow(dead_code)]

/// One reduction = one completed message dispatch. Matches BEAM's "one reduction
/// per function call" granularity. At ~1M msg/s this yields every ~1 ms.
const YIELD_INTERVAL: u32 = 1024;
use std::collections::VecDeque;
use std::sync::Arc;

use crate::ast::{Expr, Message, MethodDef, Stmt};
use crate::builtins::{UserMethodFound, as_block, force_value, lookup_user_method};
use crate::class::{Class, Method};
use crate::entity::InstanceHandle;
use crate::env::Env;
use crate::eval::{
    EvalError, Evaluator, SelfBinding, check_prim_sends_in_body, eval_literal, self_sym,
};
use crate::value::Value;

pub struct IEvaluator {
    pub inner: Arc<Evaluator>,
}

impl IEvaluator {
    pub fn new(ev: Arc<Evaluator>) -> Self {
        Self { inner: ev }
    }

    /// Iterative replacement for `Evaluator::eval_program`.
    ///
    /// Requires: `env` has at least one live frame; all `LocalDecl` names in
    ///   `stmts` are pre-declared before any expression is evaluated.
    /// Ensures: semantically identical to `Evaluator::eval_program`; Smol method
    ///   calls within the body use heap-allocated `CallFrame`s rather than Rust
    ///   stack frames.
    pub fn eval_program(&self, stmts: &[Stmt], env: &mut Env) -> Result<Value, EvalError> {
        for stmt in stmts {
            if let Stmt::LocalDecl(names) = stmt {
                for name in names {
                    env.define(name.clone(), Value::Nil);
                }
            }
        }
        let frame = Cont::CallFrame {
            self_binding: None,
            depth: 0,
            pending: stmts.iter().cloned().collect(),
            env: env.clone(),
            implicit_return: Value::Nil,
            is_method: false,
            pops_env: false,
            saved_block_home: self.inner.current_block_home.get(),
            frame_block_home: None,
            defining_class: None,
            sync: false,
        };
        step_loop(&self.inner, frame)
    }

    /// Iterative replacement for `Evaluator::eval_block`.
    ///
    /// Requires: `args.len() == block.params.len()`.
    /// Ensures: semantically identical to `Evaluator::eval_block`; method calls
    ///   within the body use heap-allocated `CallFrame`s.
    pub fn eval_block(
        &self,
        block: &crate::value::Block,
        args: Vec<Value>,
        _outer_env: &mut Env,
    ) -> Result<Value, EvalError> {
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
        // Pre-bind LocalDecl names.
        for stmt in &block.body {
            if let Stmt::LocalDecl(names) = stmt {
                for name in names {
                    block_env.define(name.clone(), Value::Nil);
                }
            }
        }
        let saved_home = self.inner.current_block_home.get();
        let frame = Cont::CallFrame {
            self_binding: None,
            depth: 0,
            pending: block.body.iter().cloned().collect(),
            env: block_env,
            implicit_return: Value::Nil,
            is_method: false,
            pops_env: true,
            saved_block_home: saved_home,
            frame_block_home: Some(block.home_depth),
            defining_class: None,
            sync: false,
        };
        self.inner.current_block_home.set(Some(block.home_depth));
        // current_block_home is restored by the CallFrame when it pops.
        step_loop(&self.inner, frame)
    }

    /// Iterative replacement for `Evaluator::eval_block_sync`.
    ///
    /// Requires: `args.len() == block.params.len()`.
    /// Ensures: each statement result is force-resolved before the next runs.
    pub fn eval_block_sync(
        &self,
        block: &crate::value::Block,
        args: Vec<Value>,
        outer_env: &mut Env,
    ) -> Result<Value, EvalError> {
        // For C29, delegate to eval_block (sync force-value is applied by builtins
        // calling eval_block_sync; the sync wrapper is in the builtin, not here).
        self.eval_block(block, args, outer_env)
    }

    /// Iterative replacement for `Evaluator::invoke_method`.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: semantically identical to `Evaluator::invoke_method`; method calls
    ///   within the body use heap-allocated `CallFrame`s.
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
        push_method_call_frame(&self.inner, found, args, env, &mut stack)?;
        let initial = stack
            .pop()
            .expect("push_method_call_frame must push a frame");
        step_loop(&self.inner, initial)
    }

    /// Iterative replacement for `Evaluator::invoke_class_method`.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: semantically identical to `Evaluator::invoke_class_method`.
    pub fn invoke_class_method(
        &self,
        handle: InstanceHandle,
        defining_class: Arc<Class>,
        method: Arc<Method>,
        args: Vec<Value>,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
        // Class-side dispatch: use the recursive path for C29.
        // C30 will promote IEvaluator to be the default, at which point this
        // becomes the iterative path too.
        self.inner
            .invoke_class_method(handle, defining_class, method, args, env)
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

    /// An active method or block invocation frame.
    ///
    /// Pushed by `invoke_method` / `eval_block`. Popped when `pending`
    /// statements are exhausted or a matching `^`-return unwinds to it.
    CallFrame {
        /// For method frames: the `SelfBinding` pushed to `method_stack`.
        /// `None` for block frames (blocks do not push a self binding).
        self_binding: Option<SelfBinding>,
        /// Stack depth of this frame (for matching `EvalError::Return`).
        /// Zero for program-level and block frames.
        depth: usize,
        /// Remaining statements for this frame.
        pending: VecDeque<Stmt>,
        /// Environment for this frame.
        env: Env,
        /// Value returned when the frame exits without an explicit `^`.
        /// Methods: `Value::Instance(self)`. Blocks/program: last `acc`.
        implicit_return: Value,
        /// `true` for method frames (implicit return = self), `false` for
        /// blocks and program-level frames (implicit return = last acc).
        is_method: bool,
        /// Whether to call `env.pop_frame()` when this frame is popped.
        /// `true` for method and block frames (which called `push_frame` on setup);
        /// `false` for program-level frames (eval_program does not push a frame).
        pops_env: bool,
        /// `current_block_home` on the evaluator before this frame was entered;
        /// restored when the frame is popped.
        saved_block_home: Option<usize>,
        /// `current_block_home` to set for the duration of this frame.
        /// `None` for method frames (they reset home tracking to None).
        frame_block_home: Option<usize>,
        /// Defining class for super-send lookups within this frame (S6).
        defining_class: Option<Arc<Class>>,
        /// When `true`, `ForceAcc` is pushed after each statement so that
        /// `Value::Future` results are resolved before the next statement runs.
        /// Used by `syncValue`, `on:do:` body, and `ensure:` body.
        sync: bool,
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

/// Unwind heap `CallFrame`s until one with `depth == target_depth` is found,
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
            Cont::CallFrame {
                self_binding,
                depth,
                saved_block_home,
                mut env,
                is_method,
                pops_env,
                ..
            } => {
                if is_method {
                    debug_assert!(self_binding.is_some());
                    ev.pop_self();
                }
                ev.current_block_home.set(saved_block_home);
                if pops_env {
                    env.pop_frame();
                }
                if depth == target_depth {
                    return Ok(UnwindOutcome::Reached);
                }
            }
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
            _ => {} // OnDoCatch and other non-frame entries are discarded during ^-return
        }
    }
    Ok(UnwindOutcome::Exhausted)
}

/// Pop and clean up all `CallFrame`s remaining on the stack (for terminal error propagation).
fn cleanup_frames(ev: &Evaluator, stack: &mut Vec<Cont>) {
    while let Some(frame) = stack.pop() {
        if let Cont::CallFrame {
            self_binding,
            saved_block_home,
            mut env,
            is_method,
            pops_env,
            ..
        } = frame
        {
            if is_method {
                debug_assert!(self_binding.is_some());
                ev.pop_self();
            }
            ev.current_block_home.set(saved_block_home);
            if pops_env {
                env.pop_frame();
            }
        }
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
            // HandleOnDoResult (condition handling), cleaning up CallFrames along the way.
            while let Some(cont) = stack.pop() {
                match cont {
                    Cont::CallFrame {
                        self_binding,
                        saved_block_home,
                        mut env,
                        is_method,
                        pops_env,
                        ..
                    } => {
                        if is_method {
                            debug_assert!(self_binding.is_some());
                            ev.pop_self();
                        }
                        ev.current_block_home.set(saved_block_home);
                        if pops_env {
                            env.pop_frame();
                        }
                    }
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
                    _ => {
                        // All other continuations (ForceAcc, RestoreAcc, CollectArgs, etc.)
                        // are discarded during error propagation.
                    }
                }
            }
            // Stack exhausted — no handler found; propagate.
            Err(other)
        }
    }
}

/// Drive the iterative evaluator to completion starting from `initial_cont`.
///
/// Requires: any `CallFrame` in `initial_cont` has already had its env
///   frame pushed and, for method frames, its self binding pushed to
///   `ev.method_stack`.
/// Ensures: returns the value accumulated when the continuation stack
///   is empty, or propagates the first terminal error.
pub(crate) fn step_loop(ev: &Evaluator, initial_cont: Cont) -> Result<Value, EvalError> {
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
            Cont::EvalExpr { expr, mut env } => match expr {
                Expr::Lit(lit) => {
                    acc = eval_literal(&lit);
                }
                Expr::Var(name) => {
                    acc = ev
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
                    acc = Value::Block(Arc::new(crate::value::Block {
                        params,
                        locals,
                        body,
                        env: env.clone(),
                        home_depth,
                        home_entity: ev.current_entity_id.get(),
                    }));
                }
                Expr::NamespaceRef {
                    namespace_path,
                    name,
                } => {
                    acc = ev.eval_namespace_ref(&namespace_path, &name, &mut env)?;
                }
                Expr::Super => {
                    return Err(EvalError::TypeError(
                        "super may only appear as a message receiver".into(),
                    ));
                }
                Expr::Send { receiver, message } => {
                    if matches!(receiver.as_ref(), Expr::Super) {
                        // Super sends: use recursive path (S6 will iterativise).
                        match ev.send_super(&message, &mut env) {
                            Ok(v) => acc = v,
                            Err(e) => handle_step_error(e, ev, &mut stack, &mut acc)?,
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
                    acc = eval_array_literal_iterative(ev, &elements, &mut env)?;
                }
                Expr::DynamicArray(elements) => {
                    let array_handle = ev
                        .registry
                        .kernel()
                        .lookup_class("Array")
                        .ok_or_else(|| EvalError::UndefinedVariable(Arc::from("Array")))?;
                    if elements.is_empty() {
                        acc = build_array(array_handle.0.class.clone(), vec![]);
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
            },

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

            // ── CallFrame dispatch ────────────────────────────────────────
            Cont::CallFrame {
                self_binding,
                depth,
                mut pending,
                mut env,
                implicit_return,
                is_method,
                pops_env,
                saved_block_home,
                frame_block_home,
                defining_class,
                sync,
            } => {
                if pending.is_empty() {
                    // Frame exhausted — pop method stack and restore home.
                    if is_method {
                        debug_assert!(
                            self_binding.is_some(),
                            "method CallFrame must have a self_binding"
                        );
                        ev.pop_self();
                    }
                    ev.current_block_home.set(saved_block_home);
                    if pops_env {
                        env.pop_frame();
                    }
                    // Method frames return implicit self; block/program frames
                    // return the last accumulated value.
                    if is_method {
                        acc = implicit_return;
                    }
                    // else acc stays as the last expression result
                } else {
                    let stmt = pending.pop_front().unwrap();
                    // Push the frame back first (continuing frame is below new work).
                    stack.push(Cont::CallFrame {
                        self_binding,
                        depth,
                        pending,
                        env: env.clone(),
                        implicit_return,
                        is_method,
                        pops_env,
                        saved_block_home,
                        frame_block_home,
                        defining_class,
                        sync,
                    });
                    // For sync frames, force the statement result before the frame resumes.
                    if sync {
                        stack.push(Cont::ForceAcc);
                    }
                    dispatch_stmt(ev, stmt, &env, depth, &mut stack, &mut acc)?;
                }
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

            // ── OnDoCatch (success path) ──────────────────────────────────
            Cont::OnDoCatch { .. } => {
                // Body completed without raising a signal; acc = body result, pass through.
            }

            // ── HandleOnDoResult (success path) ───────────────────────────
            Cont::HandleOnDoResult { .. } => {
                // Handler completed normally; acc = handler result, pass through.
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
            Cont::AfterReceiver { message, mut env } => {
                let receiver = acc.clone();
                match message {
                    Message::Unary(sel) => {
                        if let Err(e) = dispatch_to_stack(
                            ev,
                            receiver,
                            sel,
                            vec![],
                            &mut env,
                            &mut stack,
                            &mut acc,
                        ) {
                            handle_step_error(e, ev, &mut stack, &mut acc)?;
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
            }

            // ── CollectArgs ───────────────────────────────────────────────
            Cont::CollectArgs {
                receiver,
                selector,
                mut done,
                mut pending,
                mut env,
            } => {
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
                    reductions += 1;
                    if reductions >= YIELD_INTERVAL {
                        reductions = 0;
                        may::coroutine::yield_now();
                    }
                    if let Err(e) = dispatch_to_stack(
                        ev, receiver, selector, done, &mut env, &mut stack, &mut acc,
                    ) {
                        handle_step_error(e, ev, &mut stack, &mut acc)?;
                    }
                }
            }

            // ── CascadeStep ───────────────────────────────────────────────
            Cont::CascadeStep {
                receiver,
                mut pending,
                mut env,
            } => {
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
                            if let Err(e) = dispatch_to_stack(
                                ev,
                                recv,
                                sel,
                                vec![],
                                &mut env,
                                &mut stack,
                                &mut acc,
                            ) {
                                handle_step_error(e, ev, &mut stack, &mut acc)?;
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
                            let mut exprs: VecDeque<Expr> =
                                parts.into_iter().map(|(_, e)| e).collect();
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
                // else: pending empty — acc already holds last send result, nothing to push
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
    let saved_home = ev.current_block_home.get();
    ev.current_block_home.set(Some(block.home_depth));
    stack.push(Cont::CallFrame {
        self_binding: None,
        depth: 0,
        pending: block.body.iter().cloned().collect(),
        env: block_env,
        implicit_return: Value::Nil,
        is_method: false,
        pops_env: true,
        saved_block_home: saved_home,
        frame_block_home: Some(block.home_depth),
        defining_class: None,
        sync,
    });
    Ok(())
}

/// Drain tail-call frame patterns from the stack before pushing a new callee.
///
/// Requires: none (safe to call on any stack state; no-op if no pattern matches).
/// Ensures:
///   (1) All empty block frames (`is_method=false`, `pending` empty) at the top are
///       drained (env popped, `current_block_home` restored).
///   (2) If the top is then `ReturnUnwind(D)` and, scanning past any further empty
///       block frames, the next is an empty method frame at depth D, the entire
///       sequence (ReturnUnwind + intervening block frames + method frame) is drained.
///   (3) Steps (1)–(2) repeat until no further pattern applies.
///   Safe: empty block frames return `acc` unchanged; `ReturnUnwind(D)` would have
///   overridden the method's implicit self-return with the same value the new callee
///   will produce.
fn drain_tail_frames(ev: &Evaluator, stack: &mut Vec<Cont>) {
    loop {
        // Phase 1: drain leading empty block frames.
        loop {
            let should_drain = matches!(
                stack.last(),
                Some(Cont::CallFrame { is_method: false, pending, .. }) if pending.is_empty()
            );
            if !should_drain {
                break;
            }
            if let Some(Cont::CallFrame {
                saved_block_home,
                pops_env,
                mut env,
                ..
            }) = stack.pop()
            {
                ev.current_block_home.set(saved_block_home);
                if pops_env {
                    env.pop_frame();
                }
            }
        }

        // Phase 2: check for ReturnUnwind(D) followed (past empty block frames)
        // by an empty method frame at depth D.
        let target = match stack.last() {
            Some(Cont::ReturnUnwind { target_depth }) => *target_depth,
            _ => break,
        };

        // Scan downward past empty block frames to find the matching method frame.
        let mut scan_idx = stack.len().checked_sub(2);
        let mut intermediate = 0usize;
        let found_method = loop {
            match scan_idx {
                None => break false,
                Some(idx) => match stack.get(idx) {
                    Some(Cont::CallFrame {
                        is_method: false,
                        pending,
                        ..
                    }) if pending.is_empty() => {
                        intermediate += 1;
                        scan_idx = idx.checked_sub(1);
                    }
                    Some(Cont::CallFrame {
                        is_method: true,
                        depth,
                        pending,
                        ..
                    }) if *depth == target && pending.is_empty() => {
                        break true;
                    }
                    _ => break false,
                },
            }
        };

        if !found_method {
            break;
        }

        // Drain: ReturnUnwind, then intermediate empty block frames, then the method frame.
        stack.pop(); // ReturnUnwind
        for _ in 0..intermediate {
            if let Some(Cont::CallFrame {
                saved_block_home,
                pops_env,
                mut env,
                ..
            }) = stack.pop()
            {
                ev.current_block_home.set(saved_block_home);
                if pops_env {
                    env.pop_frame();
                }
            }
        }
        if let Some(Cont::CallFrame {
            saved_block_home,
            pops_env,
            mut env,
            is_method: true,
            ..
        }) = stack.pop()
        {
            ev.pop_self();
            ev.current_block_home.set(saved_block_home);
            if pops_env {
                env.pop_frame();
            }
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
fn dispatch_to_stack(
    ev: &Evaluator,
    receiver: Value,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    // (1) User-defined method lookup (covers Block, Int, Nil, ValueObject, entity self-send).
    if let Some(found) = lookup_user_method(ev, &receiver, &selector) {
        drain_tail_frames(ev, stack);
        push_method_call_frame(ev, found, args, env, stack)?;
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

    // (6) Inline Bool ifTrue:/ifFalse:/ifTrue:ifFalse:/ifFalse:ifTrue: dispatch.
    if let Value::Bool(b) = receiver {
        match selector.as_ref() {
            "ifTrue:" => {
                let blk = as_block(args.into_iter().next().unwrap_or(Value::Nil), &selector)?;
                if b {
                    push_block_frame(ev, &blk, vec![], false, stack)?;
                } else {
                    *acc = Value::Nil;
                }
                return Ok(());
            }
            "ifFalse:" => {
                let blk = as_block(args.into_iter().next().unwrap_or(Value::Nil), &selector)?;
                if !b {
                    push_block_frame(ev, &blk, vec![], false, stack)?;
                } else {
                    *acc = Value::Nil;
                }
                return Ok(());
            }
            "ifTrue:ifFalse:" => {
                let mut iter = args.into_iter();
                let t_blk = as_block(iter.next().unwrap_or(Value::Nil), &selector)?;
                let f_blk = as_block(iter.next().unwrap_or(Value::Nil), &selector)?;
                if b {
                    push_block_frame(ev, &t_blk, vec![], false, stack)?;
                } else {
                    push_block_frame(ev, &f_blk, vec![], false, stack)?;
                }
                return Ok(());
            }
            "ifFalse:ifTrue:" => {
                let mut iter = args.into_iter();
                let f_blk = as_block(iter.next().unwrap_or(Value::Nil), &selector)?;
                let t_blk = as_block(iter.next().unwrap_or(Value::Nil), &selector)?;
                if b {
                    push_block_frame(ev, &t_blk, vec![], false, stack)?;
                } else {
                    push_block_frame(ev, &f_blk, vec![], false, stack)?;
                }
                return Ok(());
            }
            _ => {}
        }
    }

    // (7) Universal nil-guard family — checked after user methods so user overrides win.
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

    // (8) All other selectors fall through to builtins.
    *acc = crate::builtins::dispatch(ev, receiver, selector, args, env)?;
    Ok(())
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
                handle: handle.clone(),
                defining_class: def_class.clone(),
            };
            ev.push_self(binding.clone());
            let depth = ev.method_depth.get();
            let saved_block_home = ev.current_block_home.get();
            ev.current_block_home.set(None);
            stack.push(Cont::CallFrame {
                self_binding: Some(binding),
                depth,
                pending: method.body.iter().cloned().collect(),
                env: method_env,
                implicit_return: self_value,
                is_method: true,
                pops_env: true,
                saved_block_home,
                frame_block_home: None,
                defining_class: Some(def_class),
                sync: false,
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
                defining_class: def_class.clone(),
                self_value: self_value.clone(),
            };
            ev.push_self(binding.clone());
            let depth = ev.method_depth.get();
            let saved_block_home = ev.current_block_home.get();
            ev.current_block_home.set(None);
            stack.push(Cont::CallFrame {
                self_binding: Some(binding),
                depth,
                pending: method.body.iter().cloned().collect(),
                env: method_env,
                implicit_return: self_value,
                is_method: true,
                pops_env: true,
                saved_block_home,
                frame_block_home: None,
                defining_class: Some(def_class),
                sync: false,
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
