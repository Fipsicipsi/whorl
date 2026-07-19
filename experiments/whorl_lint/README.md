# whorl_lint -- Whorl MIR front-end

**Status: builds and runs end-to-end on the pinned nightly.** Verified on a
real crate: the classic two-account transfer produces the correct events
(second acquire with the first guard's class in the held-set), the stable
solver reports the deadlock with a witness pointing at the real source line,
and a consistently-ordered crate gets a `[SAFE]` verdict with a valid global
lock order. It remains an experiment: intra-procedural, a small lock-API
matcher, and a young test surface -- see limits below.

**This is NOT part of the stable `whorl` crate or its CI workspace.** It is a
separate cargo workspace that builds ONLY on a pinned Rust **nightly** with the
`rustc-dev` component and `cargo-dylint`.
The stable `whorl` crate is deliberately zero-dependency and builds on stable
Rust; this crate uses `#![feature(rustc_private)]` and the unstable compiler
internals, so it is intentionally kept out of the stable build and CI.

## What it does

It is a `dylint` `LateLintPass` that walks real Rust **MIR** and, for every lock
acquisition (`std`/`parking_lot`/`spin` `Mutex::lock`, `RwLock::read/write`, and
`critical_section::with` / `interrupt::free` as a single reentrant
`<critical-section>` resource -- see `../embedded`), emits a Whorl event:

- **`acquires`** -- the lock **CLASS** of the lock taken, recovered by
  canonicalizing the receiver `Place` (base type + field-path/static identity).
- **`held`** -- the set of lock classes whose RAII guards are still **live**
  (held) at that acquire, computed by a gen/kill liveness pass over the guard
  locals (gen at `Store`/`Call`, kill at `Drop`/`Move`/`StorageDead`).

These events are written as JSON (a `whorl::model::Program`) to the path in
`$WHORL_EVENTS_OUT` (default `./whorl-events.json`). The **existing**
`whorl::solver::analyze` (Havender cycle check over the inferred lock-class
partial order) then consumes them -- unchanged. This crate never links the
solver, so the nightly/rustc_private blast radius stays isolated.

The algorithm mirrors **lockbud** (github.com/BurtonQin/lockbud): type-based
guard identification + gen/kill guard liveness + receiver-Place lock identity.
Unlike lockbud (which uses points-to alias for *exact* same-lock identity and is
unsound by design), Whorl uses a **coarser, sound** field-path/static lock CLASS
and a **union** (may-held) join, so it over-approximates held-sets rather than
dropping edges.

## Build / run (Windows PowerShell)

```powershell
# one-time tooling (MSVC Build Tools / Windows SDK must already be installed --
# dylint-link wraps link.exe)
cargo install cargo-dylint dylint-link
rustup toolchain install nightly-2026-04-16-x86_64-pc-windows-msvc `
  --component rustc-dev --component llvm-tools-preview --component rust-src

# build the lint (rust-toolchain.toml auto-selects the pinned nightly)
cd whorl_lint
cargo build
cargo dylint list --path .            # confirm the dylib loads

# run it over a target crate; the lint writes whorl-events.json there
$env:WHORL_EVENTS_OUT = "$PWD\whorl-events.json"
cargo dylint whorl_lock_order --path <path-to-target-crate>

# feed the events to the stable solver (loader is gated behind a `mir-json`
# feature in the stable crate -- see Wiring, below)
cargo run -p whorl -- --events whorl-events.json

cargo test                            # ui_test fixtures (ui/*.rs + ui/*.stderr)
```

Compiled artifact on Windows:
`target/debug/whorl_lint@nightly-2026-04-16-x86_64-pc-windows-msvc.dll`
(no `lib` prefix, `.dll` suffix). If you hit `STATUS_DLL_NOT_FOUND` /
`0xc0000135`, the rustc_driver/std DLLs in the toolchain `bin` dir are not on
`PATH`; run via `cargo dylint` (it sets this up) rather than loading the DLL
directly. Keep the project path short to avoid `MAX_PATH` issues with the long
mangled DLL name.

## CO-VERSIONING HAZARD (read before bumping anything)

`dylint_linting` / `dylint_testing` (6.0.1), the `rust-toolchain` nightly date
(`nightly-2026-04-16`), and the `clippy_utils` git **rev**
(`f6d310692116e9a527ce6d0b3526c965d9c5d7b9`) are a **single co-versioned
bundle**. Change one and you MUST change all three, re-pulled from the
`trailofbits/dylint` template at the exact tag you install. Mixing versions
breaks compilation in confusing ways.

## Honesty: what is NOT verified to compile

This scaffold is "as-correct-as-possible, clearly-flagged", not
build-verified. `rustc_private` churns fast. Search `src/lib.rs` for
`// VERIFY:` -- each marks an API call whose exact shape on
`nightly-2026-04-16` was not independently confirmed and may need adjusting at
first `cargo build`:

- `tcx.def_path_str_with_args(adt.did(), args)` (renamed from
  `_with_substs`; arg position for parking_lot/lock_api protected-data type).
- `body.basic_blocks` as a field; `body.basic_blocks.predecessors()` shape.
- `Rvalue::Ref { region, borrow_kind, place }` field names/positions.
- `body.stmt_at(loc)` + the `Either` combinators in `span_to_site`.
- `static` base representation in `lock_class_of_receiver`.
- The `BasicBlock <-> usize` conversions in `solve_live` are written
  defensively but may need `bb.index()` / `BasicBlock::from_usize` instead.

The lock-method matcher (`is_lock_acquire`) currently uses fragile
`def_path_str` suffix matching so the scaffold has no hard `clippy_utils` call
site. **Production:** replace it with `clippy_utils::paths::PathLookup`:
`static MUTEX_LOCK: PathLookup = value_path!(std::sync::Mutex::lock);` then
`MUTEX_LOCK.matches(cx, callee_did)`. The old slice `match_def_path` API was
removed in clippy PR #14705; `PathLookup` is correct ONLY for the pinned rev.

## Wiring into the stable crate (no new stable deps)

Add a small `--events <file>` mode to the stable `whorl` binary behind an
optional `mir-json` feature: read the JSON, build a `model::Program`
(`Event { function, site, held, acquires }` + `class_instances`), and call the
existing `solver::analyze`. The JSON is intentionally trivial so it can be
hand-parsed without adding `serde` to the zero-dependency stable crate. The
lint and the solver thus stay in **separate** toolchains/workspaces, joined
only by this JSON file.
