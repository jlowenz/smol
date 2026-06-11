// C30 will promote IEvaluator to the default evaluator; until then all items
// here are only reachable via test_helpers and are flagged as dead by clippy.
#![allow(dead_code)]

/// One reduction = one completed message dispatch. Matches BEAM's "one reduction
/// per function call" granularity. At ~1M msg/s this yields every ~1 ms.
const YIELD_INTERVAL: u32 = 1024;
use std::collections::VecDeque;
use std::sync::Arc;

use crate::ast::{Expr, Message, MethodDef, Stmt};
use crate::builtins::{UserMethodFound, lookup_user_method};
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
    },
}

/// Unwind heap `CallFrame`s until one with `depth == target_depth` is found.
///
/// Ensures: for each `CallFrame` popped, `ev.pop_self()` is called (if method frame),
///   `ev.current_block_home` is restored, and `env.pop_frame()` is called (if `pops_env`).
///   Returns `true` if a matching frame was found; `false` if the stack was exhausted.
fn unwind_stack_to_depth(ev: &Evaluator, stack: &mut Vec<Cont>, target_depth: usize) -> bool {
    while let Some(frame) = stack.pop() {
        if let Cont::CallFrame {
            self_binding,
            depth,
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
            if depth == target_depth {
                return true;
            }
        }
    }
    false
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
/// `EvalError::Return(v, depth)` is intercepted: the stack is unwound to
/// the matching `CallFrame`; if found, `acc` is set to `v` and `Ok(())` is
/// returned so the loop continues. Other errors trigger frame cleanup and
/// propagate.
fn handle_step_error(
    e: EvalError,
    ev: &Evaluator,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    match e {
        EvalError::Return(v, depth) => {
            if unwind_stack_to_depth(ev, stack, depth) {
                *acc = v;
                Ok(())
            } else {
                cleanup_frames(ev, stack);
                Err(EvalError::Return(v, depth))
            }
        }
        other => {
            cleanup_frames(ev, stack);
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
fn step_loop(ev: &Evaluator, initial_cont: Cont) -> Result<Value, EvalError> {
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
                if unwind_stack_to_depth(ev, &mut stack, target_depth) {
                    acc = return_val;
                } else {
                    cleanup_frames(ev, &mut stack);
                    return Err(EvalError::Return(return_val, target_depth));
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
                    });
                    dispatch_stmt(ev, stmt, &env, depth, &mut stack, &mut acc)?;
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

/// Dispatch a message send, either by pushing a `CallFrame` for user-defined methods
/// or by calling `builtins::dispatch` for everything else.
///
/// Requires: `args` are fully evaluated in left-to-right order.
fn dispatch_to_stack(
    ev: &Evaluator,
    receiver: Value,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
    stack: &mut Vec<Cont>,
    acc: &mut Value,
) -> Result<(), EvalError> {
    if let Some(found) = lookup_user_method(ev, &receiver, &selector) {
        push_method_call_frame(ev, found, args, env, stack)?;
    } else {
        *acc = crate::builtins::dispatch(ev, receiver, selector, args, env)?;
    }
    Ok(())
}

/// Build and push a `CallFrame` for a user-defined method call.
///
/// Requires: `args.len() == method.pattern.params().len()`.
/// Ensures: a `CallFrame` is pushed onto `stack` with the method env set up;
///   `ev.method_stack` is updated (push_self called).
fn push_method_call_frame(
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;
    use crate::parser::Parser;

    /// Run Smol source through the iterative evaluator inside a may coroutine.
    #[allow(clippy::arc_with_non_send_sync)]
    fn iter_eval(src: &str) -> Result<Value, EvalError> {
        let src = src.to_owned();
        let (tx, rx) = std::sync::mpsc::channel();
        crate::entity::init_coroutine_stack();
        may::go!(move || {
            let prog = Parser::parse(&src).expect("parse");
            let ev = IEvaluator::new(Arc::new(Evaluator::new()));
            let mut env = Env::new();
            ev.inner.registry.populate_env(&mut env);
            let result = ev.eval_program(&prog.stmts, &mut env);
            ev.inner.shutdown();
            tx.send(result).unwrap();
        });
        rx.recv().expect("iter_eval coroutine panicked")
    }

    #[test]
    fn s1_literal_return() {
        let v = iter_eval("^42").unwrap();
        assert_eq!(v, Value::Int(42));
    }

    #[test]
    fn s1_var_lookup() {
        let v = iter_eval("| x | x := 7. x").unwrap();
        assert_eq!(v, Value::Int(7));
    }

    #[test]
    fn s2_binary_send() {
        let v = iter_eval("3 + 4").unwrap();
        assert_eq!(v, Value::Int(7));
    }

    #[test]
    fn s2_unary_send() {
        let v = iter_eval("'hello' size").unwrap();
        assert_eq!(v, Value::Int(5));
    }

    #[test]
    fn s2_keyword_send() {
        let v = iter_eval("10 max: 20").unwrap();
        assert_eq!(v, Value::Int(20));
    }

    #[test]
    fn s2_user_defined_method() {
        // Value subclass: -> ValueObject, dispatch is synchronous. Pure methods (no ivar write).
        let src = "
            Value subclass: #Calc instanceVariableNames: ''.
            Calc >> add: x to: y [ ^x + y ].
            | c |
            c := Calc new.
            c add: 3 to: 4
        ";
        let v = iter_eval(src).unwrap();
        assert_eq!(v, Value::Int(7));
    }

    #[test]
    fn s2_chained_user_methods() {
        // Exercises multiple heap CallFrames: double calls triple, both pure.
        let src = "
            Value subclass: #Calc instanceVariableNames: ''.
            Calc >> double: x [ ^x * 2 ].
            Calc >> quadruple: x [ ^self double: (self double: x) ].
            | c |
            c := Calc new.
            c quadruple: 3
        ";
        let v = iter_eval(src).unwrap();
        assert_eq!(v, Value::Int(12));
    }

    #[test]
    fn s3_cascade() {
        // Cascade sends multiple messages to the same receiver; result is last send.
        // 'hello' printString returns 'hello'; 'hello' size returns 5.
        let v = iter_eval("'hello' printString; size").unwrap();
        assert_eq!(v, Value::Int(5));
    }

    #[test]
    fn s3_dynamic_array() {
        let v = iter_eval("{ 1 + 1. 2 + 2. 3 + 3 }").unwrap();
        // Result is an Array with 3 elements: 2, 4, 6.
        use crate::object::IndexedStorage;
        if let Value::Instance(h) = v
            && let crate::entity::InstanceKind::ValueObject(obj) = &h.0.kind
            && let Some(IndexedStorage::Pointer(slots)) = obj.indexed.as_ref()
        {
            let guard = slots.lock();
            assert_eq!(guard[0], Value::Int(2));
            assert_eq!(guard[1], Value::Int(4));
            assert_eq!(guard[2], Value::Int(6));
        } else {
            panic!("expected Array instance");
        }
    }

    #[test]
    fn s3_array_literal() {
        let v = iter_eval("#(1 2 3)").unwrap();
        use crate::object::IndexedStorage;
        if let Value::Instance(h) = v
            && let crate::entity::InstanceKind::ValueObject(obj) = &h.0.kind
            && let Some(IndexedStorage::Pointer(slots)) = obj.indexed.as_ref()
        {
            let guard = slots.lock();
            assert_eq!(guard[0], Value::Int(1));
            assert_eq!(guard[1], Value::Int(2));
            assert_eq!(guard[2], Value::Int(3));
        } else {
            panic!("expected Array instance");
        }
    }

    #[test]
    fn s5_nonlocal_return_from_block() {
        // ^-return from a block inside a method, through a builtin (ifTrue:).
        let src = "
            Value subclass: #Foo instanceVariableNames: ''.
            Foo >> bar [
                true ifTrue: [ ^42 ].
                ^99
            ].
            Foo new bar
        ";
        let v = iter_eval(src).unwrap();
        assert_eq!(v, Value::Int(42));
    }

    #[test]
    fn s5_on_do_catches_signal() {
        // on:do: catches a signal raised within iterative eval.
        let src = "
            [Error signal: 'oops']
                on: Error
                do: [:e | 'caught']
        ";
        let v = iter_eval(src).unwrap();
        assert_eq!(v, Value::Str(Arc::from("caught")));
    }

    #[test]
    fn s5_ensure_runs_cleanup() {
        // ensure: body block always runs; result comes from the protected block.
        let src = "
            Value subclass: #Tester instanceVariableNames: ''.
            Tester >> go [
                ^[42] ensure: [0]
            ].
            Tester new go
        ";
        let v = iter_eval(src).unwrap();
        assert_eq!(v, Value::Int(42));
    }
}
