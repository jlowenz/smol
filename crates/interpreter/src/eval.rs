use std::cell::{Cell, RefCell};
use std::sync::{Arc, OnceLock};

use thiserror::Error;

use crate::ast::*;
use crate::class::{Class, Method};
use crate::class_registry::ClassRegistry;
use crate::env::Env;
use crate::namespace::Namespace;
use crate::object::Object;
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
    /// Non-local `^`-return. Propagates up the call stack until caught by the
    /// enclosing method frame. Not an error condition; handled by eval_program.
    #[error("non-local return")]
    Return(Value),
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
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl Evaluator {
    pub fn new() -> Self {
        let registry = Arc::new(ClassRegistry::new());
        let kernel = registry.kernel();
        Self {
            registry,
            method_stack: RefCell::new(Vec::new()),
            method_depth: Cell::new(0),
            active_namespace: RefCell::new(kernel),
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
                if let Some(owner) = receiver.class.lookup_class_var_owner(name) {
                    if let Some(v) = owner.class_var_storage.borrow().get(name).cloned() {
                        return Some(v);
                    }
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
                if let Some(owner) = class.lookup_class_var_owner(name) {
                    if let Some(v) = owner.class_var_storage.borrow().get(name).cloned() {
                        return Some(v);
                    }
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
                Err(EvalError::Return(v))
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
                Ok(Value::Block(Arc::new(Block {
                    params: params.clone(),
                    locals: locals.clone(),
                    body: body.clone(),
                    env: env.clone(),
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
                    None => Err(EvalError::MessageNotUnderstood {
                        receiver_kind: "Class",
                        selector,
                    }),
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
    /// Creates a new frame, binds params, runs the body, catches `^`-returns.
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
        // Work in the block's captured environment.
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
        match result {
            Ok(v) => Ok(v),
            Err(EvalError::Return(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    /// Invoke a user-defined method on a receiver object.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: method locals and `self` are bound in a fresh frame; the frame
    ///   is cleaned up on return; `EvalError::Return` is caught and unwrapped.
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
        let result = self.eval_program(&method.body, &mut method_env);
        self.pop_self();
        method_env.pop_frame();
        match result {
            Ok(v) => Ok(v),
            Err(EvalError::Return(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    /// Invoke a user-defined class-side method.
    ///
    /// Requires: `args.len() == method.pattern.params().len()`.
    /// Ensures: `self` is bound to `Value::Class(class)` in the method frame.
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
        let result = self.eval_program(&method.body, &mut method_env);
        self.pop_self();
        method_env.pop_frame();
        match result {
            Ok(v) => Ok(v),
            Err(EvalError::Return(v)) => Ok(v),
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
}
