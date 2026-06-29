# Contributing to Whorl

Thanks for your interest. Whorl is a small, soundness-first project; a few
conventions keep it that way.

## Build & check

```sh
cargo test                              # all unit tests
cargo fmt --all --check                 # formatting (CI enforces)
cargo clippy --all-targets -- -D warnings   # lints (CI enforces, zero warnings)
cargo run -- examples/cross_call.whorl  # try it
cargo run -- --dot examples/isr_preemption.whorl   # visualize the graph
```

CI runs exactly these. A PR should be `fmt`-clean, `clippy`-clean, and green.

## Principles

- **Soundness is the spine.** Whorl may report a false positive (a `[DEADLOCK]`
  that is actually safe) but must **never** report a false negative (a `[SAFE]`
  that is actually a deadlock). Any change that could weaken this needs a strong
  argument and tests for the boundary.
- **Adversarially review new analysis.** Every analysis or suppression mechanism
  in this project (`ordered`, `couple`, `extern`, `isr`/`mask`, the 0-CFA and
  may-held passes) shipped a soundness bug that was only caught by trying hard to
  break it. If you add or change one, include tests that *attempt to construct a
  missed deadlock*, not just happy-path cases.
- **Zero runtime dependencies.** The graph, fixpoints, parser, and solver are
  hand-written `std`-only on purpose. Please keep it that way.
- **Be honest about scope.** Whorl proves freedom from lock-ordering and
  single-core ISR-preemption deadlocks only. Don't let docs or messages imply
  more.

## Adding a test or example

- Unit tests live next to the code (`#[cfg(test)] mod tests`). Prefer a small
  source string asserting `Outcome::Deadlock` / `DeadlockFree`, and add a
  *negative* counterpart when you fix a soundness bug.
- Examples are `examples/*.whorl` and double as documentation; keep them small
  and commented, and make sure their verdict is what you intend.

## Scope of changes

The honest roadmap (and what is deliberately *out* of scope) lives in
[`docs/ROADMAP.md`](docs/ROADMAP.md). Large new directions are best discussed in
an issue first.
