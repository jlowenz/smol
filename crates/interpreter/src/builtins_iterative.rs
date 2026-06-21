use std::collections::HashMap;
use std::sync::Arc;

/// Cooperative yield interval: one yield every N completed message dispatches.
/// Matches the step-loop constant in eval_iterative so loop primitives and the
/// iterative evaluator yield at the same granularity.
use num_bigint::BigInt;
use num_traits::Zero;
use parking_lot::RwLock;

use crate::class::{Class, IndexedSlotKind};
use crate::entity::FrameCommitOutcome;
use crate::entity::{
    EntityId, FutureHandle, InstanceHandle, InstanceKind, InstanceState, send_to_entity,
    spawn_class_entity, spawn_entity_bare,
};
use crate::env::Env;
use crate::eval::{EvalError, Evaluator};
use crate::namespace::Namespace;
use crate::object::{IndexedStorage, Object};
use crate::shared_tx::{ChildTxInner, RootTxInner, TxContext, next_ctx_id};
use crate::value::{Block, Value};
use may::sync::Mutex;
use triple_store::exec_context::ExecContext;
use triple_store::fact::{Fact, StoredFact};
use triple_store::term::Term;

/// Entry point for all built-in method dispatch.
pub fn dispatch(
    ev: &Evaluator,
    recv: Value,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // `yourself` is universal — every value returns itself.
    if selector.as_ref() == "yourself" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(recv);
    }
    // `await` is universal — blocks until a Future resolves; no-op on non-futures.
    if selector.as_ref() == "await" {
        expect_arity(&selector, 0, args.len())?;
        return force_value(recv);
    }
    // `isNil` / `notNil` are universal.
    if selector.as_ref() == "isNil" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(Value::Bool(matches!(force_value(recv)?, Value::Nil)));
    }
    if selector.as_ref() == "notNil" {
        expect_arity(&selector, 0, args.len())?;
        return Ok(Value::Bool(!matches!(force_value(recv)?, Value::Nil)));
    }
    // `_primHash` is universal — handled here so all value kinds are covered.
    if selector.as_ref() == "_primHash" {
        expect_arity(&selector, 0, args.len())?;
        let v = force_value(recv)?;
        return Ok(Value::Int(hash_value(&v)));
    }
    // `_primClass` — maps any value to its kernel class entity handle.
    if selector.as_ref() == "_primClass" {
        expect_arity(&selector, 0, args.len())?;
        let recv = force_value(recv)?;
        return value_to_class_handle(ev, &recv)
            .map(Value::Instance)
            .ok_or_else(|| mnu("Object", &selector));
    }
    // `_primIsKindOf:` — true iff receiver's class is aClass or a subclass of it.
    if selector.as_ref() == "_primIsKindOf:" {
        expect_arity(&selector, 1, args.len())?;
        let recv = force_value(recv)?;
        let arg = force_value(args.into_iter().next().unwrap())?;
        let recv_class = match value_to_class_handle(ev, &recv) {
            Some(h) => h.0.class.clone(),
            None => return Ok(Value::Bool(false)),
        };
        let arg_class = match arg {
            Value::Instance(h) => h.0.class.clone(),
            _ => return Ok(Value::Bool(false)),
        };
        return Ok(Value::Bool(is_kind_of(&recv_class, &arg_class)));
    }
    // `hash` is universal for non-object types (primitive values don't route through
    // Object method lookup). For Value::Instance with Entity/ValueObject kind, the
    // type-specific dispatch runs instead to find any user-defined `hash` override.
    // Value::Future is excluded so the transparent-future arm below resolves the
    // future first; if it resolves to an Instance, hash is re-dispatched through
    // dispatch_entity so user-defined overrides are found.
    if selector.as_ref() == "hash" && !matches!(recv, Value::Instance(_) | Value::Future(_)) {
        expect_arity(&selector, 0, args.len())?;
        let v = force_value(recv)?;
        return Ok(Value::Int(hash_value(&v)));
    }
    // `->` is universal — any receiver can create an Association.
    // Implemented in Rust so primitive values (String, Int, …) work without an
    // Object-inherited method dispatch path.
    if selector.as_ref() == "->" {
        expect_arity(&selector, 1, args.len())?;
        let value = args.into_iter().next().unwrap();
        let assoc_class = ev
            .registry
            .kernel()
            .lookup_class("Association")
            .ok_or_else(|| {
                EvalError::TypeError("'->': Association not yet defined in kernel".into())
            })?;
        return crate::builtins::dispatch(
            ev,
            Value::Instance(assoc_class),
            Arc::from("key:value:"),
            vec![recv, value],
            env,
        );
    }
    // `then:` — chains a block after this value resolves.
    // Future: spawns an async continuation and returns a new Future.
    // Block: evaluates the receiver block synchronously (≡ `value`), then — if the
    //   result is a Future, chains the continuation asynchronously; otherwise runs it
    //   synchronously. This is the canonical `[loop] then: [result]` combinator: the
    //   receiver block runs first (queuing any actor messages), then the continuation
    //   is guaranteed to execute after it.
    // Any other value: the value is already resolved; the block is run synchronously.
    // The continuation block receives no arguments in any case.
    if selector.as_ref() == "then:" {
        expect_arity(&selector, 1, args.len())?;
        let blk_val = args.into_iter().next().unwrap();
        return dispatch_then(ev, recv, blk_val, &selector, env);
    }

    match recv {
        Value::Int(n) => dispatch_int(ev, n, &selector, args, env),
        Value::Float(f) => dispatch_float(ev, f, &selector, args, env),
        Value::Bool(_) => dispatch_primitive_fallback(ev, "Bool", &selector, args),
        Value::Nil => dispatch_nil(ev, &selector, args, env),
        Value::Str(s) => dispatch_str(ev, s, &selector, args, env),
        Value::Symbol(s) => dispatch_symbol(ev, s, &selector, args, env),
        Value::Block(blk) => dispatch_block(ev, blk, &selector, args, env),
        Value::LargeInt(n) => dispatch_largeint(ev, n, &selector, args, env),
        Value::Instance(handle) => match &handle.0.kind {
            InstanceKind::ValueObject(_) => dispatch_object_value(ev, handle, selector, args, env),
            InstanceKind::Entity(_) => dispatch_entity(ev, handle, selector, args, env),
            InstanceKind::ClassEntity(_) => dispatch_class(ev, handle, selector, args, env),
        },
        Value::Future(_) => unreachable!(
            "dispatch() called with unresolved Future; dispatch_to_stack forces futures before \
             calling dispatch(), and then: intercepts Futures before force_value"
        ),
        Value::Namespace(ns) => dispatch_namespace(ev, ns, selector, args, env),
        Value::Handle(_) => Err(EvalError::MessageNotUnderstood {
            receiver_kind: "Handle",
            selector,
        }),
    }
}

/// `then:` — chains a block after this value resolves. Extracted `#[inline(never)]`
/// from `dispatch` so its `may::go!` continuation closures (large debug frames)
/// don't inflate `dispatch`'s frame on every dispatch.
///
/// Future receiver: spawns an async continuation, returns a new Future.
/// Block receiver: runs it (≡ `value`); if the result is a Future, chains
///   asynchronously, else runs the continuation synchronously.
/// Any other value: already resolved; the continuation runs synchronously.
#[inline(never)]
fn dispatch_then(
    ev: &Evaluator,
    recv: Value,
    blk_val: Value,
    selector: &Arc<str>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    match recv {
        Value::Future(future) => {
            let blk = as_block(blk_val, selector)?;
            let result_future = crate::entity::FutureHandle::new();
            let result_reply = result_future.clone();
            let ctx = ev.fork_context();
            crate::entity::init_coroutine_stack();
            may::go!(move || {
                match future.wait() {
                    Err(e) => result_reply.resolve_err(e),
                    Ok(_) => {
                        let then_ev = ctx.into_evaluator();
                        let mut dummy_env = crate::env::Env::new();
                        let result = then_ev.eval_block(&blk, vec![], &mut dummy_env);
                        match result {
                            Ok(v) => match force_value(v) {
                                Ok(forced) => result_reply.resolve_ok(forced),
                                Err(e) => result_reply.resolve_err(e),
                            },
                            Err(EvalError::Return(v, _)) => result_reply.resolve_ok(v),
                            Err(e) => result_reply.resolve_err(e),
                        }
                    }
                }
            });
            Ok(Value::Future(result_future))
        }
        Value::Block(recv_blk) => {
            let cont_blk = as_block(blk_val, selector)?;
            let result = ev.eval_block(&recv_blk, vec![], env)?;
            match result {
                Value::Future(future) => {
                    let result_future = crate::entity::FutureHandle::new();
                    let result_reply = result_future.clone();
                    let ctx = ev.fork_context();
                    crate::entity::init_coroutine_stack();
                    may::go!(move || {
                        match future.wait() {
                            Err(e) => result_reply.resolve_err(e),
                            Ok(_) => {
                                let then_ev = ctx.into_evaluator();
                                let mut dummy_env = crate::env::Env::new();
                                let r = then_ev.eval_block(&cont_blk, vec![], &mut dummy_env);
                                match r {
                                    Ok(v) => match force_value(v) {
                                        Ok(forced) => result_reply.resolve_ok(forced),
                                        Err(e) => result_reply.resolve_err(e),
                                    },
                                    Err(EvalError::Return(v, _)) => result_reply.resolve_ok(v),
                                    Err(e) => result_reply.resolve_err(e),
                                }
                            }
                        }
                    });
                    Ok(Value::Future(result_future))
                }
                _ => ev.eval_block(&cont_blk, vec![], env),
            }
        }
        _ => {
            let blk = as_block(blk_val, selector)?;
            ev.eval_block(&blk, vec![], env)
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Result of a user-method lookup for the iterative evaluator's step loop.
///
/// When `Some`, the step loop should push a `CallFrame` instead of calling
/// `builtins::dispatch`. When `None`, the dispatch falls through to builtins.
pub(crate) enum UserMethodFound {
    /// Instance or entity self-send: invoke `method` with `handle` as receiver.
    Instance {
        handle: InstanceHandle,
        method: Arc<crate::class::Method>,
        def_class: Arc<crate::class::Class>,
    },
    /// Primitive receiver (e.g. nil): invoke `method` with `self_value` as receiver.
    Primitive {
        self_value: Value,
        method: Arc<crate::class::Method>,
        def_class: Arc<crate::class::Class>,
    },
}

/// Look up a user-defined method that the step loop should dispatch as a `CallFrame`.
///
/// Requires: `ev` is the current evaluator.
/// Ensures: returns `Some` only for synchronous user-method calls — ValueObject
///   instance methods, entity self-sends, and nil methods. Cross-entity sends and
///   class-entity dispatch return `None` (handled by `builtins::dispatch`).
pub(crate) fn lookup_user_method(
    ev: &Evaluator,
    recv: &Value,
    selector: &Arc<str>,
) -> Option<UserMethodFound> {
    match recv {
        Value::Instance(handle) => match &handle.0.kind {
            InstanceKind::ValueObject(_) => {
                let (method, def_class) = handle.0.class.lookup(selector)?;
                Some(UserMethodFound::Instance {
                    handle: handle.clone(),
                    method,
                    def_class,
                })
            }
            InstanceKind::Entity(d) => {
                // Only intercept self-sends; cross-entity sends stay async via builtins.
                if ev.current_entity_id.get() != Some(d.id) {
                    return None;
                }
                let (method, def_class) = handle.0.class.lookup(selector)?;
                Some(UserMethodFound::Instance {
                    handle: handle.clone(),
                    method,
                    def_class,
                })
            }
            InstanceKind::ClassEntity(_) => None, // class-side dispatch handled by builtins
        },
        Value::Nil => {
            let nil_handle = ev.registry.kernel().lookup_class("Nil")?;
            let (method, def_class) = nil_handle.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Nil,
                method,
                def_class,
            })
        }
        Value::Block(blk) => {
            let blk_cls = ev.registry.kernel().lookup_class("Block")?;
            let (method, def_class) = blk_cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Block(blk.clone()),
                method,
                def_class,
            })
        }
        Value::Int(n) => {
            let int_cls = ev.registry.kernel().lookup_class("Int")?;
            let (method, def_class) = int_cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Int(*n),
                method,
                def_class,
            })
        }
        Value::Bool(b) => {
            let cls = ev
                .registry
                .kernel()
                .lookup_class(if *b { "True" } else { "False" })?;
            let (method, def_class) = cls.0.class.lookup(selector)?;
            Some(UserMethodFound::Primitive {
                self_value: Value::Bool(*b),
                method,
                def_class,
            })
        }
        _ => None, // Float, Str, Symbol, LargeInt, Future, Namespace
    }
}

