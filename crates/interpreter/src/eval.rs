use std::sync::Arc;

use thiserror::Error;

use crate::ast::*;
use crate::env::Env;
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

pub struct Evaluator;

impl Evaluator {
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
            Stmt::LocalDecl(_) => Ok(Value::Nil), // already handled in eval_program
            Stmt::Expr(e) => self.eval_expr(e, env),
            Stmt::Assign { name, value } => {
                let v = self.eval_expr(value, env)?;
                if !env.set(name.as_ref(), v.clone()) {
                    env.define(name.clone(), v.clone());
                }
                Ok(v)
            }
            Stmt::Return(e) => {
                let v = self.eval_expr(e, env)?;
                Err(EvalError::Return(v))
            }
        }
    }

    pub fn eval_expr(&self, expr: &Expr, env: &mut Env) -> Result<Value, EvalError> {
        match expr {
            Expr::Lit(lit) => Ok(eval_literal(lit)),
            Expr::Var(name) => env
                .get(name.as_ref())
                .ok_or_else(|| EvalError::UndefinedVariable(name.clone())),
            Expr::Send { receiver, message } => {
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
                let selector: Arc<str> = parts.iter().map(|(k, _)| k.as_ref()).collect::<String>().into();
                let mut args = Vec::with_capacity(parts.len());
                for (_, expr) in parts {
                    args.push(self.eval_expr(expr, env)?);
                }
                self.dispatch(recv, selector, args, env)
            }
        }
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
}

fn eval_literal(lit: &Literal) -> Value {
    match lit {
        Literal::Int(n) => Value::Int(*n),
        Literal::Float(f) => Value::Float(*f),
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Nil => Value::Nil,
        Literal::Str(s) => Value::Str(s.clone()),
        Literal::Symbol(s) => Value::Symbol(s.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn eval(src: &str) -> Value {
        let prog = Parser::parse(src).expect("parse");
        let ev = Evaluator;
        let mut env = Env::new();
        ev.eval_program(&prog.stmts, &mut env).expect("eval")
    }

    fn eval_err(src: &str) -> EvalError {
        let prog = Parser::parse(src).expect("parse");
        let ev = Evaluator;
        let mut env = Env::new();
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
}
