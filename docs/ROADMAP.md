# Roadmap & honest scope

Whorl is a **complete research prototype** of a sound deadlock analysis. This
document is honest about what it is, what it is not, and what a real product
would require - so nobody mistakes the prototype for a production tool.

## What Whorl is today

A zero-dependency Rust program (~1700 lines) that proves freedom from two
deadlock classes - **lock-ordering** and **single-core interrupt-preemption** -
on programs written in **its own small language**. The genuinely reusable,
finished asset is the language-agnostic solver (`src/solver.rs`): it consumes a
list of lock-acquisition events and returns a topological lock order or a
witnessed cycle. Every analysis phase was hardened by adversarial multi-agent
review (7 real soundness bugs found and fixed across 8 reviews).

## The honest gap to a product

The hard truth: **a standalone language nobody writes production code in cannot
be the product.** The toy language also quietly *hides* the two hardest
problems by handing the solver, for free, what real code never provides:

1. **Lock classes.** The analysis is class-level; in the toy language a human
   writes `lock x : Class`. Real code has lock *instances* and no classes.
   Deriving a class partition from real `Mutex` instances (by field path /
   static identity, with overrides) is research-grade: too coarse floods false
   positives, too fine can never prove the cross-instance deadlock.
2. **Held-sets.** Here they come from lexical `with { }` nesting. Real Rust
   held-ness is RAII guard *liveness* (NLL/drop scopes, early `drop`, guards in
   structs or across `.await`) and must be computed from MIR dataflow; C has no
   RAII at all.

So the path to a product is **not polish - it is building the entire front-half
the prototype was designed to avoid**, and *earning* the soundness claim
(currently argued, not mechanically proven).

## Prioritized path (if pursued)

The single highest-leverage move: **retarget the existing solver as a
`dylint`/MIR analysis pass over `no_std` embedded Rust**, with lock-class
derivation as the make-or-break sub-problem. Prove it on one real firmware crate
before building anything else.

| Phase | Goal | Effort |
|---|---|---|
| **P0** | Stop investing in the `.whorl` frontend; validate the buyer (embedded/RTOS interviews); build an independent corpus of real deadlocks (lockdep splats, ABBA CVEs) with labeled verdicts | weeks |
| **P1** *(the gate)* | A MIR pass: recognize lock APIs, derive classes (field-path/static + override), compute held-sets from guard liveness - feed the existing solver, on one real crate | months |
| **P2** | *Earn* soundness: differential-test vs ThreadSanitizer/Loom/lockdep; stress/mechanize the "exact at the edge level" may-held claim; publish a measured recall number and a hard applicability envelope | months |
| **P3** | Product ergonomics: SARIF, stable error codes, per-site suppression, baseline/ratchet, CI action, and the cheap completeness must-haves - **`try_lock` first**, plus a reentrant flag and RwLock modes | months |
| **P4** | Design partners, then a DO-178C / ISO 26262 tool-qualification kit (needs a safety/cert specialist) - only after product-market fit | multi-year |

## Explicitly out of scope (scope traps)

- **The `.whorl` standalone language** - keep `solver.rs`, discard the frontend.
- **C/C++** - bigger market, far harder (raw handles, macros, no RAII); defer.
- **async/await, condition variables, channel cycles** - different deadlock
  classes Havender does not cover. A green verdict is **not** "cannot deadlock".
- **General cloud/desktop Rust** - false-positive rate on idiomatic open-world
  code is unwinnable for years, and free lockdep + TSan are good enough there.
- **Certification work before validated design partners.**

## Realistic outcome

Always a legitimate open-source contribution (the solver + the Havender
reduction). Plausibly a sound deadlock lint for `no_std` embedded Rust that a
handful of firmware teams keep in CI. And - only if those partners stick - an
open-core business whose moat is being a *qualified* component of a certified
toolchain. It is **not** clippy-for-everyone, not a general concurrency tool,
and not a solo, short-horizon effort.