fn mnu(recv_kind: &'static str, selector: &Arc<str>) -> EvalError {
    EvalError::MessageNotUnderstood {
        receiver_kind: recv_kind,
        selector: selector.clone(),
    }
}

pub(crate) fn expect_arity(
    _selector: &Arc<str>,
    expected: usize,
    got: usize,
) -> Result<(), EvalError> {
    if got != expected {
        Err(EvalError::ArityMismatch { expected, got })
    } else {
        Ok(())
    }
}

/// Resolves a `Value::Future` to its concrete value at the point it is needed.
///
/// Requires: called from a may coroutine so that `FutureHandle::wait()` parks
///   cooperatively rather than blocking the OS thread.
/// Ensures: returns the first non-Future value reachable by following the chain,
///   or propagates any error from the future.
pub(crate) fn force_value(v: Value) -> Result<Value, EvalError> {
    match v {
        Value::Future(f) => force_value(f.wait()?),
        v => Ok(v),
    }
}

pub(crate) fn as_int(v: Value, selector: &Arc<str>) -> Result<i64, EvalError> {
    match force_value(v)? {
        Value::Int(n) => Ok(n),
        _ => Err(EvalError::TypeError(format!(
            "{selector}: argument must be Int"
        ))),
    }
}

pub(crate) fn as_block(v: Value, selector: &Arc<str>) -> Result<Arc<Block>, EvalError> {
    match force_value(v)? {
        Value::Block(b) => Ok(b),
        _ => Err(EvalError::TypeError(format!(
            "{selector}: argument must be a Block"
        ))),
    }
}

fn bigint_from_value(v: Value, sel: &Arc<str>) -> Result<BigInt, EvalError> {
    match force_value(v)? {
        Value::Int(n) => Ok(BigInt::from(n)),
        Value::LargeInt(b) => Ok((*b).clone()),
        _ => Err(EvalError::TypeError(format!(
            "{sel}: argument must be Int or LargeInt"
        ))),
    }
}

fn normalize_bigint(b: BigInt) -> Value {
    use num_traits::ToPrimitive;
    if let Some(n) = b.to_i64() {
        Value::Int(n)
    } else {
        Value::LargeInt(Arc::new(b))
    }
}

/// Smalltalk `//`: floor division (truncates toward negative infinity).
fn bigint_floor_div(n: BigInt, d: &BigInt) -> BigInt {
    use num_traits::Signed;
    let q = &n / d;
    let r = n % d;
    if !r.is_zero() && r.is_negative() != d.is_negative() {
        q - 1
    } else {
        q
    }
}

/// Smalltalk `\\`: modulo with the sign of the divisor.
fn bigint_floor_rem(n: BigInt, d: &BigInt) -> BigInt {
    use num_traits::Signed;
    let r = &n % d;
    if !r.is_zero() && r.is_negative() != d.is_negative() {
        r + d
    } else {
        r
    }
}

// ── Hashing helpers ───────────────────────────────────────────────────────────

/// MurmurHash3 bit-mixer — avalanches bits to reduce clustering from
/// pointer-aligned addresses.
pub(crate) fn fmix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.wrapping_mul(0xff51afd7ed558ccd);
    k ^= k >> 33;
    k = k.wrapping_mul(0xc4ceb9fe1a85ec53);
    k ^= k >> 33;
    k
}

/// FNV-1a over the UTF-8 bytes of a string.
pub(crate) fn fnv1a_hash(s: &str) -> i64 {
    let mut h: u64 = 14695981039346656037;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h as i64
}

/// Returns a stable hash for any `Value`. Used by `_primHash`.
///
/// - Strings / Symbols: FNV-1a over bytes (content-based, value-equal strings hash equal).
/// - Integers: the value itself (or low 64 bits for LargeInt).
/// - Float: bit-pattern of the `f64`.
/// - Bool / Nil: fixed constants.
/// - Object / Class / Block / Namespace: `fmix64` of the heap pointer (identity hash).
pub(crate) fn hash_value(v: &Value) -> i64 {
    match v {
        Value::Int(n) => *n,
        Value::Float(f) => f.to_bits() as i64,
        Value::Str(s) => fnv1a_hash(s),
        Value::Symbol(s) => fnv1a_hash(s),
        Value::Bool(b) => {
            if *b {
                1
            } else {
                0
            }
        }
        Value::Nil => 0,
        Value::LargeInt(n) => {
            use num_bigint::Sign;
            let (sign, bytes) = n.to_bytes_le();
            let mut h: u64 = 0;
            for (i, &b) in bytes.iter().take(8).enumerate() {
                h |= (b as u64) << (i * 8);
            }
            if sign == Sign::Minus {
                h = h.wrapping_neg();
            }
            h as i64
        }
        Value::Instance(h) => fmix64(Arc::as_ptr(&h.0) as usize as u64) as i64,
        // Futures are resolved by force_value before hash_value is called.
        Value::Future(_) => unreachable!("hash_value called on unresolved Future"),
        Value::Block(b) => fmix64(Arc::as_ptr(b) as usize as u64) as i64,
        Value::Namespace(ns) => fnv1a_hash(&ns.name),
        Value::Handle(h) => fmix64(Arc::as_ptr(h) as *const () as usize as u64) as i64,
    }
}

/// Construct a bare `Error` instance without calling kernel methods (`initialize`, `pvtInit`).
///
/// Requires: None — if the `Error` class is unavailable (e.g. pre-bootstrap), returns
///   `Value::Str(message)` as a degenerate fallback.
/// Ensures: returns a `Value::Instance` of class `Error` with `message` ivar set;
///   `restarts` ivar is nil (no restarts — internal signals are not resumable).
pub(crate) fn make_error_value(
    registry: &crate::class_registry::ClassRegistry,
    message: &str,
) -> Value {
    let Some(handle) = registry.kernel().lookup_class("Error") else {
        return Value::Str(Arc::from(message));
    };
    let error_class = handle.0.class.clone();
    let obj = Arc::new(crate::object::Object::new(error_class.clone()));
    obj.set_slot(Arc::from("message"), Value::Str(Arc::from(message)));
    Value::Instance(InstanceHandle(Arc::new(InstanceState {
        class: error_class,
        kind: InstanceKind::ValueObject(obj),
    })))
}

/// Fallback dispatch for primitive value types (Int, Float, Bool, Nil, Str,
/// Symbol, Block, LargeInt). Called from the `_` arm of each type-specific
/// dispatch function after type-specific cases are exhausted.
///
/// Handles selectors that are meaningful on every value but are not universal
/// enough to bypass user-defined method lookup (unlike the pre-checks in
/// `dispatch()`). Object/ValueObject instances never reach this function —
/// they have their own fallback in `dispatch_object_primitive`.
fn dispatch_primitive_fallback(
    ev: &Evaluator,
    kind: &'static str,
    sel: &Arc<str>,
    mut args: Vec<Value>,
) -> Result<Value, EvalError> {
    // NOTE: primitive types (Int, Float, etc.) don't carry an Env — handler_chain
    // checking is skipped here. Use dispatch_object_primitive for Condition instances.
    match sel.as_ref() {
        "error:" => {
            expect_arity(sel, 1, args.len())?;
            let msg = match args.remove(0) {
                Value::Str(s) => s,
                other => Arc::from(format!("{other}").as_str()),
            };
            Err(EvalError::Signal(make_error_value(&ev.registry, &msg)))
        }
        _ => Err(mnu(kind, sel)),
    }
}

// ── Int ───────────────────────────────────────────────────────────────────────

