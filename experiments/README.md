# Experiments

Research spikes toward the one thing that makes Whorl matter beyond its own toy
language: running the analysis over real Rust, with the soundness claim earned
rather than asserted.

None of this is part of the stable, zero-dependency `whorl` crate or its CI. The
shipped analyzer is still `../src`. These are spikes, clearly marked as such.

## Why this direction

A competitive review found that every individual selling point of Whorl is
already published or shipped on real code (Lockbud, RcChecker, Archerfish,
RacerD, Astree). They share one trait: they are unsound bug-finders. Whorl's only
defensible position is the opposite end: a sound (no-false-negative) prover for
the closed-world embedded residual. That position is worthless until two things
exist: a front-end that runs on real Rust, and evidence that the soundness claim
holds head to head. These three spikes are exactly those pieces.

## What is here

- **`whorl_lint/`** -- the production direction. A dylint `LateLintPass` that
  walks real Rust MIR (`optimized_mir`, post drop-elaboration), recovers the
  lock class from each `lock()` receiver place, computes the held-set from RAII
  guard liveness, and emits Whorl `Event` JSON that the stable solver consumes.
  WIP scaffold: builds only on a pinned nightly with `rustc-dev` +
  `cargo-dylint`, and is its own cargo workspace so it never touches stable CI.
  Unverified rustc-internal calls are marked `// VERIFY:`.

- **`mir-poc/`** -- a stable-Rust feasibility probe. It parses
  `rustc --emit=mir` text to extract, per acquisition, the lock class and the
  held-set, then runs the same Havender cycle check. No nightly, no
  `rustc_private`. It proves on stable, today, that the two make-or-break
  sub-problems (lock identity, held-set from guard liveness) are reachable from
  real Rust.

- **`corpus/`** -- a differential / ground-truth harness. Labeled Rust cases, a
  runner that compares the PoC verdict to the label, and a soundness metric (a
  case labeled DEADLOCK that the tool calls SAFE is a false negative -- the one
  thing a sound analyzer must never do). The `lockbud` column is the head-to-head
  that earns the soundness claim.

## Order of maturity

`mir-poc` runs here and now. `corpus` runs here and now. `whorl_lint` is the
real target and needs the nightly toolchain to build; the other two de-risk it
and give it a test oracle.
