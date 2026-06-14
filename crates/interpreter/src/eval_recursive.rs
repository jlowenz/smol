use std::sync::Arc;

use crate::ast::*;
use crate::class::{Class, Method};
use crate::class_registry::ClassRegistry;
use crate::entity::{InstanceHandle, InstanceKind};
use crate::env::Env;
use crate::eval::{EvalError, Evaluator, SelfBinding, eval_literal, self_sym};
use crate::object::{IndexedStorage, Object};
use crate::parser::Parser;
use crate::value::{Block, Value};

/// Fully recursive Smol evaluator. Every Smol method call consumes a Rust
/// stack frame; used as the reference implementation in differential tests.
///
/// Invariant: `inner` always points to a valid `Evaluator` (shared state).
/// All mutable state lives inside `inner`; `RecursiveEvaluator` itself is
/// immutable and `Clone`-able by cloning the `Arc`.
pub struct RecursiveEvaluator {
    pub(crate) inner: Arc<Evaluator>,
}

impl RecursiveEvaluator {
    /// Create a recursive evaluator and evaluate `kernel_recursive.smol`.
    ///
    /// Ensures: after construction the registry contains all kernel classes
    ///   (Array, OrderedCollection, etc.) registered via the recursive path.
    pub fn new() -> Self {
        let ev = Self::new_without_kernel();
        let mut bootstrap_env = Env::new();
        ev.inner.registry.populate_env(&mut bootstrap_env);
        let src = include_str!("kernel_recursive.smol");
        let prog = Parser::parse(src).expect("kernel_recursive.smol: parse error");
        ev.eval_program(&prog.stmts, &mut bootstrap_env)
            .expect("kernel_recursive.smol: eval error");
        ev
    }

    /// Create a recursive evaluator without loading `kernel_recursive.smol`.
    pub fn new_without_kernel() -> Self {
        Self {
            inner: Arc::new(Evaluator::new_without_kernel()),
        }
    }

    /// Create a recursive evaluator sharing an existing registry (for entity
    /// spawns that should share the same class set).
    pub fn with_registry(registry: Arc<ClassRegistry>) -> Self {
        Self {
            inner: Arc::new(Evaluator::with_registry(registry)),
        }
    }

    /// Signal all tracked entities to terminate.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// Temporarily sets the active namespace for the duration of `f`.
    /// Passes `self` to the closure so dispatch remains on the recursive path.
    pub(crate) fn with_namespace<F, R>(&self, ns: Arc<crate::namespace::Namespace>, f: F) -> R
    where
        F: FnOnce(&RecursiveEvaluator) -> R,
    {
        let old = self.inner.active_namespace.borrow().clone();
        *self.inner.active_namespace.borrow_mut() = ns;
        let result = f(self);
        *self.inner.active_namespace.borrow_mut() = old;
        result
    }

    // ── Public entry points ───────────────────────────────────────────────────

    /// Recursively evaluate a sequence of statements.
    ///
    /// Requires: `env` has at least one live frame; all `LocalDecl` names are
    ///   pre-bound before any expression runs.
    /// Ensures: returns the value of the last statement, or `Value::Nil` for
    ///   an empty list. `EvalError::Return` propagates to the caller.
    pub fn eval_program(&self, stmts: &[Stmt], env: &mut Env) -> Result<Value, EvalError> {
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

    /// Recursively evaluate a Block.
    ///
    /// Requires: `args.len() == block.params.len()`.
    /// Ensures: returns block's result; `EvalError::Return` propagates.
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
        let prev_home = self.inner.current_block_home.get();
        self.inner.current_block_home.set(Some(block.home_depth));
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
        self.inner.current_block_home.set(prev_home);
        result
    }

