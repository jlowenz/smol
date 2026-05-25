use std::cell::{Cell, RefCell};
use std::sync::{Arc, OnceLock};

use crate::entity::{EntityId, InstanceHandle};

use thiserror::Error;

use crate::ast::*;
use crate::class::{Class, Method};
use crate::class_registry::ClassRegistry;
use crate::env::Env;
use crate::namespace::Namespace;
use crate::object::{IndexedStorage, Object};
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
    /// Smol-raised signal (from `_primSignal:` / `Error signal:`). Carries a
    /// clean message string, distinct from `TypeError` so `on:do:` can surface
    /// it without the "type error: " prefix.
    #[error("signal: {0}")]
    Signal(Arc<str>),
    /// Non-local `^`-return carrying the value and the method-stack depth of the
    /// frame that should catch it. Only the `invoke_method` / `invoke_class_method`
    /// whose depth matches will unwrap this; all others propagate it unchanged.
    #[error("non-local return")]
    Return(Value, usize),
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
}

pub struct Evaluator {
    pub registry: Arc<ClassRegistry>,
    method_stack: RefCell<Vec<SelfBinding>>,
    /// Depth of the active method stack. Maintained alongside `method_stack` so
    /// the common case — no active method — can be tested with a plain `Cell`
    /// read instead of a `RefCell` borrow.
    method_depth: Cell<usize>,
    /// Currently active namespace. Defaults to `Kernel`. Temporarily swapped by
    /// `Namespace do: [...]` for the duration of the block.
    ///
    /// Note: this is a per-evaluator (single-threaded) cell. Future multi-fiber
    /// execution will need to move this to a per-frame or per-fiber slot.
    active_namespace: RefCell<Arc<Namespace>>,
    /// The method-stack depth that the innermost active `eval_block` invocation
    /// should target with a `^`-return. `None` when not inside any `eval_block`
    /// (i.e., evaluating a method body directly). Saved and restored by each
    /// `eval_block` call so nesting is handled correctly.
    current_block_home: Cell<Option<usize>>,
    /// The `EntityId` of the entity whose message handler is currently executing
    /// on this evaluator. `None` outside any entity handler. Used for sync
    /// self-send detection. Stored here (not in a thread-local) because may
    /// coroutines multiplex over OS threads with work-stealing, so a TLS value
    /// is not reliably tied to the executing coroutine.
    pub(crate) current_entity_id: Cell<Option<EntityId>>,
}

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
        let registry = Arc::new(ClassRegistry::new());
        let kernel = registry.kernel();
        Self {
            registry,
            method_stack: RefCell::new(Vec::new()),
            method_depth: Cell::new(0),
            active_namespace: RefCell::new(kernel),
            current_block_home: Cell::new(None),
            current_entity_id: Cell::new(None),
        }
    }

    // ── Method receiver stack ─────────────────────────────────────────────────

    fn push_self(&self, binding: SelfBinding) {
        self.method_stack.borrow_mut().push(binding);
        self.method_depth.set(self.method_depth.get() + 1);
    }

    fn pop_self(&self) {
        self.method_stack.borrow_mut().pop();
        self.method_depth.set(self.method_depth.get() - 1);
    }

    /// Returns the `defining_class` of the top method frame (for `super` dispatch).
    fn current_defining_class(&self) -> Option<Arc<Class>> {
        match self.method_stack.borrow().last() {
            Some(SelfBinding::Instance { defining_class, .. }) => Some(defining_class.clone()),
            Some(SelfBinding::ClassSide { defining_class, .. }) => Some(defining_class.clone()),
            None => None,
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
                // 2. instance variables
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
            None => env.get(name),
        }
    }

    /// Assign `value` to `name`, following the method-context lookup order, and
    /// return the stored value for use as the statement's result.
    ///
    /// Requires: `value` is the fully-evaluated right-hand side.
    /// Ensures: exactly one `Value::clone` is performed regardless of path.
    #[inline]
    fn assign_var(&self, name: &Arc<str>, value: Value, env: &mut Env) -> Result<Value, EvalError> {
        if self.method_depth.get() > 0 {
            let top = self.method_stack.borrow().last().cloned();
            match top {
                Some(SelfBinding::Instance { handle, .. }) => {
                    let obj = handle.ivar_storage();
                    // Extract bool before any other access to avoid holding the Mutex
                    // guard across a re-entrant lock on the same object.
                    let has_ivar = obj.slots.lock().contains_key(name.as_ref());
                    if has_ivar {
                        let ret = value.clone();
                        obj.set_slot(name.clone(), value);
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
                None => {}
            }
        }
        // Default: update existing binding or create new local.
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
    pub fn eval_program(&self, stmts: &[Stmt], env: &mut Env) -> Result<Value, EvalError> {
        // Pre-bind all LocalDecl names to nil before evaluating any statement.
        for stmt in stmts {
            if let Stmt::LocalDecl(names) = stmt {
                for name in names {
                    env.define(name.clone(), Value::Nil);
                }
            }
        }
        let mut last = Value::Nil;
        for stmt in stmts {
            last = self.eval_stmt(stmt, env)?;
        }
        Ok(last)
    }

    fn eval_stmt(&self, stmt: &Stmt, env: &mut Env) -> Result<Value, EvalError> {
        match stmt {
            Stmt::LocalDecl(_) => Ok(Value::Nil), // already pre-bound in eval_program
            Stmt::Expr(e) => self.eval_expr(e, env),
            Stmt::Assign { name, value } => {
                let v = self.eval_expr(value, env)?;
                self.assign_var(name, v, env)
            }
            Stmt::Return(e) => {
                let v = self.eval_expr(e, env)?;
                // Target the home method for this return. Inside eval_block the home
                // is the depth of the method that defined the block; at the method body
                // level current_block_home is None so we use the current method depth.
                let depth = self
                    .current_block_home
                    .get()
                    .unwrap_or_else(|| self.method_depth.get());
                Err(EvalError::Return(v, depth))
            }
            Stmt::MethodDef(def) => {
                use crate::entity::InstanceKind;
                // Look up the class from env first (covers non-Kernel namespace classes).
                let class = match self.lookup_var(&def.class_name, env) {
                    Some(Value::Instance(h)) if matches!(h.0.kind, InstanceKind::ClassObject) => {
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
                Ok(Value::Nil)
            }
        }
    }

    pub fn eval_expr(&self, expr: &Expr, env: &mut Env) -> Result<Value, EvalError> {
        match expr {
            Expr::Super => Err(EvalError::TypeError(
                "super may only appear as a message receiver".into(),
            )),
            Expr::Lit(lit) => Ok(eval_literal(lit)),
            Expr::Var(name) => self
                .lookup_var(name.as_ref(), env)
                .ok_or_else(|| EvalError::UndefinedVariable(name.clone())),
            Expr::Send { receiver, message } => {
                // Detect `super send` before evaluating the receiver normally.
                if matches!(receiver.as_ref(), Expr::Super) {
                    return self.send_super(message, env);
                }
                let recv = self.eval_expr(receiver, env)?;
                self.send(recv, message, env)
            }
            Expr::Block {
                params,
                locals,
                body,
            } => {
                // Blocks inherit the home depth of their creation context: the same method
                // that an enclosing block targets, or the current method depth if no block
                // context is active. This ensures nested blocks all point to the right frame.
                let home_depth = self
                    .current_block_home
                    .get()
                    .unwrap_or_else(|| self.method_depth.get());
                Ok(Value::Block(Arc::new(Block {
                    params: params.clone(),
                    locals: locals.clone(),
                    body: body.clone(),
                    env: env.clone(),
                    home_depth,
                    home_entity: self.current_entity_id.get(),
                })))
            }
            Expr::Cascade { receiver, messages } => {
                let recv = self.eval_expr(receiver, env)?;
                let mut result = Value::Nil;
                for msg in messages {
                    result = self.send(recv.clone(), msg, env)?;
                }
                Ok(result)
            }
            Expr::NamespaceRef {
                namespace_path,
                name,
            } => self.eval_namespace_ref(namespace_path, name, env),
            Expr::ArrayLiteral(elements) => self.eval_array_literal(elements, env),
        }
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
    fn eval_literal_as_value(&self, lit: &Literal, env: &mut Env) -> Result<Value, EvalError> {
        match lit {
            Literal::Array(elems) => self.eval_array_literal(elems, env),
            _ => Ok(eval_literal(lit)),
        }
    }

    fn send(&self, recv: Value, message: &Message, env: &mut Env) -> Result<Value, EvalError> {
        match message {
            Message::Unary(sel) => self.dispatch(recv, sel.clone(), vec![], env),
            Message::Binary { op, arg } => {
                let arg_val = self.eval_expr(arg, env)?;
                self.dispatch(recv, op.clone(), vec![arg_val], env)
            }
            Message::Keyword { parts } => {
                let selector: Arc<str> = parts
                    .iter()
                    .map(|(k, _)| k.as_ref())
                    .collect::<String>()
                    .into();
                let mut args = Vec::with_capacity(parts.len());
                for (_, expr) in parts {
                    args.push(self.eval_expr(expr, env)?);
                }
                self.dispatch(recv, selector, args, env)
            }
        }
    }

    /// Evaluate message args for a super send without evaluating the receiver.
    fn eval_message_args(
        &self,
        message: &Message,
        env: &mut Env,
    ) -> Result<(Arc<str>, Vec<Value>), EvalError> {
        match message {
            Message::Unary(sel) => Ok((sel.clone(), vec![])),
            Message::Binary { op, arg } => {
                let v = self.eval_expr(arg, env)?;
                Ok((op.clone(), vec![v]))
            }
            Message::Keyword { parts } => {
                let selector: Arc<str> = parts
                    .iter()
                    .map(|(k, _)| k.as_ref())
                    .collect::<String>()
                    .into();
                let mut args = Vec::with_capacity(parts.len());
                for (_, expr) in parts {
                    args.push(self.eval_expr(expr, env)?);
                }
                Ok((selector, args))
            }
        }
    }

    /// Handle `super send`: look up the selector starting from the superclass of
    /// the defining class and invoke on the current receiver.
    fn send_super(&self, message: &Message, env: &mut Env) -> Result<Value, EvalError> {
        let defining_class = self
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
        let (selector, args) = self.eval_message_args(message, env)?;

        // Clone the top binding to avoid holding the RefCell borrow during invoke.
        let top = self.method_stack.borrow().last().cloned();
        match top {
            Some(SelfBinding::Instance { handle, .. }) => match superclass.lookup(&selector) {
                Some((method, def_class)) => {
                    self.invoke_method(handle, def_class, method, args, env)
                }
                None => Err(EvalError::MessageNotUnderstood {
                    receiver_kind: "Object",
                    selector,
                }),
            },
            Some(SelfBinding::ClassSide { handle, .. }) => {
                match superclass.lookup_class_method(&selector) {
                    Some((method, def_class)) => {
                        self.invoke_class_method(handle, def_class, method, args, env)
                    }
                    // No user-defined super method; fall through to built-in class
                    // dispatch so `super new` / `super new:` reach the Rust primitives
                    // without re-entering the receiver's own `new` override.
                    None => {
                        crate::builtins::dispatch_class_builtin(self, handle, selector, args, env)
                    }
                }
            }
            None => Err(EvalError::TypeError(
                "super used outside method context".into(),
            )),
        }
    }

    /// Evaluate `Expr::NamespaceRef`: resolve a `Ns::ClassName` path.
    fn eval_namespace_ref(
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

    /// Dispatches a message to a value. `selector` is the full selector string.
    pub fn dispatch(
        &self,
        recv: Value,
        selector: Arc<str>,
        args: Vec<Value>,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
        crate::builtins::dispatch(self, recv, selector, args, env)
    }

    /// Evaluate a `Block` value by sending it `value` / `value:` etc.
    ///
    /// Sets `current_block_home` to the block's `home_depth` so that any `^`
    /// inside the block targets the correct enclosing method frame. Does NOT
    /// catch `EvalError::Return` — it propagates to `invoke_method` / `invoke_class_method`
    /// of the frame whose depth matches the Return's target.
    pub fn eval_block(
        &self,
        block: &Block,
        args: Vec<Value>,
        _outer_env: &mut Env,
    ) -> Result<Value, EvalError> {
        if args.len() != block.params.len() {
            return Err(EvalError::ArityMismatch {
                expected: block.params.len(),
                got: args.len(),
            });
        }
        let prev_home = self.current_block_home.get();
        self.current_block_home.set(Some(block.home_depth));

        let mut block_env = block.env.clone();
        block_env.push_frame();
        for name in &block.locals {
            block_env.define(name.clone(), Value::Nil);
        }
        for (name, val) in block.params.iter().zip(args) {
            block_env.define(name.clone(), val);
        }
        let result = self.eval_program(&block.body, &mut block_env);
        block_env.pop_frame();
        self.current_block_home.set(prev_home);
        result
    }

    /// Evaluate a `Block` with sync semantics: after each statement any
    /// `Value::Future` result is awaited before proceeding to the next.
    ///
    /// Requires: `args.len() == block.params.len()`.
    /// Ensures: every statement result is fully resolved before the next
    ///   statement runs; the final result is fully resolved.
    pub fn eval_block_sync(
        &self,
        block: &Block,
        args: Vec<Value>,
        _outer_env: &mut Env,
    ) -> Result<Value, EvalError> {
        if args.len() != block.params.len() {
            return Err(EvalError::ArityMismatch {
                expected: block.params.len(),
                got: args.len(),
            });
        }
        let prev_home = self.current_block_home.get();
        self.current_block_home.set(Some(block.home_depth));

        let mut block_env = block.env.clone();
        block_env.push_frame();
        for name in &block.locals {
            block_env.define(name.clone(), Value::Nil);
        }
        for (name, val) in block.params.iter().zip(args) {
            block_env.define(name.clone(), val);
        }
        let result = self.eval_program_sync(&block.body, &mut block_env);
        block_env.pop_frame();
        self.current_block_home.set(prev_home);
        result
    }

    fn eval_program_sync(&self, stmts: &[Stmt], env: &mut Env) -> Result<Value, EvalError> {
        for stmt in stmts {
            if let Stmt::LocalDecl(names) = stmt {
                for name in names {
                    env.define(name.clone(), Value::Nil);
                }
            }
        }
        let mut last = Value::Nil;
        for stmt in stmts {
            let v = self.eval_stmt(stmt, env)?;
            last = crate::builtins::force_value(v)?;
        }
        Ok(last)
    }

    /// Invoke a user-defined method on a receiver object.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: method locals and `self` are bound in a fresh frame; the frame
    ///   is cleaned up on return; a method with no explicit `^` returns
    ///   `Value::Instance(handle)` (Smalltalk implicit self return); `EvalError::Return`
    ///   whose depth matches this frame is caught and unwrapped; Returns
    ///   targeting an outer frame propagate.
    pub fn invoke_method(
        &self,
        handle: InstanceHandle,
        defining_class: Arc<Class>,
        method: Arc<Method>,
        args: Vec<Value>,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
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
        self.push_self(SelfBinding::Instance {
            handle: handle.clone(),
            defining_class,
        });
        let my_depth = self.method_depth.get();
        // A method body is not a block: reset current_block_home so that ^-returns
        // inside this body target this frame (not the block home of a caller).
        let prev_home = self.current_block_home.get();
        self.current_block_home.set(None);
        let result = self.eval_program(&method.body, &mut method_env);
        self.current_block_home.set(prev_home);
        self.pop_self();
        method_env.pop_frame();
        match result {
            Ok(_) => Ok(self_value),
            Err(EvalError::Return(v, d)) if d == my_depth => Ok(v),
            Err(e) => Err(e),
        }
    }

    /// Invoke a user-defined class-side method.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: `self` is bound to `Value::Instance(handle)` in the method frame;
    ///   a method with no explicit `^` returns `Value::Instance(handle)` (Smalltalk
    ///   implicit self return); `EvalError::Return` whose depth matches this
    ///   frame is caught and unwrapped.
    pub fn invoke_class_method(
        &self,
        handle: crate::entity::InstanceHandle,
        defining_class: Arc<Class>,
        method: Arc<Method>,
        args: Vec<Value>,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
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
        self.push_self(SelfBinding::ClassSide {
            handle,
            defining_class,
        });
        let my_depth = self.method_depth.get();
        let prev_home = self.current_block_home.get();
        self.current_block_home.set(None);
        let result = self.eval_program(&method.body, &mut method_env);
        self.current_block_home.set(prev_home);
        self.pop_self();
        method_env.pop_frame();
        match result {
            Ok(_) => Ok(self_val),
            Err(EvalError::Return(v, d)) if d == my_depth => Ok(v),
            Err(e) => Err(e),
        }
    }

    /// Create an evaluator from an existing `ClassRegistry`, loading `kernel.smol`.
    ///
    /// Ensures: same post-conditions as `new()` but shares the given registry
    ///   instead of creating a fresh one. Intended for entity message loops so
    ///   that all entities share a single registry and kernel class set.
    pub fn with_registry(registry: Arc<ClassRegistry>) -> Self {
        let kernel = registry.kernel();
        Self {
            registry,
            method_stack: RefCell::new(Vec::new()),
            method_depth: Cell::new(0),
            active_namespace: RefCell::new(kernel),
            current_block_home: Cell::new(None),
            current_entity_id: Cell::new(None),
        }
    }
}

/// Returns a pre-allocated `Arc<str>` for the identifier `"self"`.
/// Allocated once per process via `OnceLock`; subsequent calls are a refcount bump.
fn self_sym() -> Arc<str> {
    static SELF: OnceLock<Arc<str>> = OnceLock::new();
    SELF.get_or_init(|| Arc::from("self")).clone()
}

fn eval_literal(lit: &Literal) -> Value {
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
    matches!(expr, Expr::Super) || matches!(expr, Expr::Var(name) if name.as_ref() == "self")
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
        Expr::Lit(_)
        | Expr::Var(_)
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
fn check_prim_sends_in_body(stmts: &[Stmt]) -> Result<(), EvalError> {
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

#[cfg(test)]
#[path = "tests/eval.rs"]
mod tests;
