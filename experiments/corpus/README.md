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

Current status on this corpus: 20 cases, 18/20 match, 0 false negatives, 1 false
positive, 1 fail-closed `[INCOMPLETE]`. `cross_call_deadlock.rs` was added as a deliberate red test: it proved both implementations had an interprocedural false negative
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
is a bug in one of them. Current status: 18/20 match ground truth, 0 false
negatives, 0 divergences between the two implementations; the two misses are the
deliberate Havender false positive and one fail-closed `[INCOMPLETE]` (the
two-path accessor, which genuinely cannot be resolved to a single lock). Where the PoC fails closed
(`[INCOMPLETE]`) and the lint resolves the case, that is recorded as
`ok (poc fail-closed)`, not a divergence -- one implementation being more
capable is fine, one being wrong is not.

## The `lockbud` head-to-head (`run_lockbud.py`) -- Direction 2, run

Lockbud (`github.com/BurtonQin/lockbud`) is the established static Rust deadlock
detector and an unsound bug-finder by design. `run_lockbud.py` runs it on every
case next to the Whorl verdict. Result on this corpus:

```
whorl false negatives: 0   | lockbud false negatives: 6 | lockbud false positives: 1
```

The whorl column is the real pipeline (dylint front-end into the solver), not
the text probe, so this is tool against tool.

Lockbud misses six of the labeled deadlocks, including
`two_account_deadlock.rs` -- the canonical textbook case -- plus
`branch_deadlock.rs`, `closure_call_deadlock.rs`, and all three class-split
cases added by the adversarial rounds. Several are the same systematic blind
spot: its
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

## The second adversarial round

The class-derivation rewrite was itself attacked, on the principle that the code
which just failed a review is the code most likely to fail the next one. Three
more false negatives came out, all of the same SPLIT shape, and all of them
paths that returned BEFORE reaching the fail-closed marker:

1. **The `Deref` shortcut applied to every `Deref` impl.** Rendering `deref(x)`
   as `(*x)` is right for `Arc`/`Rc`/`Box`/`Lazy`, where the deref yields the
   pointee. A user-defined `impl Deref for W { fn deref(&self) -> &Mutex<i32> {
   &self.inner } }` returns a FIELD, so the same mutex rendered as `&W.*` one
   way and `&S.*.a:W.inner:Mutex<i32>` the other. It is now an explicit
   allowlist of pointee-preserving wrappers; everything else falls through to
   fail-closed. The review also killed the obvious-looking alternative fix:
   MIR keeps the TRAIT item's DefId for trait-method calls, so looking
   `Deref::deref` up in the accessor summaries can never hit.
2. **The accessor summary's single-path guard scanned only statements.** A
   returned value routinely arrives from a TERMINATOR (a tail call), so a
   two-path accessor looked single-path and was summarized from the branch not
   taken -- relabelling one lock as another and severing the ring at that node.
   The guard is now total over every definition of the return chain.
3. **Class symbols embed rendered type text, and generic bodies are analyzed
   un-monomorphized.** A lock in `Pair<T>` renders `Mutex<T>` while the same
   lock seen from a concrete body renders `Mutex<i32>`: one lock, two symbols.
   Polymorphic symbols now fail closed, in both implementations.

`deref_field_split_deadlock.rs`, `two_path_accessor_deadlock.rs` and
`generic_class_split_deadlock.rs` are the regression tests. All three are real
AB/BA deadlocks that used to read `[SAFE]` and now fail closed.

The pattern across both rounds is worth stating plainly: every defect found so
far has been a SPLIT (one lock, two symbols) or a fail-closed test reading
pre-widening state. Merging is safe; splitting is not. That is the invariant to
attack first next time.

## Resolving what used to fail closed

Failing closed is correct but it is not free: three of eighteen verdicts were
non-conclusive. Two were then resolved properly rather than merely made safe.

- **Generic bodies.** Instead of refusing to render a polymorphic symbol, type
  ARGUMENTS are now erased: `Mutex<T>` from a generic body and `Mutex<i32>` from
  a concrete one both render `Mutex`, so they unify and the cycle closes. This
  MERGES rather than splits, which is the safe direction, and it matches what a
  lock class is supposed to mean -- the role a lock plays, not the payload it
  guards. `generic_class_split_deadlock.rs` is now correctly a DEADLOCK.
- **Trait-method accessors.** MIR keeps the TRAIT item's DefId, so a lock
  reached through a user's `Deref` impl could not be looked up. Callees are now
  resolved to the impl that will actually run, which makes the impl's accessor
  summary usable. `deref_field_split_deadlock.rs` is now correctly a DEADLOCK.
  The resolved impl must be a body we analyze; resolving to a foreign one would
  point a call edge at code we never see, so that still fails closed.

Getting the composition right took two tries, and both mistakes were splits.
Composing an accessor suffix onto its argument has to account for whether the
argument is already a reference (use the suffix whole) or a fresh borrow of the
place (drop the suffix's leading deref, which that borrow already supplied).
One deref too many or too few is a different symbol, and a different symbol is a
lock the cycle can never reach.

**Static identity** closed the precision gap that erasure exposed. A static lock
has a NAME, but the receiver only carries its TYPE, so two distinct statics of
the same type collapsed into one class -- and two statics taken in a consistent
order then read as a same-class self-edge, a false positive
(`two_statics_consistent_safe.rs`). The identity is now recovered from the
constant's allocation (`_t = const {alloc: &T}`), and every site that builds a
symbol goes through one static-aware renderer, so a lock reached one way cannot
carry the static's name while the same lock reached another way carries only its
type. `two_statics_inverted_deadlock.rs` guards the other direction: telling the
two apart must not lose the real cycle.

The diagnostics got much better as a side effect, because classes are now named
the way the source names them. In `tracing-core` the two registries separate
into `callsite::LOCKED_CALLSITES` and `callsite::dispatchers::LOCKED_DISPATCHERS`
instead of collapsing into one opaque `Lazy` type, and the embedded witness reads
`<critical-section> < BUS` rather than `&spin::mutex::Mutex<u32>.*`.

## Caveats

This corpus is tiny. Eighteen labeled cases beat zero, and the head-to-head
result is real, but it cannot carry a general soundness claim. Growing it --
the published Lockbud/Archerfish real-world bugs, real driver/HAL-shaped SAFE
code, locks behind closures and trait objects (the known remaining blind spot
of both Whorl implementations) -- is the actual, unglamorous work behind the
moat.
