# Q24 experiment: an imperative algorithm without `^` in blocks

**Date:** 2026-11-09. **Question:** Q24 (`docs/open_questions.md`). **Status:**
exploratory; nothing decided.

## Purpose

Measure what dropping `^` in blocks costs on a non-trivial imperative algorithm with
many interleaved exits, and which tools reduce that cost.

## Setup

**Algorithm.** A calculator: a tokenizer, then shunting-yard to RPN, then RPN
evaluation, over integers, `+ - * /` (integer `/`), parentheses and spaces. It has
seven error exits:

| Phase | Error exits |
|---|---|
| Tokenizer | unexpected character |
| Shunting-yard | unmatched `)`; unmatched `(` |
| RPN evaluation | missing operand; division by zero; empty expression; missing operator |

It also has ordinary loop exits: end of input, end of a number, and the operator-popping
loops.

**Contract.** Each style defines a class whose `evaluate: aString` answers an Integer
or a String `'error: …'`. A shared harness checks 17 cases: precedence, left
associativity, nested parentheses, and every error exit.

**Styles:**

| File | Style |
|---|---|
| `q24_calc_s1_classic.smol` | S1, classic Smalltalk: temps rebound in loops, `^` from any block. **Reading reference only**; the parser rejects it under D13 ("block assigns to `c`, which is not declared in this block's params or locals"). |
| `q24_calc_s2_today.smol` | S2, Smol today: D13 plus `^` in blocks. Loop variables are ivars changed by pvt methods (D13 Pattern 2), and each step answers nil or an error that the caller re-raises (Pattern 4). |
| `q24_calc_s3_expr.smol` | S3, expression style with today's kernel only. No `^` in any block; loops are tail-recursive methods; Cons lists; errors are values passed through. |
| `q24_calc_s4_expr_lib.smol` | S4, expression style plus proposed library pieces: `loop:` with Continue/Break, `cond:`, `reversed`, and a `CalcError` condition. |
| `q24_calc_s4b_named_loop.smol` | S4b, S4 with the tokenizer as a *named* loop (`recur:`). |

**Run** from the repo root, after `cargo build -p interpreter --release`:

```
cat docs/experiments/q24_calc_prelude.smol docs/experiments/q24_calc_s4_expr_lib.smol | target/release/smol
```

S4b loads after S4: append `docs/experiments/q24_calc_s4b_named_loop.smol` to the
`cat`.

## Results

S2, S3, S4 and S4b pass 17/17.

| Style | Methods | Lines | Max block nesting | `^` in blocks | How errors get out |
|---|---|---|---|---|---|
| S1 (not legal) | 6 | 74 | 7 | 12 | `^'error…'` from anywhere |
| S2 | 14 | 80 | 3 | 22 | 6 re-raises, `ifNotNil: [:err \| ^err]` |
| S3 | 16 | 95 | 3 | 0 | 6 pass-through sites (`isKindOf: String`, `prepend:`) |
| S4 | 14 | 75 | 2 | 0 | `CalcError` signalled; one `on:do:` in `evaluate:` |

S4's library is 11 methods and 17 lines. None of it needs evaluator or VM support.
The S4b tokenizer is 1 method of 16 lines at nesting depth 5; S4's is 3 methods at
depth 2.

Counts come from a throwaway script that matches brackets after removing strings and
comments. "Lines" counts non-blank lines inside the class's method definitions.
"Nesting" counts block depth inside a method body.

The same step in each style, handling `)`:

```smalltalk
"S1"
[ops isEmpty ifTrue: [^'error: unmatched )']. ops last ~= '(']
    whileTrue: [out add: ops removeLast].
ops removeLast

"S2: its own pvt method, because the caller must re-raise the error"
Calc2 >> pvtCloseParen [
    [ops isEmpty ifTrue: [^'error: unmatched )']. ops last ~= '(']
        whileTrue: [out add: ops removeLast].
    ops removeLast.
    ^nil ]

"S3"
Calc3 >> close: toks ops: ops out: out [
    ^ops isNil
        ifTrue: ['error: unmatched )']
        ifFalse: [ops head = '('
            ifTrue: [self rpn: toks ops: ops tail out: out]
            ifFalse: [self close: toks ops: ops tail out: (Cons with: ops head rest: out)]] ]

"S4"
Calc4 >> close: toks ops: ops out: out [
    ^self cond: {
        [ops isNil] -> [CalcError signal: 'unmatched )'].
        [ops head = '('] -> [self rpn: toks ops: ops tail out: out].
        [true] -> [self close: toks ops: ops tail out: (Cons with: ops head rest: out)] } ]
```

## Findings

