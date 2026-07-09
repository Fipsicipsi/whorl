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

Current status on this corpus: 12 cases, 11/12 match, 0 false negatives, 1 false
positive. The false positive is `outer_lock_serializes.rs`, the classic Havender
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
is a bug in one of them. Current status: 11/12 match ground truth, 0 false
negatives, 0 divergences between the two implementations; the single miss is
the same deliberate Havender false positive in both.

## The `lockbud` column (Direction 2: earn the claim)

The runner prints a `lockbud` column held at `TBD`. Filling it is the head-to-head
that turns "argued soundness" into "earned soundness":

1. Install Lockbud (`github.com/BurtonQin/lockbud`) -- it ships its own rustc
   driver and pins its own nightly; follow its README (do not mix its nightly
   with the dylint template's nightly used by `../whorl_lint`).
2. Run it on each case and record its verdict.
3. Compare. Lockbud is an unsound bug-finder by design, so the thesis to validate
   is: "Whorl flags what Lockbud misses (no false negatives), and Whorl's false
   positives are bounded and explainable." That table is the pitch and the paper.

A natural next step is to add the published Lockbud / Archerfish real-world bugs
as cases, and adversarial cases that target Whorl's own predicted false positives
(the strict class partial order), so the escape-hatch story stays honest.

## Caveats

This corpus is tiny and intra-procedural (the analyzer's current limit). Growing
it -- adversarial cases, real-crate snippets, interprocedural held-sets once
`whorl_lint` supports them -- is the actual, unglamorous work behind the moat.