    /// Evaluate a Block with sync semantics: after each statement any
    /// `Value::Future` result is awaited before proceeding.
    ///
    /// Requires: `args.len() == block.params.len()`.
    /// Ensures: every statement result is fully resolved before the next runs.
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
        let prev_home = self.inner.current_block_home.get();
        self.inner.current_block_home.set(Some(block.home_depth));
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
        self.inner.current_block_home.set(prev_home);
        result
    }

    /// Invoke a user-defined instance method recursively.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: returns `self` (implicit Smalltalk return) unless an explicit
    ///   `^` return is encountered.
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
        self.inner.push_self(SelfBinding::Instance {
            handle: handle.clone(),
            defining_class,
        });
        let my_depth = self.inner.method_depth.get();
        let prev_home = self.inner.current_block_home.get();
        self.inner.current_block_home.set(None);
        let result = self.eval_program(&method.body, &mut method_env);
        self.inner.current_block_home.set(prev_home);
        self.inner.pop_self();
        method_env.pop_frame();
        match result {
            Ok(_) => Ok(self_value),
            Err(EvalError::Return(v, d)) if d == my_depth => Ok(v),
            Err(e) => Err(e),
        }
    }

    /// Invoke a user-defined class-side method recursively.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: returns `self` (the class) unless an explicit `^` is used.
    pub fn invoke_class_method(
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
        self.inner.push_self(SelfBinding::ClassSide {
            handle,
            defining_class,
        });
        let my_depth = self.inner.method_depth.get();
        let prev_home = self.inner.current_block_home.get();
        self.inner.current_block_home.set(None);
        let result = self.eval_program(&method.body, &mut method_env);
        self.inner.current_block_home.set(prev_home);
        self.inner.pop_self();
        method_env.pop_frame();
        match result {
            Ok(_) => Ok(self_val),
            Err(EvalError::Return(v, d)) if d == my_depth => Ok(v),
            Err(e) => Err(e),
        }
    }

    /// Invoke a user-defined method where `self` is a primitive value (e.g. `nil`).
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: returns `self_value` unless an explicit `^` is used.
    pub fn invoke_primitive_method(
        &self,
        self_value: Value,
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
        let mut method_env = env.clone();
        method_env.push_frame();
        method_env.define(self_sym(), self_value.clone());
        for (param, val) in params.iter().zip(args.iter()) {
            method_env.define(param.clone(), val.clone());
        }
        for local in &method.locals {
            method_env.define(local.clone(), Value::Nil);
        }
        self.inner.push_self(SelfBinding::Primitive {
            defining_class,
            self_value: self_value.clone(),
        });
        let my_depth = self.inner.method_depth.get();
        let prev_home = self.inner.current_block_home.get();
        self.inner.current_block_home.set(None);
        let result = self.eval_program(&method.body, &mut method_env);
        self.inner.current_block_home.set(prev_home);
        self.inner.pop_self();
        method_env.pop_frame();
        match result {
            Ok(_) => Ok(self_value),
            Err(EvalError::Return(v, d)) if d == my_depth => Ok(v),
            Err(e) => Err(e),
        }
    }

    // ── Private recursive eval helpers ────────────────────────────────────────

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
            last = crate::builtins_iterative::force_value(v)?;
        }
        Ok(last)
    }

    fn eval_stmt(&self, stmt: &Stmt, env: &mut Env) -> Result<Value, EvalError> {
        match stmt {
            Stmt::LocalDecl(_) => Ok(Value::Nil),
            Stmt::Expr(e) => self.eval_expr(e, env),
            Stmt::Assign { name, value } => {
                let v = self.eval_expr(value, env)?;
                self.inner.assign_var(name, v, env)
            }
            Stmt::Return(e) => {
                let v = self.eval_expr(e, env)?;
                let depth = self
                    .inner
                    .current_block_home
                    .get()
                    .unwrap_or_else(|| self.inner.method_depth.get());
                Err(EvalError::Return(v, depth))
            }
            Stmt::MethodDef(def) => {
                let class = match self.inner.lookup_var(&def.class_name, env) {
                    Some(Value::Instance(h))
                        if matches!(h.0.kind, InstanceKind::ClassEntity(_)) =>
                    {
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
                crate::eval::check_prim_sends_in_body(&def.body)?;
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

    pub(crate) fn eval_expr(&self, expr: &Expr, env: &mut Env) -> Result<Value, EvalError> {
        match expr {
            Expr::Super => Err(EvalError::TypeError(
                "super may only appear as a message receiver".into(),
            )),
            Expr::Lit(lit) => Ok(eval_literal(lit)),
            Expr::Var(name) => self
                .inner
                .lookup_var(name.as_ref(), env)
                .ok_or_else(|| EvalError::UndefinedVariable(name.clone())),
            Expr::Send { receiver, message } => {
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
                let home_depth = self
                    .inner
                    .current_block_home
                    .get()
                    .unwrap_or_else(|| self.inner.method_depth.get());
                Ok(Value::Block(Arc::new(Block {
                    params: params.clone(),
                    locals: locals.clone(),
                    body: body.clone(),
                    env: env.clone(),
                    home_depth,
                    home_entity: self.inner.current_entity_id.get(),
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
            } => self.inner.eval_namespace_ref(namespace_path, name, env),
            Expr::ArrayLiteral(elements) => self.eval_array_literal(elements, env),
            Expr::DynamicArray(elements) => self.eval_dynamic_array(elements, env),
        }
    }

    fn eval_array_literal(&self, elements: &[Literal], env: &mut Env) -> Result<Value, EvalError> {
        use crate::entity::{InstanceHandle, InstanceKind, InstanceState};
        let array_handle = self
            .inner
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
            let v = self.inner.eval_literal_as_value(elem, env)?;
            if let Some(IndexedStorage::Pointer(slots)) = obj.indexed.as_ref() {
                slots.lock()[i] = v;
            }
        }
        Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
            class: array_class,
            kind: InstanceKind::ValueObject(obj),
        }))))
    }

    fn eval_dynamic_array(&self, elements: &[Expr], env: &mut Env) -> Result<Value, EvalError> {
        use crate::entity::{InstanceHandle, InstanceKind, InstanceState};
        let array_handle = self
            .inner
            .registry
            .kernel()
            .lookup_class("Array")
            .ok_or_else(|| EvalError::UndefinedVariable(Arc::from("Array")))?;
        let array_class = array_handle.0.class.clone();
        let obj = Arc::new(Object::new_with_indexed(
            array_class.clone(),
            elements.len(),
        ));
        for (i, elem_expr) in elements.iter().enumerate() {
            let v = self.eval_expr(elem_expr, env)?;
            if let Some(IndexedStorage::Pointer(slots)) = obj.indexed.as_ref() {
                slots.lock()[i] = v;
            }
        }
        Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
            class: array_class,
            kind: InstanceKind::ValueObject(obj),
        }))))
    }

    fn send(&self, recv: Value, message: &Message, env: &mut Env) -> Result<Value, EvalError> {
        match message {
            Message::Unary(sel) => {
                crate::builtins_recursive::dispatch(self, recv, sel.clone(), vec![], env)
            }
            Message::Binary { op, arg } => {
                let arg_val = self.eval_expr(arg, env)?;
                crate::builtins_recursive::dispatch(self, recv, op.clone(), vec![arg_val], env)
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
                crate::builtins_recursive::dispatch(self, recv, selector, args, env)
            }
        }
    }

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

    pub(crate) fn send_super(&self, message: &Message, env: &mut Env) -> Result<Value, EvalError> {
        let defining_class = self
            .inner
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
        let top = self.inner.method_stack.borrow().last().cloned();
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
                    None => crate::builtins_recursive::dispatch_class_builtin(
                        self, handle, selector, args, env,
                    ),
                }
            }
            Some(SelfBinding::Primitive { self_value, .. }) => match superclass.lookup(&selector) {
                Some((method, def_class)) => {
                    self.invoke_primitive_method(self_value, def_class, method, args, env)
                }
                None => Err(EvalError::MessageNotUnderstood {
                    receiver_kind: "nil",
                    selector,
                }),
            },
            None => Err(EvalError::TypeError(
                "super used outside method context".into(),
            )),
        }
    }
}