1. **D13 already makes today's Smol pay most of the cost.** S2 keeps loop variables
   in ivars and has methods that exist only to assign (`pvtSkip`, `pvtEmit:`,
   `pvtTakeDigit`). Every step answers "nil or an error" for its caller to re-raise.
   It must loop with `0 until: n do:` rather than `coll do:`, or `^` hits I5. It has a
   trap: `pvtSkip` must say `^nil`, because the implicit `self` return would read as
   an error. Compared with S2, S4 is the same size with no `^` in blocks and shallower
   nesting. S3 (today's kernel only) costs 2 more methods and 15 more lines.
2. **Interleaved exits split into two kinds, and each has a natural tool.**
   - The seven error exits fit a condition that is signalled and handled on the same
     coroutine. That holds here because every send goes to `self` or a value.
   - Ordinary exits become base cases of recursive methods, or the loop's stopping
     value.

   Neither needs a jump. S3's real pain was passing errors along by hand.
3. **Nested loops with an anonymous loop are obscure.** In S4 the inner (number) loop
   must end by encoding the outer loop's next step:
   `Break with: (Continue with: st key -> (Cons with: st value rest: toks))`.
   S3 avoids this because its loops are named methods: the inner loop's exit is a
   tail call to the outer loop's method.
4. **Named loops fix this.** In S4b the block receives `again`, which runs the next
   iteration (Scheme's named `let`). Continuing means calling `again value: next` in
   tail position; stopping means answering any other value. An inner loop continues
   the outer one by calling the outer `again`:
   ```smalltalk
   [c isDigit] -> [(i -> 0) recur: [:num :more |
       (num key < s size and: [(s at: num key) isDigit])
           ifTrue: [more value: num key + 1 -> (num value * 10 + (s at: num key) digitValue)]
           ifFalse: [again value: num key -> (Cons with: num value rest: toks)]]]
   ```
   The library is one method:
   `Object >> recur: aBlock [ ^aBlock value: self value: [:next | next recur: aBlock] ]`.
   These are plain calls, not escapes, so they mean the same thing wherever the block
   runs. A nested `recur:` in which the inner loop continues the outer one runs in
   constant space: 39.5 MB peak at 3×10⁴ outer iterations and 40.0 MB at 3×10⁵, with
   time linear. The nesting depth (5) can be reduced by passing `again` to a helper
   method.

   This separates two protocols. **Named loops** serve loops the caller owns.
   **Continue/Break values** serve iterators the callee owns (`do:`, folds), where
   the caller can only ask the callee to stop.
5. **Records are wanted.** Loop state is packed into Associations (`st key`,
   `st value`). The shunting-yard was written as methods partly because its three
   state variables do not fit in one Association.
6. **Where control-abstraction blocks run.** `{ … }` creates an `Array`, which is an
   entity. A `cond` defined on `Array` runs its clause blocks on the Array's
   coroutine; S4 sends `cond:` to `self` instead. Probes in which the home forces the
   result inside its own turn, including a clause block that itself forces a send
   back to the home, **completed** on the current tree. The reason was not traced,
   and it differs from I6's table row "block forces home, home waits → hangs" (shape
   `(col do: [...]) await`). What remains is cost (an entity per `{ … }` evaluation)
   and the question of which coroutine runs a control abstraction's blocks. An
   immutable tuple literal would remove both, and could serve as the record in
   finding 5.
7. **Timing (side note; performance is not a gate).** Per evaluation of a 39-token
   expression, over 300 runs:

   | Style | Time per evaluation | Why |
   |---|---|---|
   | S3 | ≈3.1 ms | values only |
   | S2 | ≈6.0 ms | sends to OrderedCollection entities |
   | S4 | ≈7.6 ms | an Array entity, Associations and blocks allocated per `cond:` |

   Candidates to recover S4's cost:
   - VD4 inlining of a `cond:` with a literal array of literal block pairs (Squeak
     inlines `caseOf:`);
   - tuple values instead of Associations;
   - avoiding the `again` block allocated per iteration.

   Unexplained and not chased: the S4b tokenizer on `'1 '` repeated n times takes
   2.6 s at n = 2×10⁴ and 44 s at n = 2×10⁵, which is superlinear (the space there
   is dominated by the token lists).

## Open

- Nested loops: is `recur:` with `again` the shape we want? What surface sugar would
  it get (a loop literal, or destructuring of the state)?
- Records/tuples: an immutable literal that is a value, usable for loop state and for
  `cond:` clauses.
- Why the forced `cond`-on-`Array` probes complete when I6's `do:` shape hangs.
- Whether a guard statement at method level is still wanted once conditions and
  named loops are in place. This experiment did not need one.
