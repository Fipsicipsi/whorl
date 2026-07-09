# corpus: a differential / ground-truth harness

The soundness claim is Whorl's entire moat, and the literature shows soundness
claims are routinely false in practice. This harness is how the claim gets
earned instead of asserted.

Each file in `cases/` is a small, hand-labeled Rust program:

```rust
// EXPECT: DEADLOCK
// DESC: classic two-account transfer; both locks are class Account.bal ...
```

`run.py` emits MIR for each case with stable `rustc`, runs the
[`../mir-poc`](../mir-poc) analyzer, and compares the verdict to the label.

## Run

```sh
python run.py
```

Needs stable `rustc` on PATH. Exit code is non-zero if any false negative exists.

## The metric that matters

A sound analyzer may reject a safe program (a false positive) but must never
accept an unsafe one. So the cases split into two error classes, and they are
not equal:

- **False negative** (labeled DEADLOCK, tool says SAFE) -- a **soundness
  violation**. Must be zero. The runner counts these separately and fails on any.
- **False positive** (labeled SAFE, tool says DEADLOCK) -- acceptable, but
  tracked, because too many make the tool useless.

Current status on this corpus: 13 cases, 12/13 match, 0 false negatives, 1 false
positive. The 13th case (`cross_call_deadlock.rs`) was added as a deliberate
red test: it proved both implementations had an interprocedural false negative
(a guard held across a call was invisible in the callee), which was then fixed
by call-edge tracking plus an entry-may fixpoint -- in the lint/solver pipeline
and in the PoC alike. The false positive is `outer_lock_serializes.rs`, the classic Havender
case (two same-class locks only ever taken under a common outer lock, so the
inversion cannot happen concurrently). It is deliberate: the strict class
partial order rejects the program, and the honest answer is an explicit escape
hatch (`with ordered(..)` / a trusted annotation), not a weakened verdict.
A second predicted false positive (guard released via `std::mem::drop`) was
fixed by sound move-tracking: a `_a = move _b` statement transfers guard
ownership, so the held-set follows the value, and `mem::drop` kills it.

## The dylint column (`run_dylint.py`)

`run_dylint.py` runs the REAL pipeline (cargo dylint -> events JSON ->
`whorl --events`) on every case and cross-checks it against both the ground
truth and the stable-MIR PoC. Two independent implementations of the same
analysis on the same corpus is a differential test in itself: any disagreement
is a bug in one of them. Current status: 12/13 match ground truth, 0 false
negatives, 0 divergences between the two implementations; the single miss is
the same deliberate Havender false positive in both.

## The `lockbud` head-to-head (`run_lockbud.py`) -- Direction 2, run

Lockbud (`github.com/BurtonQin/lockbud`) is the established static Rust deadlock
detector and an unsound bug-finder by design. `run_lockbud.py` runs it on every
case next to the Whorl verdict. Result on this corpus:

```
whorl false negatives: 0   | lockbud false negatives: 2 | lockbud false positives: 1
```

Lockbud misses `two_account_deadlock.rs` -- the canonical textbook deadlock --
and `branch_deadlock.rs`. Both misses are the same systematic blind spot: its
DoubleLock detector needs the SAME lock twice and its ConflictLock detector
needs an inverted order between two DISTINCT named locks, so an unordered
acquisition of two instances of the same lock class falls through both. That
class-level case is exactly what Whorl's lock-class abstraction (plus the
instance count) exists to catch. Notably, on `outer_lock_serializes.rs` (the
deliberate Havender false positive) lockbud reports the same false positive.

The thesis this table exists to test holds on this corpus: Whorl misses
nothing; its one false positive is deliberate, explained, and shared by the
incumbent. The corpus is small; the next step is scale, not celebration --
add the published Lockbud/Archerfish real-world bugs as cases and grow the
SAFE side with real driver/HAL-shaped code.

Reproducing: build lockbud from its repo with its own pinned nightly
(2026-02-07 -- do NOT mix it with the dylint template's nightly). As of
2026-07, lockbud master needs four small fixes to build on its own pinned
toolchain (its code lags the pin): `extern crate rustc_hash` is ambiguous
(the toolchain ships two rustc-hash rmetas) -- use
`rustc_data_structures::fx` instead; `StatementKind::Deinit` and
`MutatingUseContext::Deinit` were removed; `Operand::RuntimeChecks` is a new
match arm; `Input::source_name()` is gone (match `Input::File`/`Input::Str`);
`rustc_driver::catch_with_exit_code` now returns `ExitCode` (return it from
`main`).

## Caveats

This corpus is tiny and intra-procedural (the analyzer's current limit). Growing
it -- adversarial cases, real-crate snippets, interprocedural held-sets once
`whorl_lint` supports them -- is the actual, unglamorous work behind the moat.
