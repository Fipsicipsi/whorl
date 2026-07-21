# mir-poc: real Rust to a Whorl verdict, on stable

A feasibility probe. It takes real Rust that uses `std::sync::Mutex` / `RwLock`,
runs it through stable `rustc --emit=mir`, and extracts the only two things
Whorl's solver needs from each lock acquisition:

1. the lock **class** -- from the `lock()` receiver place
   (`&((*_1).0: Mutex<i64>)` becomes the field-path class `Mutex<i64>::field0`);
2. the **held-set** -- the guards still live at this acquire, computed from RAII
   guard liveness via the `drop(_guard)` terminators.

It then runs the same Havender cycle check as `whorl::solver` and prints a
verdict. No nightly, no `rustc_private`.

## Run

```sh
rustc --emit=mir --crate-type=lib demo.rs -o demo.mir
python poc.py demo.mir
```

Or all of the bundled cases at once:

```sh
for f in demo demo2 safe cycle; do rustc --emit=mir --crate-type=lib $f.rs -o $f.mir; done
python poc.py demo.mir demo2.mir safe.mir cycle.mir
```

## What it demonstrates

`demo.rs` has two functions that lock the **same** class. They get **opposite**
verdicts:

- `transfer` holds the first guard across the second `lock()` -> held-set is
  non-empty -> DEADLOCK.
- `sequential` drops the first guard before the second `lock()` -> held-set is
  empty -> SAFE.

Same class, different verdict, decided purely by guard liveness. That is the hard
sub-problem, working on real Rust. `demo2.rs` adds a branch (`if`) and an
`RwLock`; `safe.rs` and `cycle.rs` show a clean whole-program SAFE and a true
cross-class cycle.

## How it works

- Lock acquisitions, the receiver place, the unwrap-to-guard, and guard drops are
  read from the MIR.
- The held-set is a CFG **may-be-held** dataflow: a forward fixpoint with a union
  join over predecessor blocks, so branches and loops are handled soundly (a
  guard counts as held at an acquire if it is live on any path reaching it).
- The verdict is a self-loop check (same class held then re-acquired) plus a DFS
  cycle search over the cross-class ordering graph.

## Honest limits (these are why the production path is `../whorl_lint`)

- **Interprocedural via direct calls only.** Calls to named local functions
  feed an entry-may fixpoint (a guard held at a call site counts as held
  throughout the callee, transitively), so cross-call deadlocks are caught.
  Calls through closures, function pointers and trait objects are NOT resolved
  here. Rather than silently miss the edges, the probe FAILS CLOSED: an indirect
  call made while a lock is held yields `[INCOMPLETE]`, never `[SAFE]`. The
  typed dylint pass does resolve the common case by binding closures to the
  callback parameters they are passed to.
- **Move tracking is minimal.** Plain `_a = move _b` statements transfer guard
  ownership (so `std::mem::drop` releases correctly), but a guard moved into a
  struct or returned is conservatively kept in the held-set (sound, may
  over-report).
- **Positional field names.** Classes are `field0`, not `bal`; the MIR text does
  not carry source field names. The typed dylint pass recovers real names.
- **Text parsing is a shortcut.** Robust extraction wants in-memory typed MIR
  (the dylint pass), not regexes over a debug dump.
- **`RwLock` read and write are treated as exclusive** (sound, may over-report).
- Tested on `std::sync` locks; `parking_lot` is recognized by pattern only.
