# embedded: critical-section deadlocks

The field trial (`../fieldtrial`) found the gap: real `no_std` embedded code has
no `std::sync::Mutex`. Its concurrency is built on a critical section --
`critical_section::with(|cs| ...)` or `cortex_m::interrupt::free(|cs| ...)` --
which masks interrupts, plus spinlocks. This is where Whorl's single-core story
actually lives. These cases run only through the typed dylint front-end (the
stable-MIR text PoC handles std locks only).

```sh
python run.py
```

## The model

A critical section is ONE global, reentrant lock, not one lock per
`critical_section::Mutex`. So the front-end does not treat each `Mutex` as a
class:

- Entering `with` / `free` is an acquisition of a single `<critical-section>`
  class. Its held region is the closure body, so the section is flowed into that
  body as a call edge (reusing the interprocedural machinery). A lock taken
  inside the section gets `<critical-section> < that-lock`.
- `Mutex::borrow(cs)` is NOT an acquisition -- it is a data access under the
  already-held section, returning `&T` with no guard. So two
  `critical_section::Mutex` values touched in opposite orders inside one section
  produce no ordering and stay SAFE (`cs_two_data_safe`).
- Nesting `with` inside `with` is reentrant on a single core, so the section
  never self-edges (`cs_nested_safe`). The reentrancy is enforced on the stable
  side, where the interprocedural widening drops `<critical-section>` from the
  held-set of a nested entry.

## What it catches

The real single-core hazard is a critical section and a spinlock taken in
inconsistent orders (`cs_spin_deadlock`):

```
<critical-section>  <  spin::Mutex   via cs_then_spin::{closure#0}   (spin taken inside the section)
spin::Mutex  <  <critical-section>   via spin_then_cs                (section entered holding the spin)
```

That cycle is a genuine deadlock: an interrupt can fire while a task holds the
spinlock, and the masked path then needs the spinlock the task will not release
until it can enter the section. Whorl reports it with witnesses on the real
source lines.

## Status

Four cases, 0 false negatives: one deadlock (the CS/spin inversion), three safe
(consistent order, nested reentrancy, and plain two-Mutex access under one
section). `cortex_m::interrupt::free` is matched the same way as
`critical_section::with` but is not yet exercised by a case here (it needs the
`cortex_m` dep and a thumb target to be realistic). The honest next step is a
real `no_std` HAL crate, which needs a cross-compilation target installed.