fn dispatch_int(
    ev: &Evaluator,
    n: i64,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // Note: user-defined Int methods are now routed via lookup_user_method in the
    // iterative evaluator. The old (recursive) evaluator reaches builtins directly.
    match sel.as_ref() {
        "+" => {
            expect_arity(sel, 1, args.len())?;
            let arg = force_value(args.remove(0))?;
            match arg {
                Value::Int(m) => match n.checked_add(m) {
                    Some(r) => Ok(Value::Int(r)),
                    None => Ok(normalize_bigint(BigInt::from(n) + BigInt::from(m))),
                },
                Value::LargeInt(m) => Ok(normalize_bigint(BigInt::from(n) + (*m).clone())),
                _ => Err(EvalError::TypeError(
                    "+: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "-" => {
            expect_arity(sel, 1, args.len())?;
            let arg = force_value(args.remove(0))?;
            match arg {
                Value::Int(m) => match n.checked_sub(m) {
                    Some(r) => Ok(Value::Int(r)),
                    None => Ok(normalize_bigint(BigInt::from(n) - BigInt::from(m))),
                },
                Value::LargeInt(m) => Ok(normalize_bigint(BigInt::from(n) - (*m).clone())),
                _ => Err(EvalError::TypeError(
                    "-: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "*" => {
            expect_arity(sel, 1, args.len())?;
            let arg = force_value(args.remove(0))?;
            match arg {
                Value::Int(m) => match n.checked_mul(m) {
                    Some(r) => Ok(Value::Int(r)),
                    None => Ok(normalize_bigint(BigInt::from(n) * BigInt::from(m))),
                },
                Value::LargeInt(m) => Ok(normalize_bigint(BigInt::from(n) * (*m).clone())),
                _ => Err(EvalError::TypeError(
                    "*: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "/" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Int(d) => {
                    if d == 0 {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    Ok(Value::Int(n / d))
                }
                Value::LargeInt(d) => {
                    if d.is_zero() {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    Ok(normalize_bigint(BigInt::from(n) / (*d).clone()))
                }
                _ => Err(EvalError::TypeError(
                    "/: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "//" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Int(d) => {
                    if d == 0 {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    let r = n % d;
                    Ok(Value::Int(if r != 0 && (r < 0) != (d < 0) {
                        n / d - 1
                    } else {
                        n / d
                    }))
                }
                Value::LargeInt(d) => {
                    if d.is_zero() {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    Ok(normalize_bigint(bigint_floor_div(BigInt::from(n), &d)))
                }
                _ => Err(EvalError::TypeError(
                    "//: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "\\\\" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Int(d) => {
                    if d == 0 {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    let r = n % d;
                    Ok(Value::Int(if r != 0 && (r < 0) != (d < 0) {
                        r + d
                    } else {
                        r
                    }))
                }
                Value::LargeInt(d) => {
                    if d.is_zero() {
                        return Err(EvalError::TypeError("division by zero".into()));
                    }
                    Ok(normalize_bigint(bigint_floor_rem(BigInt::from(n), &d)))
                }
                _ => Err(EvalError::TypeError(
                    "\\\\: argument must be Int or LargeInt".into(),
                )),
            }
        }
        "<" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(n < as_int(args.remove(0), sel)?))
        }
        ">" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(n > as_int(args.remove(0), sel)?))
        }
        "<=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(n <= as_int(args.remove(0), sel)?))
        }
        ">=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(n >= as_int(args.remove(0), sel)?))
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Int(n) == force_value(args.remove(0))?))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Int(n) != force_value(args.remove(0))?))
        }
        "negated" => {
            expect_arity(sel, 0, args.len())?;
            match n.checked_neg() {
                Some(r) => Ok(Value::Int(r)),
                None => Ok(normalize_bigint(-BigInt::from(n))),
            }
        }
        "abs" => {
            expect_arity(sel, 0, args.len())?;
            match n.checked_abs() {
                Some(r) => Ok(Value::Int(r)),
                None => {
                    // i64::MIN has no positive i64 representation; promote to BigInt.
                    use num_traits::Signed;
                    Ok(normalize_bigint(BigInt::from(n).abs()))
                }
            }
        }
        "max:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Int(n.max(as_int(args.remove(0), sel)?)))
        }
        "min:" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Int(n.min(as_int(args.remove(0), sel)?)))
        }
        "asString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(n.to_string())))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            let s = n.to_string();
            println!("{:?}", s);
            Ok(Value::Str(Arc::from(s)))
        }
        "asFloat" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Float(n as f64))
        }
        "to:do:" => {
            expect_arity(sel, 2, args.len())?;
            let stop = as_int(args.remove(0), sel)?;
            let blk = as_block(args.remove(0), sel)?;
            let mut i = n;
            while i <= stop {
                ev.eval_block(&blk, vec![Value::Int(i)], env)?;
                i += 1;
            }
            Ok(Value::Nil)
        }
        "to:by:do:" => {
            expect_arity(sel, 3, args.len())?;
            let stop = as_int(args.remove(0), sel)?;
            let step = as_int(args.remove(0), sel)?;
            let blk = as_block(args.remove(0), sel)?;
            if step == 0 {
                return Err(EvalError::TypeError(
                    "to:by:do: step must be non-zero".into(),
                ));
            }
            let mut i = n;
            while if step > 0 { i <= stop } else { i >= stop } {
                ev.eval_block(&blk, vec![Value::Int(i)], env)?;
                i += step;
            }
            Ok(Value::Nil)
        }
        _ => dispatch_primitive_fallback(ev, "Int", sel, args),
    }
}

// ── LargeInt ──────────────────────────────────────────────────────────────────

fn dispatch_largeint(
    ev: &Evaluator,
    n: Arc<BigInt>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    use num_traits::ToPrimitive;
    match sel.as_ref() {
        "+" | "-" | "*" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            let result = match sel.as_ref() {
                "+" => (*n).clone() + rhs,
                "-" => (*n).clone() - rhs,
                "*" => (*n).clone() * rhs,
                _ => unreachable!(),
            };
            Ok(normalize_bigint(result))
        }
        "/" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            if rhs.is_zero() {
                return Err(EvalError::TypeError("division by zero".into()));
            }
            Ok(normalize_bigint((*n).clone() / rhs))
        }
        "//" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            if rhs.is_zero() {
                return Err(EvalError::TypeError("division by zero".into()));
            }
            Ok(normalize_bigint(bigint_floor_div((*n).clone(), &rhs)))
        }
        "\\\\" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            if rhs.is_zero() {
                return Err(EvalError::TypeError("division by zero".into()));
            }
            Ok(normalize_bigint(bigint_floor_rem((*n).clone(), &rhs)))
        }
        "<" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            Ok(Value::Bool(*n < rhs))
        }
        ">" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            Ok(Value::Bool(*n > rhs))
        }
        "<=" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            Ok(Value::Bool(*n <= rhs))
        }
        ">=" => {
            expect_arity(sel, 1, args.len())?;
            let rhs = bigint_from_value(args.remove(0), sel)?;
            Ok(Value::Bool(*n >= rhs))
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(
                Value::LargeInt(n) == force_value(args.remove(0))?,
            ))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(
                Value::LargeInt(n) != force_value(args.remove(0))?,
            ))
        }
        "negated" => {
            expect_arity(sel, 0, args.len())?;
            Ok(normalize_bigint(-(*n).clone()))
        }
        "abs" => {
            expect_arity(sel, 0, args.len())?;
            use num_traits::Signed;
            Ok(normalize_bigint(n.abs()))
        }
        "asString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(n.to_string())))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(n.to_string().as_str())))
        }
        "asFloat" => {
            expect_arity(sel, 0, args.len())?;
            let f = n.to_f64().unwrap_or(f64::INFINITY);
            Ok(Value::Float(f))
        }
        _ => dispatch_primitive_fallback(ev, "LargeInt", sel, args),
    }
}

// ── Float ────────────────────────────────────────────────────────────────────

fn as_float_coerce(v: Value, sel: &Arc<str>) -> Result<f64, EvalError> {
    match force_value(v)? {
        Value::Float(f) => Ok(f),
        Value::Int(n) => Ok(n as f64),
        _ => Err(EvalError::TypeError(format!(
            "{sel}: argument must be Float or Int"
        ))),
    }
}

fn dispatch_float(
    ev: &Evaluator,
    f: f64,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "+" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Float(f + as_float_coerce(args.remove(0), sel)?))
        }
        "-" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Float(f - as_float_coerce(args.remove(0), sel)?))
        }
        "*" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Float(f * as_float_coerce(args.remove(0), sel)?))
        }
        "/" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Float(f / as_float_coerce(args.remove(0), sel)?))
        }
        "<" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(f < as_float_coerce(args.remove(0), sel)?))
        }
        ">" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(f > as_float_coerce(args.remove(0), sel)?))
        }
        "<=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(f <= as_float_coerce(args.remove(0), sel)?))
        }
        ">=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(f >= as_float_coerce(args.remove(0), sel)?))
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Float(f) == force_value(args.remove(0))?))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Float(f) != force_value(args.remove(0))?))
        }
        "negated" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Float(-f))
        }
        "abs" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Float(f.abs()))
        }
        "truncated" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f.trunc() as i64))
        }
        "rounded" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f.round() as i64))
        }
        "ceiling" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f.ceil() as i64))
        }
        "floor" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f.floor() as i64))
        }
        "asInt" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(f as i64))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(format!("{f:?}").as_str())))
        }
        _ => dispatch_primitive_fallback(ev, "Float", sel, args),
    }
}

// ── Nil ───────────────────────────────────────────────────────────────────────

fn dispatch_nil(
    ev: &Evaluator,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    // User-defined Nil methods (e.g. isEmpty, inject:into:) are intercepted by
    // lookup_user_method in dispatch_to_stack before dispatch() is called.
    match sel.as_ref() {
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(matches!(
                force_value(args.remove(0))?,
                Value::Nil
            )))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(!matches!(
                force_value(args.remove(0))?,
                Value::Nil
            )))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from("nil")))
        }
        _ => dispatch_primitive_fallback(ev, "Nil", sel, args),
    }
}

// ── String ────────────────────────────────────────────────────────────────────

fn dispatch_str(
    ev: &Evaluator,
    s: Arc<str>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "size" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Int(s.chars().count() as i64))
        }
        "at:" => {
            expect_arity(sel, 1, args.len())?;
            let idx = as_int(args.remove(0), sel)? as usize;
            // 1-based index (Smalltalk convention).
            s.chars()
                .nth(idx.wrapping_sub(1))
                .map(|c| Value::Str(Arc::from(c.to_string().as_str())))
                .ok_or_else(|| EvalError::TypeError(format!("{sel}: index {idx} out of bounds")))
        }
        "," => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Str(other) => Ok(Value::Str(Arc::from(format!("{s}{other}").as_str()))),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: argument must be String"
                ))),
            }
        }
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Str(s) == force_value(args.remove(0))?))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(Value::Str(s) != force_value(args.remove(0))?))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            let s: Arc<str> = Arc::from(format!("'{s}'").as_str());
            println!("{:?}", s);
            Ok(Value::Str(s))
        }
        "asSymbol" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Symbol(s))
        }
        "reversed" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(
                s.chars().rev().collect::<String>().as_str(),
            )))
        }
        "copyFrom:to:" => {
            expect_arity(sel, 2, args.len())?;
            let from = (as_int(args.remove(0), sel)? as usize).saturating_sub(1);
            let to = as_int(args.remove(0), sel)? as usize;
            let chars: Vec<char> = s.chars().collect();
            let slice: String = chars.get(from..to).unwrap_or(&[]).iter().collect();
            Ok(Value::Str(Arc::from(slice.as_str())))
        }
        "includesSubString:" => {
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Str(sub) => Ok(Value::Bool(s.contains(sub.as_ref()))),
                _ => Err(EvalError::TypeError(format!(
                    "{sel}: argument must be String"
                ))),
            }
        }
        _ => dispatch_primitive_fallback(ev, "String", sel, args),
    }
}

// ── Symbol ────────────────────────────────────────────────────────────────────

