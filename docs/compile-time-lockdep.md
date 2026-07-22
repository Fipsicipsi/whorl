# Compile-time lockdep for embedded Rust

Whorl is a static analyzer that proves a program cannot deadlock by lock
ordering. If it returns `[SAFE]`, no circular wait over locks is possible, on any
path, exercised or not. This note explains where that sits in a crowded field,
and shows the evidence behind the claim rather than asserting it.

It is a research prototype, not a product. Read the honest-limits section before
you trust a verdict.

## The one-sentence pitch

Linux `lockdep` is the canonical lock-ordering deadlock detector: it groups lock
instances into classes, tracks the order they are acquired in, and flags a cycle.
But it is a *runtime* tool. It only knows about orderings that actually executed;
an unexercised path is an unvalidated path.

Whorl is lockdep's model made static: the same lock-class dependency graph, but
proven over paths that never ran, at compile time, with the interrupt-masking
hazard folded into the same graph. Hence: compile-time lockdep.

## What already exists, and the gap

Every individual piece of this is prior art. Being honest about that is the point.

- **Linux lockdep, FreeBSD WITNESS** -- lock classes, a per-class order graph,
  cycle detection, interrupt-context awareness. Runtime.
- **Lockbud, RcChecker** -- static, MIR-based, points-to lock identity,
  guard-lifetime tracking, real bugs found in real crates. Unsound bug-finders
  by design (documented false negatives).
- **Archerfish** -- static interrupt-based deadlock detection in the Linux
  kernel, at scale. Precision-focused, roughly half its reports are false
  positives; not sound.
- **Clang Thread Safety Analysis** -- static and compile-time, but the lock order
  is annotated, not inferred, and the deadlock attribute is optional, so missing
  annotations are false negatives.

The four properties that no single tool combines: **static**, genuinely
**sound** (no false negatives), an **inferred** lock-*class* order (no
annotations in the common case), and **single-core interrupt / critical-section**
reasoning. That intersection is the only defensible position, and it is a narrow
one. Whorl aims at exactly it, for closed-world embedded code where "prove the
absence of deadlock" is the actual requirement.

## How it works, briefly

The guarantee rests on Havender's theorem: if every acquisition respects one
partial order over lock *classes*, no circular wait can form. So the analysis is:

1. At each acquisition of class `C` while holding class `H`, emit an edge
   `H < C`, witnessed by the source site.
2. Those edges form a graph over lock classes. There are few classes even in a
   large program (many instances, few roles).
3. Acyclic graph: a topological order exists, is a valid global lock order, and
   the program is deadlock-free. Cyclic: that cycle is the deadlock, reported as
   the chain of sites that close the ring.

Every function is treated as a possible concurrent entry point. This is
conservative and sound: Whorl may reject a safe program, but never accepts an
unsafe one. False positives are a cost; false negatives are a bug.

The front-end that produces those events from real Rust is a `dylint` lint over
optimized MIR: it recognizes lock acquisitions, recovers each lock's class from
the receiver place (base type plus field path), and computes the held-set from
RAII guard liveness. It emits the events as JSON; the solver, which never touches
compiler internals, reads that JSON. The critical-section extension models
`critical_section::with` / `cortex_m::interrupt::free` as one reentrant global
resource flowed into the masked closure body.

## Does it actually work? Three checks

The soundness claim is the whole value, and the literature shows soundness claims
are routinely false in practice. So the repository ships the checks, not just the
claim. All of these are reproducible under `experiments/`.

**1. Head to head against Lockbud.** On an 18-case labeled corpus, both tools run
end to end, next to Lockbud, the established static Rust deadlock detector:

```
whorl   false negatives: 0
lockbud false negatives: 6   (including the canonical two-account transfer deadlock)
```

Lockbud's DoubleLock detector needs the same lock twice; its ConflictLock
detector needs an inverted order between two *distinct named* locks. An unordered
acquisition of two *instances of one class* falls between them -- which is the
textbook deadlock, and exactly what a class-level abstraction is for. On the one
case Whorl reports as a false positive (a deliberate Havender case), Lockbud
reports the same false positive.

