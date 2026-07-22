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
- **Verdict: `[INCOMPLETE]`**, with a precise reason:

  ```
  unresolved indirect call at src/callsite.rs:478 in callsite::Callsites::for_each
  while holding ["&once_cell::sync::Lazy<Mutex<Vec<&dyn Callsite>>>.*"]
  ```

  tracing-core invokes a caller-supplied callback while holding its callsite
  registry lock. Whatever that callback locks orders against the registry lock,
  and Whorl cannot see it, so it declines to certify the crate. That is a true
  statement about the code, not a tool defect.

  An earlier version answered `[SAFE]` here, for the wrong reason: an adversarial
  review proved the class derivation was SPLITTING one physical lock into two
  symbols depending on how it was reached, and two symbols can never close a
  cycle. That is fixed -- simple field accessors are resolved interprocedurally
  and `Deref::deref` renders like a built-in deref, so the `once_cell::Lazy`
  route above unifies rather than splitting. What is left is the honest residue.

Only four acquisition sites turned up in a big crate because tracing-core is
deliberately lock-light and because its own spin-mutex (`src/spin/mutex.rs`) is
compiled only in `no_std` builds; the default `std` build uses the two
`std::sync` locks. The value here is robustness, not a bug hunt: the lint runs
on real code without crashing, and it now says exactly as much as it can justify.

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

## Second trial: real `no_std`, the real `cortex-m` crate, a real target

The follow-up ran in the actual target domain. A `#![no_std]` crate depending on
the real `cortex-m` crate (0.7), compiled for `thumbv7em-none-eabihf` (the target
installed with `rustup target add`), using `cortex_m::interrupt::free` and
`cortex_m::interrupt::Mutex` -- the genuine embedded primitives, not a mock:

```rust
static SHARED: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));   // cortex_m interrupt Mutex
static BUS: spin::Mutex<u32> = spin::Mutex::new(0);
pub fn task_write()   { interrupt::free(|cs| { let b = BUS.lock(); SHARED.borrow(cs).set(*b); }); }
pub fn driver_flush() { let b = BUS.lock(); interrupt::free(|cs| { SHARED.borrow(cs).set(*b); }); }
```

The lint compiled the crate for the thumb target without an ICE, recognized the
real `cortex_m::interrupt::free` as a critical-section entry, flowed the section
into the closure body, and reported the inversion:

```
[DEADLOCK]
  <critical-section>  <  spin::Mutex   via task_write::{closure#0}
  spin::Mutex  <  <critical-section>   via driver_flush
```

That is the actual embedded hazard -- an interrupt firing while a driver holds
the bus spinlock, where the masked path then needs that same spinlock -- caught
on real no_std code, for a real embedded target, using the real cortex-m API.
`cargo dylint --all --path ../whorl_lint -- --target thumbv7em-none-eabihf`.

## Next field-trial targets

- A crate with genuine nested `std` locking, to exercise false-positive behavior
  on real code (tracing-core never nests, so it could not).
- Making a full HAL/PAC (nrf/stm32/rp2040) the analyzed crate rather than a
  dependency, to run the lint over thousands of lines of generated no_std code.