fn dispatch_symbol(
    ev: &Evaluator,
    s: Arc<str>,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    match sel.as_ref() {
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(
                Value::Symbol(s) == force_value(args.remove(0))?,
            ))
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Ok(Value::Bool(
                Value::Symbol(s) != force_value(args.remove(0))?,
            ))
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(Arc::from(format!("#{s}").as_str())))
        }
        "asString" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Str(s))
        }
        "asSymbol" => {
            expect_arity(sel, 0, args.len())?;
            Ok(Value::Symbol(s))
        }
        _ => dispatch_primitive_fallback(ev, "Symbol", sel, args),
    }
}

// ── Block ─────────────────────────────────────────────────────────────────────

fn dispatch_block(
    ev: &Evaluator,
    blk: Arc<Block>,
    sel: &Arc<str>,
    args: Vec<Value>,
    _env: &mut Env,
) -> Result<Value, EvalError> {
    // The iterative evaluator intercepts value/value:/whileTrue:/whileFalse:/etc. in
    // dispatch_to_stack (via push_block_frame or lookup_user_method + TCO) before
    // dispatch() is called, so only unknown selectors reach here.
    drop(blk);
    dispatch_primitive_fallback(ev, "Block", sel, args)
}

// ── Object (user-defined instances) ──────────────────────────────────────────

fn dispatch_object_value(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // User-defined methods are intercepted by lookup_user_method in dispatch_to_stack
    // before dispatch() is called, so class.lookup() here would always return None.
    dispatch_object_primitive(ev, handle, &selector, args, env)
}

/// Dispatch a message to an actor entity. Checks for sync self-send
/// (target == CURRENT_ENTITY); otherwise enqueues the message and returns a Future.
///
/// Requires: caller may be any thread including the entity's own thread.
/// Ensures: self-send is dispatched synchronously (no deadlock); cross-entity
///   send returns `Value::Future(FutureHandle)` immediately.
fn dispatch_entity(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let entity_id = match &handle.0.kind {
        InstanceKind::Entity(d) => d.id,
        _ => unreachable!("dispatch_entity called on non-Entity instance"),
    };

    // Self-send: dispatch synchronously on the current thread to avoid deadlock.
    // User-defined methods for self-sends are intercepted by lookup_user_method
    // in dispatch_to_stack before dispatch() is called, so class.lookup() would
    // always return None here; only builtins remain.
    let current = ev.current_entity_id.get();
    if current == Some(entity_id) {
        return dispatch_object_primitive(ev, handle, &selector, args, env);
    }

    // Cross-entity or non-entity-context send: enqueue and return Future.
    // Snapshot the caller's handler_chain so the receiving entity can run inherited
    // HC handlers if it signals a condition.
    let chain_snapshot = {
        let chain = ev.handler_chain.borrow();
        if chain.is_empty() {
            vec![]
        } else {
            chain.clone()
        }
    };
    let tx_tag = ev.current_tx_ctx.borrow().clone();
    let future = match &handle.0.kind {
        InstanceKind::Entity(d) => send_to_entity(d, selector, args, chain_snapshot, tx_tag),
        _ => unreachable!(),
    };
    Ok(Value::Future(future))
}

/// Returns the canonical `InstanceHandle` for the class of `obj`.
///
/// Requires: `obj.class.class_object` is set (always true after class registration).
fn class_handle_of(obj: &Arc<Object>) -> Result<InstanceHandle, EvalError> {
    obj.class
        .class_object
        .get()
        .and_then(|w| w.upgrade())
        .map(InstanceHandle)
        .ok_or_else(|| {
            EvalError::TypeError(format!("class {} has no class object", obj.class.name))
        })
}

/// Commit a shared-transaction context buffer to the store, mapping failures to
/// a signal. `FactNotFound` (nil retract of a never-set ivar) is treated as
/// success, matching the per-message commit policy.
fn commit_shared_buffer(
    ev: &Evaluator,
    tx: triple_store::store::OwnedStoreTransaction,
) -> Result<(), EvalError> {
    match tx.commit() {
        Ok(_) | Err(triple_store::store::StoreError::FactNotFound) => Ok(()),
        Err(e) => Err(EvalError::Signal(make_error_value(
            &ev.registry,
            &format!("transaction commit failed: {e:?}"),
        ))),
    }
}

/// Extract the `TxContext` carried by a transaction handle value.
///
/// Requires: `v` is the `Value::Handle` produced by `_primBeginSharedTx`.
fn tx_ctx_from_handle(v: Value) -> Result<Arc<TxContext>, EvalError> {
    match v {
        Value::Handle(h) => h
            .downcast::<TxContext>()
            .map_err(|_| EvalError::TypeError("expected a transaction handle".into())),
        other => Err(EvalError::TypeError(format!(
            "expected a transaction handle, got {}",
            other.kind()
        ))),
    }
}

/// Shared receiver context for the `dispatch_object_primitive` chunk handlers.
///
/// `dispatch_object_primitive`'s `match sel` was one ~900-line function whose
/// debug frame summed *every* arm's locals — and that frame is live at the
/// signal/eval_block recursion point, dominating the per-nesting-level Rust
/// stack (C31.5: ~52 KB of the ~60 KB/level). Splitting the arms into
/// `#[inline(never)]` chunk handlers means only the matched chunk's frame is
/// live at any recursion, not all arms'.
struct ObjPrim<'a> {
    ev: &'a Evaluator,
    handle: &'a InstanceHandle,
    obj: &'a Arc<Object>,
    entity_id: Option<EntityId>,
    self_val: &'a Value,
}

pub(crate) fn dispatch_object_primitive(
    ev: &Evaluator,
    handle: InstanceHandle,
    sel: &Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let entity_id = match &handle.0.kind {
        InstanceKind::Entity(d) => Some(d.id),
        _ => None,
    };
    let obj = match &handle.0.kind {
        InstanceKind::ValueObject(o) => o.clone(),
        InstanceKind::Entity(d) => d.data.clone(),
        InstanceKind::ClassEntity(_) => {
            return Err(EvalError::MessageNotUnderstood {
                receiver_kind: "Class",
                selector: sel.clone(),
            });
        }
    };
    let self_val = Value::Instance(handle.clone());
    let cx = ObjPrim {
        ev,
        handle: &handle,
        obj: &obj,
        entity_id,
        self_val: &self_val,
    };

    // Each chunk handles a disjoint set of selectors and returns Ok(None) for
    // the rest; chunks run sequentially so only the matched chunk's frame is
    // ever live at a recursion point inside it (e.g. signal_condition).
    if let Some(v) = obj_prim_core(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_indexed(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_conditions(&cx, sel, &mut args, env)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_history(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_timers(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_tx(&cx, sel, &mut args)? {
        return Ok(v);
    }
    if let Some(v) = obj_prim_futures(&cx, sel, &mut args)? {
        return Ok(v);
    }
    Err(EvalError::MessageNotUnderstood {
        receiver_kind: "Object",
        selector: sel.clone(),
    })
}

/// `=`, `~=`, `printString`, `respondsTo:`.
#[inline(never)]
fn obj_prim_core(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    Ok(Some(match sel.as_ref() {
        "=" => {
            expect_arity(sel, 1, args.len())?;
            Value::Bool(*cx.self_val == force_value(args.remove(0))?)
        }
        "~=" => {
            expect_arity(sel, 1, args.len())?;
            Value::Bool(*cx.self_val != force_value(args.remove(0))?)
        }
        "printString" => {
            expect_arity(sel, 0, args.len())?;
            let s: Arc<str> = Arc::from(format!("a {}", cx.obj.class.name).as_str());
            println!("{:?}", s);
            Value::Str(s)
        }
        "respondsTo:" => {
            expect_arity(sel, 1, args.len())?;
            match args.remove(0) {
                Value::Symbol(s) => Value::Bool(cx.obj.class.lookup(&s).is_some()),
                _ => Value::Bool(false),
            }
        }
        _ => return Ok(None),
    }))
}

/// Indexed-slot primitives: `_primAt:`, `_primAt:put:`, `_primSize`, `_primAsString`.
#[inline(never)]
fn obj_prim_indexed(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    use crate::object::IndexedStorage;
    let obj = cx.obj;
    let entity_id = cx.entity_id;
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        "_primAt:" => {
            expect_arity(sel, 1, args.len())?;
            let n = as_int(args.remove(0), sel)?;
            let indexed = obj.indexed.as_ref().ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: receiver has no indexed storage"))
            })?;
            match indexed {
                IndexedStorage::Pointer(slots) => {
                    let guard = slots.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    guard[n as usize].clone()
                }
                IndexedStorage::Byte(bytes) => {
                    let guard = bytes.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    Value::Int(guard[n as usize] as i64)
                }
                IndexedStorage::Float64(floats) => {
                    let guard = floats.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    Value::Float(guard[n as usize])
                }
            }
        }
        "_primAt:put:" => {
            expect_arity(sel, 2, args.len())?;
            let n = as_int(args.remove(0), sel)?;
            let v = args.remove(0);
            let indexed = obj.indexed.as_ref().ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: receiver has no indexed storage"))
            })?;
            match indexed {
                IndexedStorage::Pointer(slots) => {
                    let mut guard = slots.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    guard[n as usize] = v.clone();
                    drop(guard);
                    if let Some(eid) = entity_id {
                        crate::store_bridge::buffer_indexed_slot(ev, eid, n as usize, &v);
                    }
                    v
                }
                IndexedStorage::Byte(bytes) => {
                    let b = match &v {
                        Value::Int(i) if *i >= 0 && *i <= 255 => *i as u8,
                        Value::Int(i) => {
                            return Err(EvalError::TypeError(format!(
                                "{sel}: byte value {i} out of range 0..=255"
                            )));
                        }
                        _ => {
                            return Err(EvalError::TypeError(format!(
                                "{sel}: Byte storage requires Int argument in 0..=255"
                            )));
                        }
                    };
                    let mut guard = bytes.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    guard[n as usize] = b;
                    drop(guard);
                    if let Some(eid) = entity_id {
                        crate::store_bridge::buffer_indexed_slot(
                            ev,
                            eid,
                            n as usize,
                            &Value::Int(b as i64),
                        );
                    }
                    v
                }
                IndexedStorage::Float64(floats) => {
                    let f = match &v {
                        Value::Float(f) => *f,
                        Value::Int(i) => *i as f64,
                        _ => {
                            return Err(EvalError::TypeError(format!(
                                "{sel}: Float64 storage requires Float or Int argument"
                            )));
                        }
                    };
                    let mut guard = floats.lock();
                    if n < 0 || n as usize >= guard.len() {
                        return Err(EvalError::TypeError(format!(
                            "{sel}: index {n} out of bounds (size {})",
                            guard.len()
                        )));
                    }
                    guard[n as usize] = f;
                    drop(guard);
                    if let Some(eid) = entity_id {
                        crate::store_bridge::buffer_indexed_slot(
                            ev,
                            eid,
                            n as usize,
                            &Value::Float(f),
                        );
                    }
                    v
                }
            }
        }
        "_primSize" => {
            expect_arity(sel, 0, args.len())?;
            let indexed = obj.indexed.as_ref().ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: receiver has no indexed storage"))
            })?;
            let len = match indexed {
                IndexedStorage::Pointer(slots) => slots.lock().len(),
                IndexedStorage::Byte(bytes) => bytes.lock().len(),
                IndexedStorage::Float64(floats) => floats.lock().len(),
            };
            Value::Int(len as i64)
        }
        "_primAsString" => {
            expect_arity(sel, 0, args.len())?;
            match obj.indexed.as_ref().ok_or_else(|| {
                EvalError::TypeError(format!("{sel}: receiver has no indexed storage"))
            })? {
                IndexedStorage::Byte(bytes) => {
                    let s = String::from_utf8_lossy(&bytes.lock()).into_owned();
                    Value::Str(Arc::from(s.as_str()))
                }
                _ => {
                    return Err(EvalError::TypeError(format!(
                        "{sel}: requires Byte indexed storage"
                    )));
                }
            }
        }
        _ => return Ok(None),
    }))
}

