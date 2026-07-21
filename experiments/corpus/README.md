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

Current status on this corpus: 14 cases, 13/14 match, 0 false negatives, 1 false
positive. `cross_call_deadlock.rs` was added as a deliberate red test: it proved both implementations had an interprocedural false negative
(a guard held across a call was invisible in the callee), which was then fixed
by call-edge tracking plus an entry-may fixpoint -- in the lint/solver pipeline
and in the PoC alike. `closure_call_deadlock.rs` is the same story one level
deeper: the ordering edge exists only inside a closure invoked through a
callback parameter, which both implementations missed. The dylint front-end now
resolves it by binding closures to the parameters they are passed to; the
text PoC cannot, so it fails closed with `[INCOMPLETE]` instead of a wrong SAFE. The false positive is `outer_lock_serializes.rs`, the classic Havender
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
is a bug in one of them. Current status: 13/14 match ground truth, 0 false
negatives, 0 divergences between the two implementations; the single miss is
the same deliberate Havender false positive in both. Where the PoC fails closed
(`[INCOMPLETE]`) and the lint resolves the case, that is recorded as
`ok (poc fail-closed)`, not a divergence -- one implementation being more
capable is fine, one being wrong is not.

## The `lockbud` head-to-head (`run_lockbud.py`) -- Direction 2, run

Lockbud (`github.com/BurtonQin/lockbud`) is the established static Rust deadlock
detector and an unsound bug-finder by design. `run_lockbud.py` runs it on every
case next to the Whorl verdict. Result on this corpus:

```
whorl false negatives: 0   | lockbud false negatives: 3 | lockbud false positives: 1
```

The whorl column is the real pipeline (dylint front-end into the solver), not
the text probe, so this is tool against tool.

Lockbud misses `two_account_deadlock.rs` -- the canonical textbook deadlock --
plus `branch_deadlock.rs` and `closure_call_deadlock.rs`. Two of the three are
the same systematic blind spot: its
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

## Lockbud's own toys (`run_toys.py`)

The strongest test is someone else's ground truth. `run_toys.py` runs the Whorl
pipeline on the lock-ordering cases from lockbud's own `toys/` directory --
programs written by lockbud's authors, not us. Result:

```
lockbud-toys | whorl false negatives: 0 | false positives: 2
```

Whorl catches all six labeled deadlocks, including ones that exercise features
beyond the hand-written corpus: a three-class cycle (`conflict`), an
interprocedural cycle that only closes through a call (`conflict-inter`,
via the call-edge fixpoint), a double-lock hidden behind `.ok().unwrap()` on a
match temporary (`intra`), a `lazy_static!` static (`static-ref`), and an
inversion split across two `thread::spawn` closures (`lock-closure`, caught
because closures are separate MIR bodies).

`static-ref` first ICEd the lint: `lazy_static!` emits `static`/`const` items,
and `optimized_mir` is the wrong query for those (they use `mir_for_ctfe`).
Fixed by gating body iteration on `DefKind::{Fn, AssocFn, Closure}` -- the
standard robustness guard. An analyzer must never crash on real input; at worst
it reports `[INCOMPLETE]`.

The two false positives are both the sound-over-precise trade, the same family
as the Havender case above:

- `call-no-deadlock`: two distinct local `Mutex<i32>` values. With no field path
  to tell them apart, the type-based class abstraction merges them into one
  class, so locking both nested reads as a same-class self-edge. Distinguishing
  the instances needs points-to/alias analysis (what lockbud does); Whorl
  deliberately trades that away for soundness.
- `recursive-no-deadlock`: `read()` then `read_recursive()` on one parking_lot
  `RwLock`. `read_recursive` is safe by construction, but Whorl does not model
  read/write/recursive semantics and treats every acquire as exclusive -- the
  conservative direction (a plain `read()` while holding a `read()` on
  `std::sync::RwLock` genuinely can deadlock against a waiting writer).

Reproducing needs a lockbud checkout (`LOCKBUD_SRC`, default `../../../lockbud-src`)
for the toy sources; the toys pull `parking_lot`/`spin`/`lazy_static` from
crates.io on first run.

## The adversarial review round

A multi-agent review was pointed at the callback-resolution work with one
instruction: construct programs where Whorl says SAFE but a deadlock exists. It
confirmed eight defects, and the verdict was blunt -- "Whorl is NOT sound
today". Two root causes:

1. **Class derivation was splitting and renaming locks.** A lock reached through
   an accessor got a different class symbol than the same lock reached through
   its field path, and a guard whose class could not be linked was given a
   fabricated `@unlinked:N` name. Both were commented in the source as
   conservative. They are the opposite: coarsening a class is sound, splitting
   or renaming one is not, because an invented node can never close a cycle.
2. **Every fail-closed test read pre-widening, intraprocedural state.** The
   `incomplete` field existed and was never assigned anywhere in the lint. The
   emptiness tests ignored the entry-may set the same pass had just computed, so
   a callback invoked under a *caller's* lock never tripped them; the front end
   deleted opaque calls whose local held-set was empty before the stable side
   could widen them; the resolution test was existential, so one benign closure
   whitewashed a whole parameter position; and a critical section whose closure
   could not be named was dropped wholesale.

All eight are fixed. The rule that came out of it: **the front end must not
decide whether a lock is held** -- only the stable side, after the
interprocedural fixpoint, can. `accessor_class_split_deadlock.rs` is the
regression test for root cause 1.

The cost was real and is visible in the field trial: `tracing-core` moved from
`[SAFE]` to `[INCOMPLETE]`, because it reaches a lock through an accessor. That
is the correct direction -- the old `[SAFE]` was not justified -- and it names
the next precision problem to solve.

## Caveats

This corpus is tiny. Fourteen labeled cases beat zero, and the head-to-head
result is real, but it cannot carry a general soundness claim. Growing it --
the published Lockbud/Archerfish real-world bugs, real driver/HAL-shaped SAFE
code, locks behind closures and trait objects (the known remaining blind spot
of both Whorl implementations) -- is the actual, unglamorous work behind the
moat.
