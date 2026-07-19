# Whorl

A static analyzer that proves freedom from lock-ordering deadlocks.

Whorl's claim: if a program passes the analysis, it cannot deadlock by lock
ordering. The lock hierarchy is inferred, so the common case needs no
annotations, and the guarantee is fully static with no runtime cost.

## Status

Whorl is a research prototype. The stable crate here is its own small language
plus the solver. A separate, experimental front-end under
[experiments/](experiments/) now runs the same analysis on **real Rust** via a
`dylint` MIR pass: it is validated head to head against Lockbud (0 vs 2 false
negatives on a labeled corpus), runs clean on `tracing-core`, and catches a
single-core critical-section-vs-spinlock deadlock on real `cortex-m` `no_std`
code. See [docs/compile-time-lockdep.md](docs/compile-time-lockdep.md) for what
that is and the evidence behind it.

A `[SAFE]` verdict means "no lock-ordering deadlock", not "cannot deadlock":
condition variables, channel and actor cycles, external resources, and multicore
preemption are out of scope. The soundness guarantee is argued and tested, not
yet mechanically proven (development found and fixed 7 soundness bugs across 8
review passes, and more in the front-end since). Read [SECURITY.md](SECURITY.md)
before relying on a verdict, and [docs/ROADMAP.md](docs/ROADMAP.md) for the path
from prototype to product.

## What it does

You write code in Whorl's small language. It tracks which lock classes are held
at each acquisition, including across function calls and through function-valued
parameters (callbacks), builds the class-ordering graph, and reports either a
valid global lock order or a witnessed cycle pointing at the source lines.
Callback targets are resolved automatically. It covers two deadlock classes:
lock ordering, and single-core interrupt preemption (`isr` and `mask`). The
analysis is sound and runs in polynomial time.

```text
lock alice : Account
lock bob   : Account

fn transfer() {
    with alice {
        with bob { }      // acquire an Account while holding an Account
    }
}
```

```
$ whorl examples/two_account_transfer.whorl
   [DEADLOCK]  a lock-ordering cycle exists -- a circular wait is possible
     Account  <  Account   via transfer @ two_account_transfer.whorl:9
   fix: acquire same-class locks together with `with ordered(a, b) { .. }`.
```

Two examples to look at: [examples/cross_call.whorl](examples/cross_call.whorl),
a deadlock invisible in any single function that is caught because the held-set
flows into the callee; and
[examples/callback_deadlock.whorl](examples/callback_deadlock.whorl), a deadlock
that flows through a callback, caught because Whorl resolves the concrete
callback target.

## How it works

The guarantee rests on Havender's theorem: if every acquisition respects a single
partial order over lock classes, no circular wait can form. The core is small and
decidable:

1. Each acquisition of class `C` while holding class `H` emits an ordering edge
   `H < C`, witnessed by the source site.
2. The edges form a directed graph over lock classes. There are few classes even
   in a large codebase (many lock instances, few roles).
3. If the graph is acyclic, a topological order exists; that order is a valid
   global lock order, and the program is deadlock-free.
4. If the graph has a cycle, that cycle is a lock-ordering inversion, reported as
   the chain of sites that order the classes into a ring.

Every function is treated as a possible concurrent entry point. This is
conservative and sound: Whorl may reject a safe program (a false positive) but
never accepts an unsafe one.

Whorl has no dependencies. The lexer, parser, held-set propagation, topological
sort (Kahn) and cycle extraction (DFS) are hand-written in pure `std`, so it
builds offline on any platform.

## Build and run

```sh
cargo test
cargo run -- examples/cross_call.whorl
cargo run -- examples/bank_ok.whorl examples/two_account_transfer.whorl
cargo run -- --dot examples/isr_preemption.whorl | dot -Tsvg > graph.svg
```

`--dot` emits the lock-order graph as Graphviz DOT with the deadlock cycle's
edges in red. The exit code is non-zero when any input contains a potential
deadlock, so `whorl` works as a CI gate.

## The language

```text
lock <name> : <Class>                          // a named lock and its class
fn <name>(<param>, ..) { <stmt>* }             // a function; params are callbacks
isr fn <name>() { <stmt>* }                    // an interrupt handler
extern fn <name> [acquires <Class>, ..]        // a foreign function and its
                                               // lock contract (FFI boundary)

with <lock> { <stmt>* }                        // hold one lock for the block
with ordered(<lock>, <lock>, ..) { <stmt>* }   // acquire same-class locks safely
couple <Class> { <stmt>* }                     // hand-over-hand traversal (lock
                                               // coupling)
mask { <stmt>* }                               // interrupts disabled in the block
<callee>(<arg>, ..)                            // call; held-set propagates,
                                               // including through callbacks
```

## Scope and limitations

Whorl proves freedom from lock-ordering (circular-wait) deadlocks, and from
single-core interrupt-preemption deadlocks when `isr` and `mask` are used.
Deadlocks from condition-variable lost wakeups, channel or actor cycles, and
external resources are out of scope; a `[SAFE]` verdict says nothing about them.
`couple`, `extern ... acquires`, and `mask` are trusted assertions made by the
author.

The analysis runs in polynomial time and gives a definitive verdict. Two
potential blow-ups are handled. Callbacks are resolved by a context-insensitive
0-CFA pre-pass, so there are no per-call-site contexts. Held-sets are summarized
as a monotone may-be-held union per function instead of being enumerated as exact
subsets, so there are no `2^classes` contexts. The held-set summary is exact at
the edge level: every ordering edge `h < C` is pairwise, so the union of edges
over all entry contexts equals the edges computed from the union of held-sets,
with no loss of precision. The one residual imprecision is the 0-CFA callback
merge, which can produce a false positive (a `[DEADLOCK]` a fully
context-sensitive analysis would call safe). It is sound: over-approximating
callback targets only adds edges and can never hide a real deadlock. The
`[INCOMPLETE]` verdict is a defensive backstop, unreachable in practice (a
program with 2^60 distinct held-sets resolves in about 0.2 seconds).

## Implemented features

- Core solver: events to a proof or a witnessed cycle.
- A `with lock { }` language with held-sets propagated through direct calls.
- Held-sets propagated through callbacks, with callback targets resolved
  automatically.
- `couple` for hand-over-hand (B-tree style) fine-grained locking. The
  same-class self-edge is suppressed only for a strictly monotone descent in
  lock-declaration order, so all couples must agree on one canonical direction;
  opposite-direction couples and single-instance re-acquisition are still caught.
  Cross-class and external-hold edges are always enforced.
- `extern fn` lock contracts folded into the whole-program check. A contract with
  no `acquires` is an explicit, audited promise of lock-freedom.
- 0-CFA callback flow and a may-be-held summary fixpoint for polynomial scaling.
- `isr fn` and `mask { }` for single-core interrupt-preemption deadlocks, modeled
  as a wait-for cycle through a synthetic CPU resource. Run-to-completion model;
  nested interrupts are out of scope. The machinery is inert unless the program
  declares an interrupt handler.

Future work, and the path to a tool that analyzes real codebases, are described
in [docs/ROADMAP.md](docs/ROADMAP.md).

## Author

Built by felix ([@fipsicipsi](https://github.com/fipsicipsi)). Contributions are
welcome; see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at
your option.
