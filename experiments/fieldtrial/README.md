# Field trial: the lint on real, third-party code

Toys prove features; a field trial proves the lint survives real code and says
something sensible. First target: `tracing-core` 0.1.36 (a foundational crate
with billions of downloads, ~6200 lines, real generics and trait impls, and two
`std::sync` locks).

## What was run

```sh
# copy the crate out of the local cargo cache, isolate it as its own workspace
cp -r ~/.cargo/registry/src/*/tracing-core-0.1.36 fieldtrial/tracing-core
printf '\n[workspace]\n' >> fieldtrial/tracing-core/Cargo.toml
cd fieldtrial/tracing-core
WHORL_EVENTS_OUT=$PWD/events.json cargo dylint --all --path ../../whorl_lint
whorl --events events.json
```

## Result

- **No ICE.** The lint walked every body in a 6200-line crate and exited 0. This
  is what the `DefKind` gate (see the corpus README) is for: real crates contain
  consts, statics, and shims that must be skipped, not crashed on.
- **274 call edges** were recorded, so the interprocedural machinery runs at real
  scale, not just on toys.
- **Two lock classes** were found and named from real types:
  `std::sync::Mutex<Vec<&dyn Callsite>>` (the callsite registry) and
  `std::sync::RwLock<Vec<Registrar>>` (the dispatcher registry).
- **Verdict: `[SAFE]`**, 0 ordering constraints. The two locks are never held at
  the same time, so no ordering edge exists and the graph is trivially acyclic.
  That matches reality: tracing-core is not known to have a lock-ordering
  deadlock. A clean crate reads clean.

Only four acquisition sites turned up in a big crate because tracing-core is
deliberately lock-light and because its own spin-mutex (`src/spin/mutex.rs`) is
compiled only in `no_std` builds; the default `std` build uses the two
`std::sync` locks. The value here is robustness, not a bug hunt: the lint runs
on real code without crashing and produces well-typed, correct output.

## The honest finding: the target domain needs a different matcher

The research thesis puts Whorl's wedge in **closed-world embedded** code. But
real embedded is `no_std`, and there `std::sync::Mutex` does not exist. The
dominant primitives are:

- `critical_section::Mutex<T>` and `cortex_m::interrupt::Mutex<T>` -- these do
  NOT hand out an RAII guard via `.lock()`. Access is `mutex.borrow(cs)` where
  `cs` is a `CriticalSection` token, returning a plain `&T`. There is no
  `MutexGuard` type and no `lock` call, so the current matcher
  (`is_lock_acquire` keys on `Mutex::lock`/`RwLock::read|write`;
  `guard_class_of_ty` keys on `*Guard` type names) sees zero acquisitions.
- `spin::Mutex` and `spin::RwLock` ARE recognized (the `inter` toy proves the
  pipeline catches a spin deadlock end to end).

So the pipeline is real and robust on `std`/`parking_lot`/`spin`. The
critical-section pattern is now supported too (see `../embedded`): entering
`critical_section::with` / `interrupt::free` is modelled as one reentrant
`<critical-section>` resource, flowed into the masked closure body, so a
spinlock taken inside a section and a section entered while holding that
spinlock form a detected cycle. This was a front-end change only; the solver
was untouched. What remains for a true field trial in the target domain is a
real `no_std` HAL crate, which needs a cross-compilation target installed.

## Next field-trial targets

- A crate with genuine nested locking, to exercise false-positive behavior on
  real code (tracing-core never nests, so it could not).
- A real `no_std` HAL/RTOS crate once `critical_section` is supported -- the
  first trial in the actual target domain.
