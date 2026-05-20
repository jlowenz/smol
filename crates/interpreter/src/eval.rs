use std::cell::{Cell, RefCell};
use std::sync::{Arc, OnceLock};

use thiserror::Error;

use crate::ast::*;
use crate::class::{Class, Method};
use crate::class_registry::ClassRegistry;
use crate::env::Env;
use crate::namespace::Namespace;
use crate::object::{IndexedStorage, Object};
use crate::parser::Parser;
use crate::value::{Block, Value};

#[derive(Debug, Error, Clone)]
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
    /// Non-local `^`-return carrying the value and the method-stack depth of the
    /// frame that should catch it. Only the `invoke_method` / `invoke_class_method`
    /// whose depth matches will unwrap this; all others propagate it unchanged.
    #[error("non-local return")]
    Return(Value, usize),
}

/// The self-binding for the currently-executing method frame.
#[derive(Clone)]
pub enum SelfBinding {
    Instance { receiver: Arc<Object>, defining_class: Arc<Class> },
    ClassSide { class: Arc<Class>, defining_class: Arc<Class> },
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
            Some(SelfBinding::Instance { receiver, .. }) => {
                // 1. local frame (params, method locals, self)
                if let Some(v) = env.lookup_top_frame(name) {
                    return Some(v);
                }
                // 2. instance variables
                if let Some(v) = receiver.get_slot(name) {
                    return Some(v);
                }
                // 3. class variables
                if let Some(owner) = receiver.class.lookup_class_var_owner(name)
                    && let Some(v) = owner.class_var_storage.borrow().get(name).cloned()
                {
                    return Some(v);
                }
                // 4. outer env (globals)
                env.lookup_outer_frames(name)
            }
            Some(SelfBinding::ClassSide { class, .. }) => {
                // 1. local frame
                if let Some(v) = env.lookup_top_frame(name) {
                    return Some(v);
                }
                // 2. class variables (no instance variables — class-side has none)
                if let Some(owner) = class.lookup_class_var_owner(name)
                    && let Some(v) = owner.class_var_storage.borrow().get(name).cloned()
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
                Some(SelfBinding::Instance { receiver, .. }) => {
                    // Check ivar first.
                    if receiver.slots.borrow().contains_key(name.as_ref()) {
                        let ret = value.clone();
                        receiver.set_slot(name.clone(), value);
                        return Ok(ret);
                    }
                    // Check class var.
                    if let Some(owner) = receiver.class.lookup_class_var_owner(name.as_ref()) {
                        let ret = value.clone();
                        owner.class_var_storage.borrow_mut().insert(name.clone(), value);
                        return Ok(ret);
                    }
                }
                Some(SelfBinding::ClassSide { class, .. }) => {
                    // No ivar check for class-side.
                    if let Some(owner) = class.lookup_class_var_owner(name.as_ref()) {
                        let ret = value.clone();
                        owner.class_var_storage.borrow_mut().insert(name.clone(), value);
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
                let depth = self.current_block_home.get()
                    .unwrap_or_else(|| self.method_depth.get());
                Err(EvalError::Return(v, depth))
            }
            Stmt::MethodDef(def) => {
                // Look up the class from env first (covers non-Kernel namespace classes).
                let class = match self.lookup_var(&def.class_name, env) {
                    Some(Value::Class(c)) => c,
                    Some(_) => return Err(EvalError::TypeError(
                        format!("{} is not a class", def.class_name)
                    )),
                    None => return Err(EvalError::UndefinedVariable(def.class_name.clone())),
                };
                let method = Arc::new(Method {
                    pattern: def.pattern.clone(),
                    locals: def.locals.clone(),
                    body: def.body.clone(),
                });
                let selector = def.pattern.selector();
                if def.is_class_side {
                    class.class_methods.borrow_mut().insert(selector, method);
                } else {
                    class.methods.borrow_mut().insert(selector, method);
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
            Expr::Block { params, locals, body } => {
                // Blocks inherit the home depth of their creation context: the same method
                // that an enclosing block targets, or the current method depth if no block
                // context is active. This ensures nested blocks all point to the right frame.
                let home_depth = self.current_block_home.get()
                    .unwrap_or_else(|| self.method_depth.get());
                Ok(Value::Block(Arc::new(Block {
                    params: params.clone(),
                    locals: locals.clone(),
                    body: body.clone(),
                    env: env.clone(),
                    home_depth,
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
            Expr::NamespaceRef { namespace_path, name } => {
                self.eval_namespace_ref(namespace_path, name, env)
            }
            Expr::ArrayLiteral(elements) => {
                self.eval_array_literal(elements, env)
            }
        }
    }

    fn eval_array_literal(&self, elements: &[Literal], env: &mut Env) -> Result<Value, EvalError> {
        let array_class = self.registry.kernel()
            .lookup_class("Array")
            .ok_or_else(|| EvalError::UndefinedVariable(Arc::from("Array")))?;
        let obj = Arc::new(Object::new_with_indexed(array_class, elements.len()));
        for (i, elem) in elements.iter().enumerate() {
            let v = self.eval_literal_as_value(elem, env)?;
            if let Some(IndexedStorage::Pointer(slots)) = obj.indexed.as_ref() {
                slots.borrow_mut()[i] = v;
            }
        }
        Ok(Value::Object(obj))
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
                let selector: Arc<str> =
                    parts.iter().map(|(k, _)| k.as_ref()).collect::<String>().into();
                let mut args = Vec::with_capacity(parts.len());
                for (_, expr) in parts {
                    args.push(self.eval_expr(expr, env)?);
                }
                self.dispatch(recv, selector, args, env)
            }
        }
    }

    /// Evaluate message args for a super send without evaluating the receiver.
    fn eval_message_args(&self, message: &Message, env: &mut Env) -> Result<(Arc<str>, Vec<Value>), EvalError> {
        match message {
            Message::Unary(sel) => Ok((sel.clone(), vec![])),
            Message::Binary { op, arg } => {
                let v = self.eval_expr(arg, env)?;
                Ok((op.clone(), vec![v]))
            }
            Message::Keyword { parts } => {
                let selector: Arc<str> = parts.iter().map(|(k, _)| k.as_ref()).collect::<String>().into();
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
        let defining_class = self.current_defining_class().ok_or_else(|| {
            EvalError::TypeError("super used outside method context".into())
        })?;
        let superclass = defining_class.superclass.as_ref().ok_or_else(|| {
            EvalError::TypeError(format!(
                "super sent in {} which has no superclass",
                defining_class.name
            ))
        })?.clone();
        let (selector, args) = self.eval_message_args(message, env)?;

        // Clone the top binding to avoid holding the RefCell borrow during invoke.
        let top = self.method_stack.borrow().last().cloned();
        match top {
            Some(SelfBinding::Instance { receiver, .. }) => {
                match superclass.lookup(&selector) {
                    Some((method, def_class)) => {
                        self.invoke_method(receiver, def_class, method, args, env)
                    }
                    None => Err(EvalError::MessageNotUnderstood {
                        receiver_kind: "Object",
                        selector,
                    }),
                }
            }
            Some(SelfBinding::ClassSide { class, .. }) => {
                match superclass.lookup_class_method(&selector) {
                    Some((method, def_class)) => {
                        self.invoke_class_method(class, def_class, method, args, env)
                    }
                    // No user-defined super method; fall through to built-in class
                    // dispatch so `super new` / `super new:` reach the Rust primitives
                    // without re-entering the receiver's own `new` override.
                    None => crate::builtins::dispatch_class_builtin(self, class, selector, args, env),
                }
            }
            None => Err(EvalError::TypeError("super used outside method context".into())),
        }
    }

    /// Evaluate `Expr::NamespaceRef`: resolve a `Ns::ClassName` path.
    fn eval_namespace_ref(
        &self,
        namespace_path: &[Arc<str>],
        name: &Arc<str>,
        env: &mut Env,
    ) -> Result<Value, EvalError> {
        let first = namespace_path.first().ok_or_else(|| {
            EvalError::TypeError("empty namespace path".into())
        })?;
        let mut ns = match self.lookup_var(first.as_ref(), env) {
            Some(Value::Namespace(ns)) => ns,
            Some(other) => return Err(EvalError::TypeError(format!(
                "{first} is not a namespace, got {}",
                other.kind()
            ))),
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
            .map(Value::Class)
            .ok_or_else(|| EvalError::TypeError(format!(
                "class {name} not found in namespace {}",
                ns.name
            )))
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

    /// Invoke a user-defined method on a receiver object.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: method locals and `self` are bound in a fresh frame; the frame
    ///   is cleaned up on return; `EvalError::Return` whose depth matches this
    ///   frame is caught and unwrapped; Returns targeting an outer frame propagate.
    pub fn invoke_method(
        &self,
        receiver: Arc<Object>,
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
        method_env.define(self_sym(), Value::Object(receiver.clone()));
        for (param, val) in params.iter().zip(args.iter()) {
            method_env.define(param.clone(), val.clone());
        }
        for local in &method.locals {
            method_env.define(local.clone(), Value::Nil);
        }
        self.push_self(SelfBinding::Instance {
            receiver: receiver.clone(),
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
            Ok(v) => Ok(v),
            Err(EvalError::Return(v, d)) if d == my_depth => Ok(v),
            Err(e) => Err(e),
        }
    }

    /// Invoke a user-defined class-side method.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: `self` is bound to `Value::Class(class)` in the method frame;
    ///   `EvalError::Return` whose depth matches this frame is caught and unwrapped.
    pub fn invoke_class_method(
        &self,
        class: Arc<Class>,
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
        method_env.define(self_sym(), Value::Class(class.clone()));
        for (param, val) in params.iter().zip(args.iter()) {
            method_env.define(param.clone(), val.clone());
        }
        for local in &method.locals {
            method_env.define(local.clone(), Value::Nil);
        }
        self.push_self(SelfBinding::ClassSide {
            class: class.clone(),
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
            Ok(v) => Ok(v),
            Err(EvalError::Return(v, d)) if d == my_depth => Ok(v),
            Err(e) => Err(e),
        }
    }

    /// After allocating a new object, send `initialize` if it is defined.
    pub fn send_initialize(
        &self,
        obj: Arc<Object>,
        env: &mut Env,
    ) -> Result<Arc<Object>, EvalError> {
        if let Some((method, def_class)) = obj.class.lookup("initialize") {
            self.invoke_method(obj.clone(), def_class, method, vec![], env)?;
        }
        Ok(obj)
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
        Literal::Array(_) => unreachable!("Literal::Array must be evaluated via eval_array_literal"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn eval(src: &str) -> Value {
        let prog = Parser::parse(src).expect("parse");
        let ev = Evaluator::new();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        ev.eval_program(&prog.stmts, &mut env).expect("eval")
    }

    fn eval_err(src: &str) -> EvalError {
        let prog = Parser::parse(src).expect("parse");
        let ev = Evaluator::new();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        ev.eval_program(&prog.stmts, &mut env).expect_err("expected error")
    }

    #[test]
    fn integer_arithmetic() {
        assert_eq!(eval("3 + 4"), Value::Int(7));
        assert_eq!(eval("10 - 3"), Value::Int(7));
        assert_eq!(eval("3 * 4"), Value::Int(12));
        assert_eq!(eval("10 // 3"), Value::Int(3));
        assert_eq!(eval("10 \\\\ 3"), Value::Int(1));
    }

    #[test]
    fn integer_comparison() {
        assert_eq!(eval("3 < 4"), Value::Bool(true));
        assert_eq!(eval("4 < 3"), Value::Bool(false));
        assert_eq!(eval("3 = 3"), Value::Bool(true));
        assert_eq!(eval("3 ~= 4"), Value::Bool(true));
    }

    #[test]
    fn boolean_conditional() {
        assert_eq!(eval("true ifTrue: ['yes']"), Value::Str(Arc::from("yes")));
        assert_eq!(eval("false ifFalse: ['no']"), Value::Str(Arc::from("no")));
        assert_eq!(
            eval("true ifTrue: ['yes'] ifFalse: ['no']"),
            Value::Str(Arc::from("yes"))
        );
    }

    #[test]
    fn block_value() {
        assert_eq!(eval("[:x | x * x] value: 7"), Value::Int(49));
    }

    #[test]
    fn times_repeat() {
        assert_eq!(eval("| n | n := 0. 3 timesRepeat: [n := n + 1]. n"), Value::Int(3));
    }

    #[test]
    fn while_true() {
        assert_eq!(
            eval("| n | n := 0. [n < 5] whileTrue: [n := n + 1]. n"),
            Value::Int(5)
        );
    }

    #[test]
    fn assignment_and_variable() {
        assert_eq!(eval("x := 42. x"), Value::Int(42));
    }

    #[test]
    fn local_decl_binds_nil() {
        assert_eq!(eval("| x | x"), Value::Nil);
    }

    #[test]
    fn yourself_returns_receiver() {
        assert_eq!(eval("42 yourself"), Value::Int(42));
    }

    #[test]
    fn cascade() {
        // The result of a cascade is the last message's return value.
        // `3 yourself; yourself` — both return 3.
        assert_eq!(eval("3 yourself; yourself"), Value::Int(3));
    }

    #[test]
    fn print_string_int() {
        assert_eq!(eval("42 printString"), Value::Str(Arc::from("42")));
    }

    #[test]
    fn print_string_float() {
        assert_eq!(eval("1.5 printString"), Value::Str(Arc::from("1.5")));
    }

    #[test]
    fn undefined_variable_error() {
        matches!(eval_err("x"), EvalError::UndefinedVariable(_));
    }

    #[test]
    fn message_not_understood() {
        matches!(eval_err("42 unknownMsg"), EvalError::MessageNotUnderstood { .. });
    }

    #[test]
    fn to_do_loop() {
        assert_eq!(
            eval("| sum | sum := 0. 1 to: 5 do: [:i | sum := sum + i]. sum"),
            Value::Int(15)
        );
    }

    #[test]
    fn string_concat() {
        assert_eq!(eval("'hello' , ' world'"), Value::Str(Arc::from("hello world")));
    }

    #[test]
    fn nil_is_nil() {
        assert_eq!(eval("nil isNil"), Value::Bool(true));
        assert_eq!(eval("42 isNil"), Value::Bool(false));
    }

    #[test]
    fn class_definition_and_new() {
        let src = r#"
Object subclass: #Point instanceVariableNames: 'x y'.
| p |
p := Point new.
p
"#;
        let v = eval(src);
        assert_eq!(format!("{v}"), "a Point");
    }

    #[test]
    fn method_def_and_call() {
        let src = r#"
Object subclass: #Counter instanceVariableNames: 'count'.
Counter >> initialize [
    count := 0
]
Counter >> increment [
    count := count + 1
]
Counter >> count [
    ^count
]
| c |
c := Counter new.
c increment.
c increment.
c increment.
c count
"#;
        assert_eq!(eval(src), Value::Int(3));
    }

    #[test]
    fn class_variable_shared() {
        let src = r#"
Object subclass: #Counted instanceVariableNames: '' classVariableNames: 'Total'.
Counted >> initialize [
    Total := (Total isNil) ifTrue: [1] ifFalse: [Total + 1]
]
Counted >> total [
    ^Total
]
| a b |
a := Counted new.
b := Counted new.
b total
"#;
        assert_eq!(eval(src), Value::Int(2));
    }

    #[test]
    fn largeint_overflow_promotion() {
        // i64::MAX + 1 should produce a LargeInt
        let src = "9223372036854775807 + 1";
        let v = eval(src);
        assert!(matches!(v, Value::LargeInt(_)));
    }

    #[test]
    fn largeint_literal() {
        let src = "99999999999999999999 + 1";
        let v = eval(src);
        assert!(matches!(v, Value::LargeInt(_)));
    }

    #[test]
    fn object_print_string() {
        let src = r#"
Object subclass: #Foo instanceVariableNames: ''.
Foo new printString
"#;
        assert_eq!(eval(src), Value::Str(Arc::from("a Foo")));
    }

    #[test]
    fn super_dispatch() {
        let src = r#"
Object subclass: #Animal instanceVariableNames: 'name'.
Animal >> speak [
    ^'...'
]
Animal subclass: #Dog instanceVariableNames: ''.
Dog >> speak [
    ^super speak , ' Woof!'
]
| d |
d := Dog new.
d speak
"#;
        assert_eq!(eval(src), Value::Str(Arc::from("... Woof!")));
    }

    #[test]
    fn class_side_method() {
        let src = r#"
Object subclass: #Counter instanceVariableNames: 'count'.
Counter class >> startingAt: n [
    | c |
    c := self new.
    c setCount: n.
    ^c
]
Counter >> setCount: n [ count := n ]
Counter >> count [ ^count ]
| c |
c := Counter startingAt: 42.
c count
"#;
        assert_eq!(eval(src), Value::Int(42));
    }

    #[test]
    fn namespace_ref() {
        let src = r#"
Kernel::Object new printString
"#;
        assert_eq!(eval(src), Value::Str(Arc::from("a Object")));
    }

    #[test]
    fn namespace_do_scopes_class() {
        let src = r#"
| result |
result := 0.
Kernel do: [ result := 1 ].
result
"#;
        assert_eq!(eval(src), Value::Int(1));
    }

    #[test]
    fn non_kernel_class_scoped_to_namespace() {
        use crate::namespace::Namespace;

        let ev = Evaluator::new();
        let my_lib = Arc::new(Namespace::new(Arc::from("MyLib"), None));
        ev.registry.register_namespace(my_lib);
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);

        // Inside MyLib do: [...], Widget is accessible by simple name.
        let result = ev.eval_program(
            &Parser::parse(r#"
| result |
MyLib do: [
    | w |
    Object subclass: #Widget instanceVariableNames: 'val'.
    Widget >> setVal: v [ val := v ]
    Widget >> val [ ^val ]
    w := Widget new.
    w setVal: 42.
    result := w val
].
result
"#).expect("parse").stmts,
            &mut env,
        ).expect("eval");
        assert_eq!(result, Value::Int(42));

        // After the block, bare Widget is not in scope.
        let err = ev.eval_program(
            &Parser::parse("Widget").expect("parse").stmts,
            &mut env,
        ).expect_err("Widget should be out of scope after do: block");
        assert!(matches!(err, EvalError::UndefinedVariable(_)));

        // Widget remains accessible via the namespace ref.
        let via_ns = ev.eval_program(
            &Parser::parse("MyLib::Widget new printString").expect("parse").stmts,
            &mut env,
        ).expect("namespace ref");
        assert_eq!(via_ns, Value::Str(Arc::from("a Widget")));
    }

    // ── Kernel loading ────────────────────────────────────────────────────────

    #[test]
    fn kernel_load_does_not_panic() {
        let _ev = Evaluator::new();
    }

    #[test]
    fn kernel_classes_in_env() {
        let ev = Evaluator::new();
        let mut env = Env::new();
        ev.registry.populate_env(&mut env);
        assert!(matches!(env.get("Array"), Some(Value::Class(_))));
        assert!(matches!(env.get("ByteArray"), Some(Value::Class(_))));
        assert!(matches!(env.get("OrderedCollection"), Some(Value::Class(_))));
    }

    // ── Array protocol ────────────────────────────────────────────────────────

    #[test]
    fn array_new_size_and_at() {
        assert_eq!(eval("(Array new: 3) size"), Value::Int(3));
        assert_eq!(eval("| a | a := Array new: 3. a at: 0"), Value::Nil);
    }

    #[test]
    fn array_at_put_and_at() {
        assert_eq!(
            eval("| a | a := Array new: 3. a at: 1 put: 99. a at: 1"),
            Value::Int(99)
        );
    }

    #[test]
    fn array_is_empty_not_empty() {
        assert_eq!(eval("(Array new: 0) isEmpty"), Value::Bool(true));
        assert_eq!(eval("(Array new: 2) notEmpty"), Value::Bool(true));
    }

    #[test]
    fn array_first_last() {
        assert_eq!(
            eval("| a | a := Array new: 3. a at: 0 put: 10. a at: 2 put: 30. a first"),
            Value::Int(10)
        );
        assert_eq!(
            eval("| a | a := Array new: 3. a at: 0 put: 10. a at: 2 put: 30. a last"),
            Value::Int(30)
        );
    }

    #[test]
    fn array_do_iterates() {
        assert_eq!(
            eval("| a s | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. s := 0. a do: [:e | s := s + e]. s"),
            Value::Int(6)
        );
    }

    #[test]
    fn array_collect_maps() {
        let src = "| a r | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. r := a collect: [:e | e * 2]. r at: 2";
        assert_eq!(eval(src), Value::Int(6));
    }

    #[test]
    fn array_select_returns_ordered_collection() {
        let src = "| a r | a := Array new: 4. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a at: 3 put: 4. r := a select: [:e | e > 2]. r size";
        assert_eq!(eval(src), Value::Int(2));
    }

    #[test]
    fn array_reject_returns_ordered_collection() {
        let src = "| a r | a := Array new: 4. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a at: 3 put: 4. r := a reject: [:e | e > 2]. r size";
        assert_eq!(eval(src), Value::Int(2));
    }

    #[test]
    fn array_detect() {
        let src = "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 5. a at: 2 put: 3. a detect: [:e | e > 4]";
        assert_eq!(eval(src), Value::Int(5));
    }

    #[test]
    fn array_detect_if_none() {
        let src = "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a detect: [:e | e > 10] ifNone: [99]";
        assert_eq!(eval(src), Value::Int(99));
    }

    #[test]
    fn array_inject_into_sum() {
        let src = "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a inject: 0 into: [:acc :e | acc + e]";
        assert_eq!(eval(src), Value::Int(6));
    }

    #[test]
    fn array_includes() {
        let src = "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a includes: 2";
        assert_eq!(eval(src), Value::Bool(true));
        let src2 = "| a | a := Array new: 3. a at: 0 put: 1. a at: 1 put: 2. a at: 2 put: 3. a includes: 9";
        assert_eq!(eval(src2), Value::Bool(false));
    }

    #[test]
    fn array_copy_from_to() {
        let src = "| a r | a := Array new: 5. 0 to: 4 do: [:i | a at: i put: i]. r := a copyFrom: 1 to: 3. r size";
        assert_eq!(eval(src), Value::Int(3));
        let src2 = "| a r | a := Array new: 5. 0 to: 4 do: [:i | a at: i put: i]. r := a copyFrom: 1 to: 3. r at: 0";
        assert_eq!(eval(src2), Value::Int(1));
    }

    #[test]
    fn array_equality() {
        let src = "| a b | a := Array new: 2. a at: 0 put: 1. a at: 1 put: 2. b := Array new: 2. b at: 0 put: 1. b at: 1 put: 2. a = b";
        assert_eq!(eval(src), Value::Bool(true));
        let src2 = "| a b | a := Array new: 2. a at: 0 put: 1. a at: 1 put: 2. b := Array new: 2. b at: 0 put: 1. b at: 1 put: 9. a = b";
        assert_eq!(eval(src2), Value::Bool(false));
    }

    #[test]
    fn array_print_string() {
        let src = "| a | a := Array new: 2. a at: 0 put: 1. a at: 1 put: 2. a printString";
        assert_eq!(eval(src), Value::Str(Arc::from("(1 2 )")));
    }

    // ── ByteArray protocol ────────────────────────────────────────────────────

    #[test]
    fn bytearray_new_size_at_put() {
        assert_eq!(eval("(ByteArray new: 4) size"), Value::Int(4));
        let src = "| b | b := ByteArray new: 4. b at: 0 put: 65. b at: 0";
        assert_eq!(eval(src), Value::Int(65));
    }

    #[test]
    fn bytearray_is_empty() {
        assert_eq!(eval("(ByteArray new: 0) isEmpty"), Value::Bool(true));
        assert_eq!(eval("(ByteArray new: 1) notEmpty"), Value::Bool(true));
    }

    #[test]
    fn bytearray_as_string() {
        let src = "| b | b := ByteArray new: 3. b at: 0 put: 65. b at: 1 put: 66. b at: 2 put: 67. b asString";
        assert_eq!(eval(src), Value::Str(Arc::from("ABC")));
    }

    #[test]
    fn bytearray_do() {
        let src = "| b s | b := ByteArray new: 3. b at: 0 put: 10. b at: 1 put: 20. b at: 2 put: 30. s := 0. b do: [:e | s := s + e]. s";
        assert_eq!(eval(src), Value::Int(60));
    }

    #[test]
    fn bytearray_copy_from_to() {
        let src = "| b r | b := ByteArray new: 5. 0 to: 4 do: [:i | b at: i put: i * 10]. r := b copyFrom: 1 to: 3. r size";
        assert_eq!(eval(src), Value::Int(3));
        let src2 = "| b r | b := ByteArray new: 5. 0 to: 4 do: [:i | b at: i put: i * 10]. r := b copyFrom: 1 to: 3. r at: 0";
        assert_eq!(eval(src2), Value::Int(10));
    }

    // ── OrderedCollection protocol ────────────────────────────────────────────

    #[test]
    fn oc_new_is_empty() {
        assert_eq!(eval("OrderedCollection new isEmpty"), Value::Bool(true));
        assert_eq!(eval("OrderedCollection new size"), Value::Int(0));
    }

    #[test]
    fn oc_add_and_size() {
        let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc size";
        assert_eq!(eval(src), Value::Int(3));
    }

    #[test]
    fn oc_at() {
        let src = "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc at: 1";
        assert_eq!(eval(src), Value::Int(20));
    }

    #[test]
    fn oc_first_last() {
        let src = "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc add: 30. oc first";
        assert_eq!(eval(src), Value::Int(10));
        let src2 = "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc add: 30. oc last";
        assert_eq!(eval(src2), Value::Int(30));
    }

    #[test]
    fn oc_add_first_prepends() {
        let src = "| oc | oc := OrderedCollection new. oc add: 2. oc add: 3. oc addFirst: 1. oc at: 0";
        assert_eq!(eval(src), Value::Int(1));
    }

    #[test]
    fn oc_remove_first() {
        let src = "| oc v | oc := OrderedCollection new. oc add: 10. oc add: 20. v := oc removeFirst. v";
        assert_eq!(eval(src), Value::Int(10));
        let src2 = "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc removeFirst. oc size";
        assert_eq!(eval(src2), Value::Int(1));
    }

    #[test]
    fn oc_remove_last() {
        let src = "| oc v | oc := OrderedCollection new. oc add: 10. oc add: 20. v := oc removeLast. v";
        assert_eq!(eval(src), Value::Int(20));
        let src2 = "| oc | oc := OrderedCollection new. oc add: 10. oc add: 20. oc removeLast. oc size";
        assert_eq!(eval(src2), Value::Int(1));
    }

    #[test]
    fn oc_remove_if_absent() {
        let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc remove: 2 ifAbsent: [0]. oc size";
        assert_eq!(eval(src), Value::Int(2));
        let src2 = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc remove: 9 ifAbsent: [99]";
        assert_eq!(eval(src2), Value::Int(99));
    }

    #[test]
    fn oc_do_iterates() {
        let src = "| oc s | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. s := 0. oc do: [:e | s := s + e]. s";
        assert_eq!(eval(src), Value::Int(6));
    }

    #[test]
    fn oc_collect_maps() {
        let src = "| oc r | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. r := oc collect: [:e | e * 10]. r at: 1";
        assert_eq!(eval(src), Value::Int(20));
    }

    #[test]
    fn oc_select_filters() {
        let src = "| oc r | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc add: 4. r := oc select: [:e | e > 2]. r size";
        assert_eq!(eval(src), Value::Int(2));
    }

    #[test]
    fn oc_reject_filters() {
        let src = "| oc r | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc add: 4. r := oc reject: [:e | e > 2]. r size";
        assert_eq!(eval(src), Value::Int(2));
    }

    #[test]
    fn oc_detect() {
        let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 5. oc add: 3. oc detect: [:e | e > 4]";
        assert_eq!(eval(src), Value::Int(5));
    }

    #[test]
    fn oc_detect_if_none() {
        let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc detect: [:e | e > 10] ifNone: [42]";
        assert_eq!(eval(src), Value::Int(42));
    }

    #[test]
    fn oc_inject_into_sum() {
        let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc add: 3. oc inject: 0 into: [:acc :e | acc + e]";
        assert_eq!(eval(src), Value::Int(6));
    }

    #[test]
    fn oc_includes() {
        let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc includes: 2";
        assert_eq!(eval(src), Value::Bool(true));
        let src2 = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc includes: 9";
        assert_eq!(eval(src2), Value::Bool(false));
    }

    #[test]
    fn oc_copy_from_to() {
        let src = "| oc r | oc := OrderedCollection new. oc add: 10. oc add: 20. oc add: 30. r := oc copyFrom: 0 to: 1. r size";
        assert_eq!(eval(src), Value::Int(2));
        let src2 = "| oc r | oc := OrderedCollection new. oc add: 10. oc add: 20. oc add: 30. r := oc copyFrom: 0 to: 1. r at: 0";
        assert_eq!(eval(src2), Value::Int(10));
    }

    #[test]
    fn oc_equality() {
        let src = "| a b | a := OrderedCollection new. b := OrderedCollection new. a add: 1. b add: 1. a = b";
        assert_eq!(eval(src), Value::Bool(true));
        let src2 = "| a b | a := OrderedCollection new. b := OrderedCollection new. a add: 1. b add: 2. a = b";
        assert_eq!(eval(src2), Value::Bool(false));
    }

    #[test]
    fn oc_not_equal() {
        let src = "| a b | a := OrderedCollection new. b := OrderedCollection new. a add: 1. b add: 2. a ~= b";
        assert_eq!(eval(src), Value::Bool(true));
    }

    #[test]
    fn oc_print_string() {
        let src = "| oc | oc := OrderedCollection new. oc add: 1. oc add: 2. oc printString";
        assert_eq!(eval(src), Value::Str(Arc::from("OrderedCollection (1 2 )")));
    }

    #[test]
    fn oc_growth_trigger() {
        // Add more than initial capacity of 8 to force pvtGrow.
        let src = "| oc | oc := OrderedCollection new. 1 to: 12 do: [:i | oc add: i]. oc size";
        assert_eq!(eval(src), Value::Int(12));
        let src2 = "| oc | oc := OrderedCollection new. 1 to: 12 do: [:i | oc add: i]. oc at: 11";
        assert_eq!(eval(src2), Value::Int(12));
    }

    #[test]
    fn oc_remove_first_empty_errors() {
        let src = "OrderedCollection new removeFirst";
        assert!(matches!(eval_err(src), EvalError::TypeError(_)));
    }

    #[test]
    fn oc_remove_last_empty_errors() {
        let src = "OrderedCollection new removeLast";
        assert!(matches!(eval_err(src), EvalError::TypeError(_)));
    }

    #[test]
    fn oc_at_out_of_bounds_errors() {
        let src = "| oc | oc := OrderedCollection new. oc add: 1. oc at: 5";
        assert!(matches!(eval_err(src), EvalError::TypeError(_)));
    }

    // ── Nested array literals ─────────────────────────────────────────────────

    #[test]
    fn array_literal_basic() {
        assert_eq!(eval("#(1 2 3) size"), Value::Int(3));
        assert_eq!(eval("#(1 2 3) at: 1"), Value::Int(2));
    }

    #[test]
    fn array_literal_empty() {
        assert_eq!(eval("#() isEmpty"), Value::Bool(true));
    }

    #[test]
    fn array_literal_nested() {
        assert_eq!(eval("#(#(0 1) #(2 3)) size"), Value::Int(2));
        let src = "(#(#(0 1) #(2 3)) at: 0) at: 1";
        assert_eq!(eval(src), Value::Int(1));
    }

    #[test]
    fn array_literal_mixed_types() {
        assert_eq!(eval("#(1 'hello' true nil) at: 1"), Value::Str(Arc::from("hello")));
        assert_eq!(eval("#(1 'hello' true nil) at: 2"), Value::Bool(true));
        assert_eq!(eval("#(1 'hello' true nil) at: 3"), Value::Nil);
    }

    #[test]
    fn array_literal_symbols() {
        assert_eq!(eval("#(#foo #bar) at: 0"), Value::Symbol(Arc::from("foo")));
    }

    #[test]
    fn array_literal_passed_to_do() {
        let src = "| s | s := 0. #(1 2 3 4) do: [:e | s := s + e]. s";
        assert_eq!(eval(src), Value::Int(10));
    }

    #[test]
    fn array_literal_non_literal_token_is_parse_error() {
        let result = crate::parser::Parser::parse("#(1 + 2)");
        assert!(result.is_err());
    }

    // ── Integration tests ─────────────────────────────────────────────────────

    #[test]
    fn oc_as_queue() {
        let src = r#"
| q |
q := OrderedCollection new.
q add: 'a'.
q add: 'b'.
q add: 'c'.
q removeFirst , (q removeFirst) , (q removeFirst)
"#;
        assert_eq!(eval(src), Value::Str(Arc::from("abc")));
    }

    #[test]
    fn collect_then_inject() {
        let src = r#"
| oc |
oc := OrderedCollection new.
1 to: 5 do: [:i | oc add: i].
(oc collect: [:e | e * e]) inject: 0 into: [:acc :e | acc + e]
"#;
        assert_eq!(eval(src), Value::Int(55)); // 1+4+9+16+25
    }

    #[test]
    fn user_subclass_of_array() {
        let src = r#"
Array subclass: #Stack instanceVariableNames: '' indexedSlots: #pointer.
Stack >> push: v [ self _primAt: 0 put: v ]
Stack >> top   [ ^self _primAt: 0 ]
| s |
s := Stack new: 1.
s push: 42.
s top
"#;
        assert_eq!(eval(src), Value::Int(42));
    }

    #[test]
    fn nested_array_literal_with_do() {
        let src = r#"
| outer s |
outer := #(#(1 2) #(3 4)).
s := 0.
outer do: [:row | row do: [:e | s := s + e]].
s
"#;
        assert_eq!(eval(src), Value::Int(10));
    }

    #[test]
    fn block_with_locals() {
        let prog = "b := [:i | | a | a := 42. a := a + i. a]. (b value: 42) printString.";
        assert_eq!(eval(prog), Value::Str(Arc::from("84")));
    }

}