/// Condition system: `error:`, `_primSignalCondition:`, `_primResume:`,
/// `_primReturn:`, `_primRetry`, and the `_primHCRun:` fallback. This chunk holds
/// the signal path; keeping it small bounds the Rust stack at the handler eval.
#[inline(never)]
fn obj_prim_conditions(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
    env: &mut Env,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        "error:" => {
            expect_arity(sel, 1, args.len())?;
            let msg = match force_value(args.remove(0))? {
                Value::Str(s) => s,
                other => Arc::from(format!("error: argument was {}", other.kind()).as_str()),
            };
            signal_condition(ev, make_error_value(&ev.registry, &msg), env)?
        }
        "_primSignalCondition:" => {
            expect_arity(sel, 1, args.len())?;
            let condition = force_value(args.remove(0))?;
            if !matches!(&condition, Value::Instance(_)) {
                return Err(EvalError::TypeError(
                    "_primSignalCondition: requires a Condition instance".into(),
                ));
            }
            signal_condition(ev, condition, env)?
        }
        "_primResume:" => {
            expect_arity(sel, 1, args.len())?;
            return Err(EvalError::Resume(args.remove(0)));
        }
        "_primReturn:" => {
            expect_arity(sel, 1, args.len())?;
            return Err(EvalError::ReturnFromBlock(args.remove(0)));
        }
        "_primRetry" => {
            expect_arity(sel, 0, args.len())?;
            return Err(EvalError::Retry);
        }
        "_primHCRun:" => {
            // Fallback path only. The iterative evaluator intercepts `_primHCRun:`
            // in `dispatch_to_stack` and runs the body on the heap `Cont` stack
            // (no recursive `step_loop`); see `eval_iterative::dispatch_to_stack`.
            // This arm preserves the recursive behavior for any direct
            // `builtins::dispatch` caller.
            expect_arity(sel, 1, args.len())?;
            let body = as_block(args.remove(0), sel)?;
            let handler_pairs = extract_handler_pairs(cx.handle)?;

            // Push handler pairs onto handler_chain.
            let chain_start_len = ev.handler_chain.borrow().len();
            {
                let mut chain = ev.handler_chain.borrow_mut();
                for (cls, blk) in handler_pairs.iter() {
                    chain.push((cls.clone(), blk.clone()));
                }
            }

            // Execute body in a loop (for Retry). Always truncate chain on exit.
            let result = loop {
                match ev.eval_block(&body, vec![], env) {
                    Ok(v) => break Ok(v),
                    Err(EvalError::ReturnFromBlock(v)) => break Ok(v),
                    Err(EvalError::Retry) => continue,
                    Err(EvalError::Return(v, d)) => break Err(EvalError::Return(v, d)),
                    Err(other) => break Err(other),
                }
            };
            ev.handler_chain.borrow_mut().truncate(chain_start_len);
            result?
        }
        _ => return Ok(None),
    }))
}

/// Temporal queries: `historyOf:`, `historyOf:from:to:`.
#[inline(never)]
fn obj_prim_history(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    let entity_id = cx.entity_id;
    Ok(Some(match sel.as_ref() {
        "historyOf:" => {
            expect_arity(sel, 1, args.len())?;
            let eid = entity_id.ok_or_else(|| {
                EvalError::TypeError("historyOf: requires an entity receiver".into())
            })?;
            let pred_name = match force_value(args.remove(0))? {
                Value::Symbol(s) | Value::Str(s) => s,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "historyOf: argument must be a Symbol, got {}",
                        other.kind()
                    )));
                }
            };
            let pred = ev.registry.store.lookup(&pred_name).ok_or_else(|| {
                EvalError::TypeError(format!("historyOf: unknown predicate '{pred_name}'"))
            })?;
            let ctx = ExecContext::Entity(eid);
            let facts = ev.store.history(&ctx, eid, pred, None).map_err(|e| {
                EvalError::Signal(make_error_value(
                    &ev.registry,
                    &format!("historyOf: store error: {e:?}"),
                ))
            })?;
            history_facts_to_array(ev, &facts)?
        }
        "historyOf:from:to:" => {
            expect_arity(sel, 3, args.len())?;
            let eid = entity_id.ok_or_else(|| {
                EvalError::TypeError("historyOf:from:to: requires an entity receiver".into())
            })?;
            let pred_name = match force_value(args.remove(0))? {
                Value::Symbol(s) | Value::Str(s) => s,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "historyOf:from:to: first argument must be a Symbol, got {}",
                        other.kind()
                    )));
                }
            };
            let from_ms = as_int(args.remove(0), sel)?;
            let to_ms = as_int(args.remove(0), sel)?;
            let pred = ev.registry.store.lookup(&pred_name).ok_or_else(|| {
                EvalError::TypeError(format!(
                    "historyOf:from:to: unknown predicate '{pred_name}'"
                ))
            })?;
            let ctx = ExecContext::Entity(eid);
            let all_facts = ev.store.history(&ctx, eid, pred, None).map_err(|e| {
                EvalError::Signal(make_error_value(
                    &ev.registry,
                    &format!("historyOf:from:to: store error: {e:?}"),
                ))
            })?;
            let facts: Vec<StoredFact> = all_facts
                .into_iter()
                .filter(|f| {
                    let start_ms = (f.valid.start.0 / 1_000_000) as i64;
                    let end_ms = f
                        .valid
                        .end
                        .map(|t| (t.0 / 1_000_000) as i64)
                        .unwrap_or(i64::MAX);
                    start_ms <= to_ms && end_ms >= from_ms
                })
                .collect();
            history_facts_to_array(ev, &facts)?
        }
        _ => return Ok(None),
    }))
}

/// Timer primitives: `_primCancelTimer:`, `_primTimerPending:`.
#[inline(never)]
fn obj_prim_timers(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        "_primCancelTimer:" => {
            // Requires: args[0] is a Value::Handle returned by _primScheduleTimer:block:.
            // Ensures: the timer callback will not be called if it has not already fired.
            //   No-op if already fired. Returns Value::Nil.
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Handle(h) => {
                    ev.os.cancel_timer(&h);
                    Value::Nil
                }
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primCancelTimer: argument must be a Handle, got {}",
                        other.kind()
                    )));
                }
            }
        }
        "_primTimerPending:" => {
            // Requires: args[0] is a Value::Handle returned by _primScheduleTimer:block:.
            // Ensures: returns Value::Bool(true) iff the timer has not fired and is not cancelled.
            expect_arity(sel, 1, args.len())?;
            match force_value(args.remove(0))? {
                Value::Handle(h) => Value::Bool(ev.os.timer_pending(&h)),
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primTimerPending: argument must be a Handle, got {}",
                        other.kind()
                    )));
                }
            }
        }
        _ => return Ok(None),
    }))
}

