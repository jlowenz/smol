# Chunk 33 plan — `doesNotUnderstand:` / reified `Message` / `perform:`

**Status:** IMPLEMENTED (2026-07-01), S0–S5 complete; 709 pass / 1 ignored.
Closes the seamless-`Future` work deferred from C31 S6. Independent of C31 S5b/S5c.

**Implementation notes (as built):**
- OQ-33-2 RESOLVED: no `_primNewMessage:`. The DNU hook reifies `Message` via the
  Rust helper `build_message` (builtins_iterative.rs), instantiating the
  Smol-declared class directly like Array literals. `perform:` is intercepted in
  `dispatch_to_stack`, not a `_prim`.
- **Two hook sites** (not one): synchronous/value receivers go through the
  `dispatch_to_stack` fallback hook (stays on the Cont stack); entity mailbox
  messages go through a hook in `dispatch_envelope` (entity.rs) invoking
  `doesNotUnderstand:` via `ev.invoke_method`. Self-sends inside a method use the
  first; external entity sends use the second.
- **Iterative-only**: the recursive kernel has no `Message`/DNU. ~10 tests that
  matched `EvalError::MessageNotUnderstood` via `eval_err_diff` were migrated to
  `eval_err` + `assert_signal_contains` (receivers routing through
  `lookup_user_method`: Int/Bool/Nil/Block/entity). Str/Symbol/Float/LargeInt do
  NOT route through user-method lookup, so they still surface the raw MNU on both
  paths — a known limitation (those primitive types don't get DNU).
- `Message arguments` = `Cons` (nil for 0-arg). `perform:withArguments:` unpacks
  it via `cons_to_vec` (spine forced, elements left unforced for Future transparency).

## Goal / motivation

Make *handling arbitrary messages* a first-class, Smol-expressible capability.
This is core Smalltalk dynamism (transparent futures, proxies, interceptors,
aspects, remote stubs). Today message-not-understood is a hard Rust
`EvalError::MessageNotUnderstood` with no Smol-level recovery hook, so behaviors
like seamless Future forwarding can only be faked with Rust special-cases.

**Ubiquitous language (bounded context = message dispatch):**
- *reified message* — a `Message` value carrying `selector` + `arguments`.
- *doesNotUnderstand:* (DNU) — the Smol method invoked when normal lookup fails,
  receiving the reified message; may forward/handle/re-raise.
- *perform* — reflective send: apply a selector + args to a receiver from Smol.

## Deliverables

1. **`Message` value class** (kernel.smol, subclass of `Value`).
   - Ivars: `selector` (Symbol), `arguments` (list/Cons).
   - Protocol: `selector`, `arguments`, `argumentCount`, `sendTo: anObject`
     (`^anObject perform: selector withArguments: arguments`), printing.
   - Constructed by the interpreter at the DNU site (via a `_primNewMessage:...`
     or direct Rust build of a `Value::Instance` ValueObject — decide in S1).

2. **`perform:` / `perform:withArguments:`** (reflective send).
   - `Object >> perform: aSymbol` → send unary `aSymbol` to self.
   - `Object >> perform: aSymbol withArguments: aColl` → send keyword/binary.
   - Implemented as a Rust primitive that re-enters `dispatch_to_stack` with the
     selector + args (must go through the iterative Cont-stack path, NOT a
     recursive eval, to preserve the flattened-stack invariant — see CLAUDE.md).
   - Arity must match selector shape (unary=0, binary=1, keyword=n colons).

3. **DNU dispatch hook** — the heart of the chunk.
   - **Hook point:** `eval_iterative.rs:2255`, the single fallback
     `dispatch(...)` at the end of `dispatch_to_stack`. Wrap it: on
     `Err(EvalError::MessageNotUnderstood { .. })`, look up `doesNotUnderstand:`
     as a *user method* on the (already-forced) receiver via `lookup_user_method`.
     Reify the failed `Message`, push a method call frame invoking
     `doesNotUnderstand:` with `[message]` as the arg (so it runs on the Cont
     stack, no Rust recursion).
   - **Default lives in Smol (option a, chosen).** `Object >> doesNotUnderstand:`
     is defined in kernel.smol and **signals a typed `MessageNotUnderstood`
     condition** (see deliverable 3b). Because every object inherits it,
     `lookup_user_method` always finds *something*, so the hook always routes
     into Smol; the Rust `EvalError::MessageNotUnderstood` at line 2255 becomes a
     pure internal trigger. (Only genuinely impossible case — no `Object` in the
     hierarchy — re-raises the raw error defensively.)
   - **Loop-guard:** the default `doesNotUnderstand:` must not itself send an
     unknown message (it only signals a condition through existing machinery), so
     it cannot recurse. A *user* override that performs another unknown selector
     re-enters the hook once per genuine miss, terminating when some send
     succeeds or the default signals.
   - **Scope for this chunk:** hook only the primary instance/value/entity
     fallback (line 2255). The *super*-send MNU sites (eval_iterative.rs:1720,
     1754) and class-side misses are OUT of scope (a super/class DNU is a
     separate, rarer concern) — document them as known non-hooked paths.

3b. **`MessageNotUnderstood` typed condition + default DNU** (option a).
   - `Error subclass: #MessageNotUnderstood` carrying `receiver` + `message`
     (the reified `Message`). Restart(s): `#return:` (resume the failed send with
     a supplied value); richer interactive restarts (retry/edit) deferred until a
     stack-view/editor exists.
   - `Object >> doesNotUnderstand: aMessage [ ^(MessageNotUnderstood ...
     receiver: self message: aMessage) signal ]`.
   - **Accepted cost:** ~30 Rust tests currently match
     `EvalError::MessageNotUnderstood { .. }` directly. With the default now
     signaling a condition, unhandled propagation surfaces as the condition
     system's error, not the raw variant. These tests migrate as a dedicated
     step (S3b) — the user accepted this churn for the cleaner end state.

4. **Rewrite `Future` to use DNU** (closes the C31 seamless-Future item).
   - `Future >> doesNotUnderstand: aMessage [ ^aMessage sendTo: handle await ]`.
   - Keep `await` (returns value), `then:`, `resolveWith:`, `rejectWith:` as
     explicit wrapper methods (they act on the wrapper/handle, not the resolved
     value, so they must NOT be forwarded).
   - Revert the C31 stopgap: `hc_then_signal_resolves_future_within_entity`
     (~tests/entity.rs:617) `^txf await` → `^txf`; with forwarding, returning the
     Future and letting the caller message it now behaves correctly.

## Steps

- **S0** — Generate `docs/primitives.md`: a table of every `_prim*` (selector,
  receiver kind, arity, one-line semantics, optimize? note). Seed from the 29
  existing prims; append `perform:` machinery + `_primNewMessage:` as they land.
- **S1** — `Message` value class (`arguments` = `Cons` list) + `_primNewMessage:`
  construction. Unit-test reification (selector/args/argumentCount).
- **S2** — `perform:` / `perform:withArguments:` primitives via re-entrant
  `dispatch_to_stack`. Tests: unary/binary/keyword, arity mismatch error,
  performing an unknown selector triggers DNU.
- **S3** — DNU hook at eval_iterative.rs:2255. Tests: object with a
  `doesNotUnderstand:` override intercepts; DNU body that performs a real
  selector works; user override performing another unknown selector terminates.
- **S3b** — `MessageNotUnderstood` typed condition + default
  `Object >> doesNotUnderstand:` (signals it). Migrate the ~30 Rust tests that
  match `EvalError::MessageNotUnderstood { .. }` to the condition/signal form.
  Test: unhandled default DNU signals the condition; `#return:` restart resumes.
- **S4** — `Future` rewrite + L617 revert. Re-run the C31 S6 matrix and the
  Future tests in eval.rs. Verify seamless forwarding: `(someFuture foo: 1)`
  awaits then forwards `foo:` to the resolved value.
- **S5** — Audit: every new kernel method exercised by a Rust test (both branches
  of DNU present/absent). Finalize `docs/primitives.md`. fmt + clippy. Update
  CLAUDE.md if the dispatch contract changes. Commit.

## Risks / open questions

- **OQ-33-1 — recursive vs iterative parity.** `kernel_recursive.smol` has no
  Future/Message. Does the recursive path need DNU too? `eval()` runs iterative
  only; `eval_diff()` runs both. If any DNU test uses `eval_diff`, the recursive
  path needs the hook at eval_recursive.rs:554/573. Default: implement iterative
  only, keep DNU tests on the iterative-only `eval()` helper, note recursive as
  unsupported (it is slated for retirement per CLAUDE.md).
- **OQ-33-2 — Message construction path.** Rust-built `Value::Instance`
  ValueObject vs a `_primNewMessage:selector:arguments:`. Prefer the primitive so
  the class shape stays defined in Smol.
- **OQ-33-3 — argument collection type. RESOLVED: `Cons` for now.** Message is a
  `Value`, so its `arguments` must be a value type (`Cons`), not the mutable
  `Array`/`OrderedCollection` entities. Ergonomic caveat (accepted): the
  `perform:` primitive reads the args *in Rust* by walking Cons, which is less
  ergonomic than indexed-slot access — a future **immutable indexable Array
  value** (its own chunk; we lack one today) would improve this. Not blocking C33.
- **OQ-33-4 — should `perform:` bypass DNU?** No — `perform:` of an unknown
  selector should itself trigger DNU (it is a normal send), which is what makes
  `sendTo:` forwarding compose.
- **OQ-33-5 (deferred) — stack-view consistency of implicit vs explicit futures.**
  Runtime keeps implicit `Value::Future` (more efficient; no wrapper/DNU hop per
  message). Explicit `Future` is for hand-constructed futures and is behaviorally
  identical via DNU. Concern raised: when inspecting the stack, the two look
  different. Deferred to a future session (needs the stack-view tooling anyway).

## Sequencing note

C31 S5b (nested-atomic auto-enrollment, ignored test entity.rs:827) and S5c
(deadlock detection) remain open and are independent of this chunk. Order TBD
with user; this chunk was chosen first because its design was fully in context.