**2. Someone else's ground truth.** Run on the lock-ordering cases from Lockbud's
own `toys/` directory -- programs written by Lockbud's authors: **0 false
negatives** on all six labeled deadlocks, including a three-class cycle, an
interprocedural cycle that only closes through a call, a double-lock hidden
behind `.ok().unwrap()` on a match temporary, a `lazy_static` static, and an
inversion split across two thread closures.

**3. Real code, and the embedded hazard nobody else models.** The lint runs on
`tracing-core` (billions of downloads, ~6200 lines) without crashing, and returns
`[INCOMPLETE]` with a precise reason:

```
unresolved indirect call at src/callsite.rs:478 in callsite::Callsites::for_each
while holding ["&once_cell::sync::Lazy<Mutex<Vec<&dyn Callsite>>>.*"]
```

That is a true statement about real code, not a tool defect: tracing-core invokes
a caller-supplied callback while holding its callsite registry lock. Whatever
that callback locks would order against the registry lock, and Whorl cannot see
it, so it declines to certify the crate. An earlier version answered `[SAFE]`
here for the wrong reason -- an adversarial review showed the same physical lock
reached two different ways was being split into two class symbols that can never
close a cycle. That is fixed: simple field accessors are now resolved
interprocedurally, and a `Deref::deref` call renders exactly like a built-in
deref, so the smart-pointer route (`once_cell::Lazy` above) unifies instead of
splitting. What remains is the honest residue. On real
`no_std` code using the real `cortex-m` crate,
compiled for `thumbv7em-none-eabihf`, it catches the genuine single-core hazard:
a critical section and a bus spinlock taken in inconsistent orders.

```
[DEADLOCK]
  <critical-section>  <  spin::Mutex   via task_write::{closure#0}
  spin::Mutex  <  <critical-section>   via driver_flush
```

An interrupt fires while a driver holds the spinlock; the masked path then needs
that spinlock. No other static Rust tool models this, because it requires
treating interrupt masking as a resource in the same order graph as the locks.

## Honest limits

- The corpus is 18 hand-labeled cases plus Lockbud's toys. That is evidence, not
  a proof. A general soundness claim needs scale and, ideally, a mechanized core.
- Soundness is argued and tested, not mechanically verified. Development found
  and fixed seven real soundness bugs across the review passes; there may be more.
- Interprocedural held-sets follow direct calls, and callbacks are resolved by
  binding a closure to the parameter it is passed to (so a lock held across an
  invoked callback is seen). What is still unresolved -- calls through function
  pointers and trait objects -- does not silently pass: an unresolved indirect
  call made while a lock is held forces `[INCOMPLETE]` rather than `[SAFE]`.
- The class abstraction merges instances a points-to analysis would separate, so
  two distinct same-type locals locked in a consistent order read as a false
  positive. That is the Havender trade: soundness over precision.
- A lock reached through a call is resolved when that call is a simple field
  accessor (a body returning a reference to a field of a parameter) or a
  `Deref`; anything more involved has no canonical class path and forces
  `[INCOMPLETE]` rather than risking a split class. The diagnostic names the
  call, so the limitation is actionable rather than mysterious.
- `[SAFE]` means no lock-ordering deadlock. Condition-variable lost wakeups,
  channel and actor cycles, external resources, and multicore preemption are out
  of scope and say nothing about them.

## Where this goes

The realistic ceiling is not a category-defining tool; it is the rigorous
embedded-concurrency analyzer that the sound end of the field left unbuilt, for
the closed-world firmware -- hand-rolled critical sections and driver locks in
HALs, BSPs, and kernels -- that the modern deadlock-free frameworks (RTIC,
Embassy, Tock) do not cover.

The code, the corpus, and the reproduction scripts are in the repository. The
fastest way to disagree with any claim here is to run `experiments/` and file the
counterexample.