/// Multi-entity transaction primitives (C31).
#[inline(never)]
fn obj_prim_tx(
    cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    let ev = cx.ev;
    Ok(Some(match sel.as_ref() {
        "_primBeginSharedTx" => {
            // Begin a transaction level. If no transaction is active, create a
            // Root under a fresh shared TxId; otherwise create a Child of the
            // current context. Sets `current_tx_ctx` to the new level and returns
            // it as an opaque handle.
            expect_arity(sel, 0, args.len())?;
            let new_ctx: Arc<TxContext> = match ev.current_tx_ctx.borrow().clone() {
                None => {
                    let tx_id = ev.store.alloc_tx_id();
                    let buf = ev.store.begin_owned_with_id(ExecContext::System, tx_id);
                    Arc::new(TxContext::Root(Arc::new(RootTxInner {
                        ctx_id: next_ctx_id(),
                        tx_id,
                        tx: Mutex::new(Some(buf)),
                        enrolled: Mutex::new(Vec::new()),
                    })))
                }
                Some(outer) => {
                    let buf = ev
                        .store
                        .begin_owned_with_id(ExecContext::System, outer.tx_id());
                    Arc::new(TxContext::Child(Arc::new(ChildTxInner {
                        ctx_id: next_ctx_id(),
                        outer,
                        tx: Mutex::new(Some(buf)),
                        participants: Mutex::new(Vec::new()),
                    })))
                }
            };
            *ev.current_tx_ctx.borrow_mut() = Some(new_ctx.clone());
            Value::Handle(new_ctx)
        }
        "_primCommitSharedTx:" => {
            // Finalize a transaction level. Root: commit the context buffer to the
            // store and clear `current_tx_ctx`. Child: absorb the child buffer into
            // its parent and restore the parent as the active context.
            expect_arity(sel, 1, args.len())?;
            let ctx = tx_ctx_from_handle(force_value(args.remove(0))?)?;
            match &*ctx {
                TxContext::Root(r) => {
                    let buf = r.tx.lock().unwrap().take();
                    if let Some(tx) = buf {
                        commit_shared_buffer(ev, tx)?;
                    }
                    *ev.current_tx_ctx.borrow_mut() = None;
                }
                TxContext::Child(c) => {
                    if let Some(child_buf) = c.tx.lock().unwrap().take()
                        && let Some(parent_buf) = c.outer.buffer().lock().unwrap().as_mut()
                    {
                        parent_buf.absorb(child_buf);
                    }
                    *ev.current_tx_ctx.borrow_mut() = Some(c.outer.clone());
                }
            }
            Value::Nil
        }
        "_primRollbackSharedTx:" => {
            // Abort a transaction level. Root: drop the context buffer (its ops
            // never commit) and clear `current_tx_ctx`. Child: drop the child
            // buffer and restore the parent as the active context.
            expect_arity(sel, 1, args.len())?;
            let ctx = tx_ctx_from_handle(force_value(args.remove(0))?)?;
            match &*ctx {
                TxContext::Root(r) => {
                    r.tx.lock().unwrap().take(); // drop = rollback
                    *ev.current_tx_ctx.borrow_mut() = None;
                }
                TxContext::Child(c) => {
                    c.tx.lock().unwrap().take(); // drop child ops
                    *ev.current_tx_ctx.borrow_mut() = Some(c.outer.clone());
                }
            }
            Value::Nil
        }
        "_primEnrolledParticipants:" => {
            // Return this level's participants as a Cons list (or nil). A Cons is a
            // Value (synchronous dispatch), so the coordinator iterates it in its
            // own coroutine and the pvtExitTx: sends it makes are tagged.
            expect_arity(sel, 1, args.len())?;
            let ctx = tx_ctx_from_handle(force_value(args.remove(0))?)?;
            let parts: Vec<Value> = ctx
                .participants()
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .map(Value::Instance)
                .collect();
            build_cons_list(ev, parts)?
        }
        "_primExitTxMode:" => {
            // Participant-side finalize of its current transaction frame, driven by
            // the coordinator. `outcome` is #commit or #rollback. The transaction
            // context is taken from the tag on this (pvtExitTx) message.
            expect_arity(sel, 1, args.len())?;
            let outcome = match force_value(args.remove(0))? {
                Value::Symbol(s) => s,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primExitTxMode: expects #commit or #rollback, got {}",
                        other.kind()
                    )));
                }
            };
            let d = match &cx.handle.0.kind {
                InstanceKind::Entity(d) => d.as_ref(),
                _ => {
                    return Err(EvalError::TypeError(
                        "_primExitTxMode: receiver must be an entity".into(),
                    ));
                }
            };
            if !d.in_tx() {
                return Ok(Some(Value::Nil)); // defensive: nothing to finalize
            }
            let ctx = ev.current_tx_ctx.borrow().clone();
            match outcome.as_ref() {
                "commit" => {
                    let parent_ctx_id = ctx.as_deref().and_then(|c| match c {
                        TxContext::Child(ch) => Some(ch.outer.ctx_id()),
                        TxContext::Root(_) => None,
                    });
                    match d.tx_commit_frame(parent_ctx_id) {
                        FrameCommitOutcome::RootCommitted(buf) => {
                            if let Some(c) = ctx.as_deref()
                                && let Some(ctx_buf) = c.buffer().lock().unwrap().as_mut()
                            {
                                ctx_buf.absorb(buf);
                            }
                        }
                        FrameCommitOutcome::PassedUpTo(_) => {
                            // Lock passed up: become a participant of the parent so
                            // its later commit/rollback reaches this entity.
                            if let Some(TxContext::Child(ch)) = ctx.as_deref() {
                                ch.outer
                                    .participants()
                                    .lock()
                                    .unwrap()
                                    .push(cx.handle.clone());
                            }
                        }
                        FrameCommitOutcome::MergedIntoAncestor => {}
                    }
                }
                "rollback" => d.tx_abort_frame(),
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primExitTxMode: unknown outcome #{other}"
                    )));
                }
            }
            Value::Nil
        }
        _ => return Ok(None),
    }))
}

/// Explicit Future reification (C31): `_primNewFutureHandle`,
/// `_primResolveFuture:value:`, `_primRejectFuture:error:`.
#[inline(never)]
fn obj_prim_futures(
    _cx: &ObjPrim,
    sel: &Arc<str>,
    args: &mut Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    Ok(Some(match sel.as_ref() {
        "_primNewFutureHandle" => {
            expect_arity(sel, 0, args.len())?;
            Value::Future(FutureHandle::new())
        }
        "_primResolveFuture:value:" => {
            expect_arity(sel, 2, args.len())?;
            let h = args.remove(0);
            let v = force_value(args.remove(0))?;
            match h {
                Value::Future(fh) => {
                    fh.resolve_ok(v);
                    Value::Nil
                }
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primResolveFuture:value: expects a future handle, got {}",
                        other.kind()
                    )));
                }
            }
        }
        "_primRejectFuture:error:" => {
            expect_arity(sel, 2, args.len())?;
            let h = args.remove(0);
            let e = force_value(args.remove(0))?;
            match h {
                Value::Future(fh) => {
                    fh.resolve_err(EvalError::Signal(e));
                    Value::Nil
                }
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primRejectFuture:error: expects a future handle, got {}",
                        other.kind()
                    )));
                }
            }
        }
        _ => return Ok(None),
    }))
}

/// Extract a HandlerContext's handler pairs from its `pairs` ivar.
///
/// Requires: `handle` is a HandlerContext instance whose `pairs` ivar is a
///   Cons-of-Association linked list `Cons(head: cls -> blk, tail: rest)`, newest
///   at the head.
/// Ensures: returns `(condition class, handler block)` pairs ordered oldest-first,
///   ready to push onto `ev.handler_chain` so that `signal_condition`'s reverse
///   walk matches the most-recently-registered handler first.
pub(crate) fn extract_handler_pairs(
    handle: &InstanceHandle,
) -> Result<Vec<crate::eval::HandlerPair>, EvalError> {
    let pairs_val = match &handle.0.kind {
        InstanceKind::ValueObject(o) => o.get_slot("pairs").unwrap_or(Value::Nil),
        InstanceKind::Entity(d) => d.data.get_slot("pairs").unwrap_or(Value::Nil),
        InstanceKind::ClassEntity(_) => {
            return Err(EvalError::TypeError(
                "_primHCRun: requires a HandlerContext instance".into(),
            ));
        }
    };

    let mut handler_pairs: Vec<crate::eval::HandlerPair> = Vec::new();
    let mut node = force_value(pairs_val)?;
    while let Value::Instance(node_h) = node {
        let node_obj = match &node_h.0.kind {
            InstanceKind::ValueObject(o) => o.clone(),
            InstanceKind::Entity(d) => d.data.clone(),
            InstanceKind::ClassEntity(_) => break,
        };
        // head is an Association(key=cls, value=blk); tail is rest.
        let head_val = force_value(node_obj.get_slot("head").unwrap_or(Value::Nil))?;
        let tail = force_value(node_obj.get_slot("tail").unwrap_or(Value::Nil))?;

        let assoc_h = match head_val {
            Value::Instance(h) => h,
            _ => {
                node = tail;
                continue;
            }
        };
        let assoc_obj = match &assoc_h.0.kind {
            InstanceKind::ValueObject(o) => o.clone(),
            InstanceKind::Entity(d) => d.data.clone(),
            InstanceKind::ClassEntity(_) => {
                node = tail;
                continue;
            }
        };
        let cls_val = force_value(assoc_obj.get_slot("key").unwrap_or(Value::Nil))?;
        let blk_val = force_value(assoc_obj.get_slot("value").unwrap_or(Value::Nil))?;

        let cls = match cls_val {
            Value::Instance(ch) => ch.0.class.clone(),
            _ => {
                node = tail;
                continue;
            }
        };
        let blk = match blk_val {
            Value::Block(b) => b,
            _ => {
                node = tail;
                continue;
            }
        };
        handler_pairs.push((cls, blk));
        node = tail;
    }
    // Reverse so oldest pair is pushed first.
    handler_pairs.reverse();
    Ok(handler_pairs)
}

/// Core signal dispatch: walk the handler_chain, then apply unhandled policy.
///
/// Requires: `condition` is a `Value::Instance` whose class is a subclass of Condition.
/// Ensures: if a matching non-unwinding handler is found its return value is returned;
///   if none match, applies unhandled policy (Condition → nil, Warning → log+nil,
///   Error → raise EvalError::Signal).
pub(crate) fn signal_condition(
    ev: &Evaluator,
    condition: Value,
    env: &mut Env,
) -> Result<Value, EvalError> {
    #[cfg(feature = "step_probe")]
    {
        let sp_probe = 0u8;
        ev.probe
            .sample_signal_condition(&sp_probe as *const u8 as usize);
    }
    let cond_class = match &condition {
        Value::Instance(h) => h.0.class.clone(),
        _ => return Err(EvalError::Signal(condition)),
    };

    // Walk handler_chain in reverse (most-recently-pushed first).
    let chain_snapshot = ev.handler_chain.borrow().clone();
    let mut re_signaled = false;
    for (handler_class, handler_block) in chain_snapshot.iter().rev() {
        if crate::class::Class::is_subclass_of(&cond_class, handler_class) {
            match ev.eval_block(handler_block, vec![condition.clone()], env) {
                Ok(v) => return Ok(v),
                Err(EvalError::Resume(v)) => return Ok(v),
                Err(EvalError::ReturnFromBlock(v)) => return Err(EvalError::ReturnFromBlock(v)),
                Err(EvalError::Retry) => return Err(EvalError::Retry),
                Err(EvalError::Signal(_)) => {
                    re_signaled = true;
                    break;
                }
                Err(other) => return Err(other),
            }
        }
    }
    if re_signaled {
        return Err(EvalError::Signal(condition));
    }

    // No handler matched — apply unhandled policy by condition class.
    let kernel = ev.registry.kernel();
    let is_error = kernel
        .lookup_class("Error")
        .map(|h| crate::class::Class::is_subclass_of(&cond_class, &h.0.class))
        .unwrap_or(false);
    let is_warning = !is_error
        && kernel
            .lookup_class("Warning")
            .map(|h| crate::class::Class::is_subclass_of(&cond_class, &h.0.class))
            .unwrap_or(false);

    if is_error {
        Err(EvalError::Signal(condition))
    } else if is_warning {
        let policy = kernel
            .lookup_class("Warning")
            .and_then(|h| h.0.class.lookup_class_var_owner("LoggingPolicy"))
            .and_then(|owner| owner.class_var_storage.read().get("LoggingPolicy").cloned());
        let message = condition_message(&condition);
        match policy.as_ref() {
            Some(Value::Symbol(s)) if s.as_ref() == "suppress" => {}
            Some(Value::Symbol(s)) if s.as_ref() == "stdout" => {
                println!("WARNING: {message}");
            }
            _ => {
                eprintln!("WARNING: {message}");
            }
        }
        Ok(Value::Nil)
    } else {
        Ok(Value::Nil)
    }
}

/// Extract the `message` ivar from a condition instance, or fall back to a string representation.
pub(crate) fn condition_message(condition: &Value) -> String {
    if let Value::Instance(h) = condition {
        let obj = match &h.0.kind {
            InstanceKind::ValueObject(o) => o.get_slot("message"),
            InstanceKind::Entity(d) => d.data.get_slot("message"),
            InstanceKind::ClassEntity(_) => None,
        };
        if let Some(Value::Str(s)) = obj {
            return s.to_string();
        }
    }
    format!("{condition}")
}

fn is_kind_of(class: &Arc<Class>, target: &Arc<Class>) -> bool {
    if Arc::ptr_eq(class, target) {
        return true;
    }
    if let Some(sc) = &class.superclass {
        is_kind_of(sc, target)
    } else {
        false
    }
}

/// Maps a value to its kernel class entity handle for `_primClass` / `_primIsKindOf:`.
///
/// Requires: `v` is not a `Value::Future` (caller must force first).
/// Ensures: returns `Some(handle)` where handle is the class entity for `v`'s type,
///   or `None` if no kernel class is available (ClassEntity receiver, Namespace).
fn value_to_class_handle(ev: &Evaluator, v: &Value) -> Option<InstanceHandle> {
    let class_name = match v {
        Value::Bool(b) => {
            if *b {
                "True"
            } else {
                "False"
            }
        }
        Value::Int(_) | Value::LargeInt(_) => "Int",
        Value::Float(_) => "Float",
        Value::Str(_) => "String",
        Value::Symbol(_) => "Symbol",
        Value::Block(_) => "Block",
        Value::Nil => "Nil",
        Value::Instance(h) => {
            return match &h.0.kind {
                InstanceKind::ValueObject(obj) => class_handle_of(obj).ok(),
                InstanceKind::Entity(d) => class_handle_of(&d.data).ok(),
                InstanceKind::ClassEntity(_) => None,
            };
        }
        _ => return None,
    };
    ev.registry.kernel().lookup_class(class_name)
}

/// Assert `(entity, __instance_of__, class_entity)` in a fresh one-shot transaction.
///
/// Requires: `inst` is an Entity kind handle.
/// Ensures: a `__instance_of__` fact is committed to the shared store. Silent no-op
///   if the class has no backing class-entity (e.g. during bootstrap).
fn assert_instance_of(ev: &Evaluator, inst: &InstanceHandle, class: &Arc<Class>) {
    let entity_id = match &inst.0.kind {
        InstanceKind::Entity(d) => d.id,
        _ => return,
    };
    let Some(class_eid) = class_entity_id_of(class) else {
        return;
    };
    let mut tx = ev.store.begin_owned(ExecContext::Entity(entity_id));
    let _ = tx.assert_single(Fact {
        subject: entity_id,
        predicate: ev.registry.instance_of_pred,
        object: Term::EntityRef(class_eid),
        asserter: entity_id,
        valid: None,
    });
    let _ = tx.commit();
}

/// Extract the EntityId of the class-entity backing `class`, if available.
fn class_entity_id_of(class: &Arc<Class>) -> Option<EntityId> {
    class
        .class_object
        .get()
        .and_then(|w| w.upgrade())
        .and_then(|s| match &s.kind {
            InstanceKind::ClassEntity(d) => Some(d.id),
            _ => None,
        })
}

/// Build an `Array` entity pre-filled with `values`.
///
/// Allocates a fresh Array entity, fills its indexed Pointer slots directly at
/// the Rust level (no cross-entity dispatch), tracks it with the evaluator, and
/// returns it. Used by `historyOf:` and `_primInstancesOf:`.
fn build_array(ev: &Evaluator, values: Vec<Value>) -> Result<Value, EvalError> {
    let array_class_handle = ev
        .registry
        .kernel()
        .lookup_class("Array")
        .ok_or_else(|| EvalError::TypeError("Array not in kernel".into()))?;
    let array_class = array_class_handle.0.class.clone();

    let mut global_env = crate::env::Env::new();
    ev.registry.populate_env(&mut global_env);
    let arr = spawn_entity_bare(
        array_class,
        ev.registry.clone(),
        global_env,
        Some(values.len()),
        ev.store.clone_handle(),
    );
    ev.track_entity(&arr);

    if let InstanceKind::Entity(d) = &arr.0.kind
        && let Some(IndexedStorage::Pointer(slots)) = &d.data.indexed
    {
        let mut guard = slots.lock();
        for (i, v) in values.into_iter().enumerate() {
            guard[i] = v;
        }
    }

    Ok(Value::Instance(arr))
}

/// Build a `Cons` linked list (head-first) from `items`, or `Value::Nil` if empty.
///
/// Returns a Value-object chain (synchronous dispatch), so a caller can iterate
/// it with `do:` in its own coroutine — used for transaction participant lists,
/// where the iterating coordinator must send tagged messages from its own
/// coroutine (an `Array` entity's `do:` would run in the array's coroutine, where
/// `current_tx_ctx` is unset and the sends would be untagged).
fn build_cons_list(ev: &Evaluator, items: Vec<Value>) -> Result<Value, EvalError> {
    let cons_class = ev
        .registry
        .kernel()
        .lookup_class("Cons")
        .ok_or_else(|| EvalError::TypeError("Cons not in kernel".into()))?
        .0
        .class
        .clone();
    let mut list = Value::Nil;
    for item in items.into_iter().rev() {
        let obj = Arc::new(Object::new(cons_class.clone()));
        obj.set_slot(Arc::from("head"), item);
        obj.set_slot(Arc::from("tail"), list);
        list = Value::Instance(InstanceHandle(Arc::new(InstanceState {
            class: cons_class.clone(),
            kind: InstanceKind::ValueObject(obj),
        })));
    }
    Ok(list)
}

/// Convert a slice of `StoredFact` to an `Array` of `Association(ts_ms → value)`.
///
/// Each Association has `key = epoch-milliseconds (Int)` and `value = resolved object`.
fn history_facts_to_array(ev: &Evaluator, facts: &[StoredFact]) -> Result<Value, EvalError> {
    let assocs: Vec<Value> = facts
        .iter()
        .map(|f| {
            let ts_ms = Value::Int((f.valid.start.0 / 1_000_000) as i64);
            let val = ev.resolve_term(&f.object);
            make_association(ev, ts_ms, val)
        })
        .collect::<Result<_, _>>()?;
    build_array(ev, assocs)
}

/// Build an Association ValueObject with `key` and `value` set.
fn make_association(ev: &Evaluator, key: Value, value: Value) -> Result<Value, EvalError> {
    let assoc_class_handle = ev
        .registry
        .kernel()
        .lookup_class("Association")
        .ok_or_else(|| EvalError::TypeError("Association not in kernel".into()))?;
    let assoc_class = assoc_class_handle.0.class.clone();
    let obj = Arc::new(Object::new(assoc_class.clone()));
    obj.set_slot(Arc::from("key"), key);
    obj.set_slot(Arc::from("value"), value);
    Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
        class: assoc_class,
        kind: InstanceKind::ValueObject(obj),
    }))))
}

// ── Namespace ─────────────────────────────────────────────────────────────────

fn dispatch_namespace(
    ev: &Evaluator,
    ns: Arc<Namespace>,
    selector: Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    match selector.as_ref() {
        "do:" => {
            expect_arity(&selector, 1, args.len())?;
            let block = match args.remove(0) {
                Value::Block(b) => b,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "do: requires a Block, got {}",
                        other.kind()
                    )));
                }
            };
            ev.with_namespace(ns, |ev| ev.eval_block(&block, vec![], env))
        }
        "name" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Symbol(ns.name.clone()))
        }
        "printString" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Str(Arc::from(
                format!("Namespace({})", ns.name).as_str(),
            )))
        }
        _ => Err(EvalError::MessageNotUnderstood {
            receiver_kind: "Namespace",
            selector,
        }),
    }
}

// ── Class (class-side dispatch) ───────────────────────────────────────────────

fn dispatch_class(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let entity_id = match &handle.0.kind {
        InstanceKind::ClassEntity(d) => d.id,
        _ => unreachable!("dispatch_class called on non-ClassEntity instance"),
    };

    // Self-send or structural class-definition message: dispatch synchronously.
    // `subclass:*` must run in the caller's context so that env bindings and
    // namespace registration are immediately visible to the next statement.
    // Value-class methods (classes whose instances are passive ValueObjects) are also
    // dispatched synchronously: they carry no actor-mailbox semantics, so
    // cross-entity dispatch would be wrong.
    // `=` and `~=` on class objects are pure structural identity checks — they
    // must be synchronous so callers don't receive a Future where a Bool is expected.
    let current = ev.current_entity_id.get();
    if current == Some(entity_id)
        || selector.starts_with("subclass:")
        || handle.0.class.inherits_is_value()
        || matches!(selector.as_ref(), "=" | "~=")
    {
        let class = handle.0.class.clone();
        if let Some((method, def_class)) = class.lookup_class_method(&selector) {
            return ev.invoke_class_method(handle, def_class, method, args, env);
        }
        return dispatch_class_builtin(ev, handle, selector, args, env);
    }

    // Cross-entity behavioral send: enqueue and return Future.
    // Snapshot the caller's handler_chain so inherited HC handlers propagate.
    let chain_snapshot = {
        let chain = ev.handler_chain.borrow();
        if chain.is_empty() {
            vec![]
        } else {
            chain.clone()
        }
    };
    let tx_tag = ev.current_tx_ctx.borrow().clone();
    let future = match &handle.0.kind {
        InstanceKind::ClassEntity(d) => send_to_entity(d, selector, args, chain_snapshot, tx_tag),
        _ => unreachable!(),
    };
    Ok(Value::Future(future))
}

/// Built-in class-side dispatch that bypasses user-defined class methods.
///
/// Called directly from `dispatch_class` (after the user-method check) and from
/// `send_super` when a class-side super send finds no user-defined method in the
/// superclass chain — allowing `super new` / `super new:` to reach Rust primitives
/// without triggering the receiver class's own `new` override.
pub(crate) fn dispatch_class_builtin(
    ev: &Evaluator,
    handle: InstanceHandle,
    selector: Arc<str>,
    mut args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    let class = handle.0.class.clone();
    let self_val = Value::Instance(handle);
    match selector.as_ref() {
        "=" => {
            expect_arity(&selector, 1, args.len())?;
            Ok(Value::Bool(self_val == force_value(args[0].clone())?))
        }
        "~=" => {
            expect_arity(&selector, 1, args.len())?;
            Ok(Value::Bool(self_val != force_value(args.remove(0))?))
        }
        "_primNew" => {
            expect_arity(&selector, 0, args.len())?;
            // Indexed classes get zero-size indexed storage; non-indexed get plain Object.
            let indexed_size = if class.indexed_slot_kind.is_some() {
                Some(0usize)
            } else {
                None
            };
            if class.inherits_is_value() {
                // Value subclasses: create and freeze immediately. Any post-construction
                // ivar write will fail with the frozen error. Correct code uses _primNewValue:.
                let obj = match indexed_size {
                    Some(n) => Arc::new(Object::new_with_indexed(class.clone(), n)),
                    None => Arc::new(Object::new(class.clone())),
                };
                obj.freeze();
                return Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
                    class,
                    kind: InstanceKind::ValueObject(obj),
                }))));
            }
            {
                let mut global_env = Env::new();
                ev.registry.populate_env(&mut global_env);
                let inst = spawn_entity_bare(
                    class.clone(),
                    ev.registry.clone(),
                    global_env,
                    indexed_size,
                    ev.store.clone_handle(),
                );
                ev.track_entity(&inst);
                assert_instance_of(ev, &inst, &class);
                Ok(Value::Instance(inst))
            }
        }
        "_primNewValue:" => {
            // Construct a frozen Value-subclass instance via an initialization block.
            //
            // Requires: args[0] is a 1-arg Block; receiver class inherits is_value.
            // Ensures: creates a new (unfrozen) ValueObject (zero-size indexed storage
            //   for indexed classes), evaluates the block with the instance as argument
            //   (mutations permitted during block), freezes on block return, returns
            //   the frozen instance.
            expect_arity(&selector, 1, args.len())?;
            if !class.inherits_is_value() {
                return Err(EvalError::TypeError(
                    "_primNewValue: requires a Value subclass".into(),
                ));
            }
            let blk = as_block(args.remove(0), &selector)?;
            let obj = if class.indexed_slot_kind.is_some() {
                Arc::new(Object::new_with_indexed(class.clone(), 0))
            } else {
                Arc::new(Object::new(class.clone()))
            };
            let inst = InstanceHandle(Arc::new(InstanceState {
                class,
                kind: InstanceKind::ValueObject(obj.clone()),
            }));
            ev.eval_block(&blk, vec![Value::Instance(inst.clone())], env)?;
            obj.freeze();
            Ok(Value::Instance(inst))
        }
        "_primNewValueIndexed:init:" => {
            // Construct a frozen indexed Value-subclass instance via an initialization block.
            //
            // Requires: args[0] is an Int size; args[1] is a 1-arg Block;
            //   receiver class inherits is_value and declares indexedSlots:.
            // Ensures: creates an indexed ValueObject of the given size, evaluates
            //   the block with the instance as argument, freezes on block return.
            expect_arity(&selector, 2, args.len())?;
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
            let n = as_int(args.remove(0), &selector)?;
            let blk = as_block(args.remove(0), &selector)?;
            let obj = Arc::new(Object::new_with_indexed(class.clone(), n.max(0) as usize));
            let inst = InstanceHandle(Arc::new(InstanceState {
                class,
                kind: InstanceKind::ValueObject(obj.clone()),
            }));
            ev.eval_block(&blk, vec![Value::Instance(inst.clone())], env)?;
            obj.freeze();
            Ok(Value::Instance(inst))
        }
        "_primScheduleTimer:block:" => {
            // Requires: args[0] is Value::Int (≥ 0, milliseconds); args[1] is Value::Block.
            // Ensures: after ms milliseconds (if not cancelled), evaluates the block in a
            //   fresh may coroutine inheriting the current continuation context. The block
            //   is responsible for all Smol-level dispatch (e.g. Timer >> pvtTimerFired).
            //   Returns Value::Handle opaque to callers.
            expect_arity(&selector, 2, args.len())?;
            let ms = as_int(args.remove(0), &selector)?.max(0) as u64;
            let blk = as_block(args.remove(0), &selector)?;
            let ctx = ev.fork_context();
            let callback = Box::new(move || {
                may::go!(move || {
                    let ev = ctx.into_evaluator();
                    let mut env = blk.env.clone();
                    let _ = ev.eval_block(&blk, vec![], &mut env);
                });
            });
            let handle = ev.os.schedule_timer(ms, callback);
            Ok(Value::Handle(handle))
        }
        "_primNew:" => {
            expect_arity(&selector, 1, args.len())?;
            let n = as_int(args.remove(0), &selector)?;
            if class.indexed_slot_kind.is_none() {
                return Err(EvalError::TypeError(
                    "_primNew: requires class to declare indexedSlots:".into(),
                ));
            }
            if class.inherits_is_value() {
                let obj = Arc::new(Object::new_with_indexed(class.clone(), n.max(0) as usize));
                obj.freeze();
                return Ok(Value::Instance(InstanceHandle(Arc::new(InstanceState {
                    class,
                    kind: InstanceKind::ValueObject(obj),
                }))));
            }
            {
                let mut global_env = Env::new();
                ev.registry.populate_env(&mut global_env);
                let inst = spawn_entity_bare(
                    class.clone(),
                    ev.registry.clone(),
                    global_env,
                    Some(n.max(0) as usize),
                    ev.store.clone_handle(),
                );
                ev.track_entity(&inst);
                assert_instance_of(ev, &inst, &class);
                Ok(Value::Instance(inst))
            }
        }
        "_primInstancesOf:" => {
            expect_arity(&selector, 1, args.len())?;
            let target_handle = match force_value(args.remove(0))? {
                Value::Instance(h) => h,
                other => {
                    return Err(EvalError::TypeError(format!(
                        "_primInstancesOf: argument must be a class, got {}",
                        other.kind()
                    )));
                }
            };
            let target_class = target_handle.0.class.clone();
            let instances: Vec<Value> = ev
                .registry
                .instance_entities
                .read()
                .values()
                .filter(|state| Arc::ptr_eq(&state.class, &target_class))
                .map(|state| Value::Instance(InstanceHandle(state.clone())))
                .collect();
            build_array(ev, instances)
        }
        "name" => {
            expect_arity(&selector, 0, args.len())?;
            Ok(Value::Str(class.name.clone()))
        }
        "printString" => {
            expect_arity(&selector, 0, args.len())?;
            let s: Arc<str> = class.name.clone();
            println!("{:?}", s);
            Ok(Value::Str(s))
        }
        sel if sel.contains("subclass:") => {
            // Extract the handle back from self_val to pass the Arc<Class> as superclass.
            handle_subclass(ev, class, selector, args, env)
        }
        _ => Err(EvalError::MessageNotUnderstood {
            receiver_kind: "Class",
            selector,
        }),
    }
}

fn handle_subclass(
    ev: &Evaluator,
    superclass: Arc<Class>,
    selector: Arc<str>,
    args: Vec<Value>,
    env: &mut Env,
) -> Result<Value, EvalError> {
    // `selector` is something like "subclass:instanceVariableNames:classVariableNames:"
    // Split on ':' boundaries to get the individual keyword parts.
    // Each keyword ends with ':', so split_inclusive(':') gives us each kw fragment.
    let keywords: Vec<&str> = selector.split_inclusive(':').collect();

    let mut class_name: Option<Arc<str>> = None;
    let mut ivar_names: Vec<Arc<str>> = vec![];
    let mut class_var_names: Vec<Arc<str>> = vec![];
    let mut indexed_slot_kind: Option<IndexedSlotKind> = None;

    for (kw, arg) in keywords.iter().zip(args.iter()) {
        match *kw {
            "subclass:" => match arg {
                Value::Symbol(s) => class_name = Some(s.clone()),
                _ => {
                    return Err(EvalError::TypeError(
                        "subclass: argument must be a Symbol".into(),
                    ));
                }
            },
            "instanceVariableNames:" => match arg {
                Value::Str(s) => {
                    ivar_names = s
                        .split_whitespace()
                        .filter(|s| !s.is_empty())
                        .map(Arc::from)
                        .collect();
                }
                _ => {
                    return Err(EvalError::TypeError(
                        "instanceVariableNames: argument must be a String".into(),
                    ));
                }
            },
            "classVariableNames:" => match arg {
                Value::Str(s) => {
                    class_var_names = s
                        .split_whitespace()
                        .filter(|s| !s.is_empty())
                        .map(Arc::from)
                        .collect();
                }
                _ => {
                    return Err(EvalError::TypeError(
                        "classVariableNames: argument must be a String".into(),
                    ));
                }
            },
            "indexedSlots:" => match arg {
                Value::Symbol(s) => match s.as_ref() {
                    "none" => indexed_slot_kind = None,
                    "pointer" => indexed_slot_kind = Some(IndexedSlotKind::Pointer),
                    "byte" => indexed_slot_kind = Some(IndexedSlotKind::Byte),
                    "float64" => indexed_slot_kind = Some(IndexedSlotKind::Float64),
                    _ => {
                        return Err(EvalError::TypeError(format!(
                            "indexedSlots: unknown kind #{s}"
                        )));
                    }
                },
                _ => {
                    return Err(EvalError::TypeError(
                        "indexedSlots: argument must be a Symbol".into(),
                    ));
                }
            },
            _ => {} // unknown keywords silently ignored
        }
    }

    let name =
        class_name.ok_or_else(|| EvalError::TypeError("subclass: name is required".into()))?;

    // Validate ivar name uniqueness across the superclass chain.
    let super_ivars = superclass.all_instance_vars();
    for ivar in &ivar_names {
        if super_ivars.iter().any(|v| v == ivar) {
            return Err(EvalError::TypeError(format!(
                "instance variable '{ivar}' already declared in a superclass"
            )));
        }
    }

    let class_var_storage: HashMap<Arc<str>, Value> = class_var_names
        .iter()
        .map(|n| (n.clone(), Value::Nil))
        .collect();

    // Inherit is_value from the superclass chain. Mixed-hierarchy subclassing
    // (actor subclassing a value class or vice versa) is a compile-time error.
    let super_is_value = superclass.inherits_is_value();
    // The selector determines whether this is a value or actor declaration.
    // `Value subclass:` → is_value: true; `Object subclass:` (any non-Value) → inherits.
    // We detect this by checking if the superclass is the built-in Value class.
    let declared_is_value = super_is_value; // inherit by default

    let new_class = Arc::new(Class {
        name: name.clone(),
        superclass: Some(superclass),
        own_instance_vars: ivar_names,
        own_class_vars: class_var_names,
        class_var_storage: RwLock::new(class_var_storage),
        indexed_slot_kind,
        methods: RwLock::new(HashMap::new()),
        class_methods: RwLock::new(HashMap::new()),
        is_value: declared_is_value,
        class_object: std::sync::OnceLock::new(),
    });
    let class_handle = spawn_class_entity(
        new_class.clone(),
        ev.registry.clone(),
        Env::new(),
        ev.store.clone_handle(),
    );
    new_class
        .class_object
        .set(Arc::downgrade(&class_handle.0))
        .ok();
    ev.registry.register_class_entity(&class_handle);
    ev.registry.register_class_predicates(&new_class);

    let active_ns = ev.active_namespace();
    active_ns.register(class_handle.clone());
    // Kernel classes get a global binding so they're always in scope by simple name.
    // Non-Kernel classes are scoped to the current frame (e.g. inside `Namespace do: [...]`);
    // outside that scope they are only reachable via `Ns::ClassName`.
    if Arc::ptr_eq(&active_ns, &ev.registry.kernel()) {
        env.define_global(name, Value::Instance(class_handle.clone()));
    } else {
        env.define(name, Value::Instance(class_handle.clone()));
    }
    Ok(Value::Instance(class_handle))
}

#[cfg(test)]
#[path = "tests/builtins.rs"]
mod tests;
