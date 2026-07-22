//! Whorl MIR front-end as a dylint LateLintPass.
//!
//! WIP SCAFFOLD. Builds ONLY on the pinned nightly with rustc-dev + cargo-dylint.
//! It is NOT part of the stable, zero-dependency `whorl` crate or its CI.
//!
//! Output: a `Program`-shaped JSON file (path from $WHORL_EVENTS_OUT, default
//! ./whorl-events.json) whose `events` are exactly `whorl::model::Event`
//! { function, site, held, acquires }. The stable `whorl` binary loads that JSON
//! and runs the existing Havender solver. This crate never links the solver, so
//! the nightly/rustc_private blast radius stays isolated from stable CI.
//!
//! Algorithm mirrors lockbud (github.com/BurtonQin/lockbud): type-based guard
//! identification + a gen/kill MIR Visitor for guard liveness + receiver-Place
//! lock identity. Lockbud's source is pinned to an OLD nightly; every concrete
//! signature below is re-derived for nightly-2026-04-16 and the ones the
//! verifier could not confirm on THIS exact nightly are marked // VERIFY:.
#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_data_structures;
extern crate rustc_hir;
extern crate rustc_index;
extern crate rustc_middle;
extern crate rustc_span;
// rustc_driver is declared by the declare_late_lint! expansion below.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use rustc_hir::def::DefKind;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::mir::visit::{
    MutatingUseContext, NonMutatingUseContext, PlaceContext, Visitor,
};
use rustc_middle::mir::{
    Body, Local, Location, Operand, Place, PlaceElem, Rvalue, StatementKind, TerminatorKind,
};
use rustc_middle::ty::{self, TyCtxt};

// declare_late_lint! auto-generates dylint_library!, register_lints, the
// declare_lint! item, and the pass struct (named `WhorlLockOrder`). This is the
// VERBATIM official-template path. The research's hand-written
// `dylint_linting::dylint_library!()` + `#[unsafe(no_mangle)] register_lints`
// using `register_late_pass(|_| ...)` was NOT used: the raw rustc_lint LintStore
// exposes `register_late_lint_pass(|tcx| ...)` (verified on the pinned nightly),
// so the |_| form is the dylint helper, not the rustc API -- using the macro
// removes that ambiguity.
dylint_linting::declare_late_lint! {
    /// ### What it does
    /// Emits per-acquisition (lock-class, held-set) events for Whorl's Havender
    /// solver; flags acquisitions involved in a lock-ordering cycle.
    ///
    /// ### Why is this bad?
    /// Inconsistent global lock order across threads can produce a circular wait
    /// (lock-ordering deadlock).
    pub WHORL_LOCK_ORDER,
    Warn,
    "potential lock-ordering deadlock (Havender cycle over inferred lock classes)"
}

// ---- The Whorl IR we emit (a structural copy of whorl::model). We DO NOT depend
// ---- on the whorl crate (it is stable/zero-dep and would drag this nightly crate
// ---- into its build). We hand-serialize to JSON the stable binary can read.
#[derive(Default)]
struct Event {
    function: String,
    site: String,
    held: BTreeSet<String>,
    acquires: String,
}
/// A call from `function` to a local `callee` with `held` guard classes live at
/// the call site. The stable side folds these into entry-may sets so a guard
/// held across a call is seen inside the callee (interprocedural soundness).
struct CallEdge {
    function: String,
    callee: String,
    held: BTreeSet<String>,
}
/// `function` invokes its own parameter #`param` (an indirect call on a
/// callable it received) while holding `held`. Which body that runs is decided
/// by whoever passed the callable in -- see `ClosureArg`.
struct ParamCall {
    function: String,
    param: usize,
    held: BTreeSet<String>,
}
/// `function` passes `closure` as argument #`param` to `callee`. Joined with a
/// matching `ParamCall` on the stable side, this resolves the indirect call.
struct ClosureArg {
    function: String,
    callee: String,
    param: usize,
    closure: String,
}
/// An indirect call whose callee cannot be resolved at all (fn pointer, trait
/// object) made while holding locks. Ordering edges are lost here, so the
/// analysis must not claim SAFE: this forces `[INCOMPLETE]`.
struct OpaqueCall {
    function: String,
    site: String,
    held: BTreeSet<String>,
}
#[derive(Default)]
struct ProgramOut {
    events: Vec<Event>,
    calls: Vec<CallEdge>,
    param_calls: Vec<ParamCall>,
    closure_args: Vec<ClosureArg>,
    opaque_calls: Vec<OpaqueCall>,
    // class symbol -> set of distinct receiver-base identities seen for it.
    // len() >= 2 => the class has >=2 instances (cross-instance inversion is
    // possible); len() == 1 => single-instance (reentrancy). Feeds
    // Program.class_instances, which the solver reads.
    class_instances: BTreeMap<String, BTreeSet<String>>,
    incomplete: Option<String>,
}

// declare_late_lint! generates `struct WhorlLockOrder;` (a unit struct), so we
// cannot add fields to it. Accumulate into a thread-local instead.
thread_local! {
    static PROGRAM: RefCell<ProgramOut> = RefCell::new(ProgramOut::default());
}

/// Record that the event list is missing information. A deadlock found anyway
/// is still real, but the ABSENCE of one stops being conclusive. First reason
/// wins; it is a verdict qualifier, not a diagnostic list.
fn mark_incomplete(reason: String) {
    PROGRAM.with(|p| {
        let mut p = p.borrow_mut();
        if p.incomplete.is_none() {
            p.incomplete = Some(reason);
        }
    });
}

impl<'tcx> LateLintPass<'tcx> for WhorlLockOrder {
    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        let tcx = cx.tcx;
        // Pre-pass: summarize simple field accessors, so a lock reached through
        // one renders to the SAME class as the same lock reached by field path.
        // This must run before any body is analyzed, since callers need it.
        let mut accessors: HashMap<DefId, (usize, String, String)> = HashMap::new();
        for &ldid in tcx.mir_keys(()) {
            let did = ldid.to_def_id();
            if !matches!(tcx.def_kind(did), DefKind::Fn | DefKind::AssocFn) {
                continue;
            }
            if !tcx.is_mir_available(did) {
                continue;
            }
            if let Some(summary) = accessor_summary(tcx.optimized_mir(did)) {
                if std::env::var("WHORL_DEBUG").is_ok() {
                    eprintln!(
                        "whorl-debug: field accessor {} => param #{}, path {}",
                        tcx.def_path_str(did),
                        summary.0,
                        summary.1
                    );
                }
                accessors.insert(did, summary);
            }
        }
        // mir_keys(()) -> &FxIndexSet<LocalDefId>: every DefId in this crate with
        // MIR. (verified present on TyCtxt)
        for &ldid in tcx.mir_keys(()) {
            let did = ldid.to_def_id();
            // Only function-like bodies. optimized_mir is the WRONG query for
            // const/static items (they use mir_for_ctfe) and calling it on them
            // ICEs -- lazy_static! generates exactly such statics. Gating on
            // DefKind is the standard robustness guard (clippy/lockbud do this).
            if !matches!(
                tcx.def_kind(did),
                DefKind::Fn | DefKind::AssocFn | DefKind::Closure
            ) {
                continue;
            }
            if !tcx.is_mir_available(did) {
                continue; // foreign/shim items without a body
            }
            // optimized_mir: post-borrowck, post-drop-elaboration, so RAII guard
            // unlocks are explicit Drop terminators. (verified present)
            let body: &Body<'tcx> = tcx.optimized_mir(did);
            analyze_body(tcx, ldid, body, &accessors);
        }
        write_events(tcx);
    }
}

/// Whorl's lock-CLASS abstraction: classify a local's TYPE as a lock guard, and
/// return (guard kind tag, protected-data type string). Mirrors lockbud's
/// `from_local_ty`. Returns None for non-guards and for async/loom guards.
fn guard_class_of_ty<'tcx>(tcx: TyCtxt<'tcx>, ty: ty::Ty<'tcx>) -> Option<(&'static str, String)> {
    let ty::TyKind::Adt(adt_def, args) = ty.kind() else {
        return None;
    };
    // VERIFY: def_path_str_with_args(def_id, GenericArgsRef). The old name was
    // def_path_str_with_substs (REMOVED). Confirmed present on TyCtxt by verifier;
    // confirm the arg is the ADT's GenericArgsRef on first build.
    let path = tcx.def_path_str_with_args(adt_def.did(), args);
    if ["async", "tokio", "future", "loom"].iter().any(|m| path.contains(m)) {
        return None; // unsupported, as in lockbud
    }
    let lib_std = !path.starts_with("parking_lot")
        && !path.starts_with("spin")
        && !path.starts_with("lock_api");
    let kind = if path.contains("MutexGuard") {
        "Mutex"
    } else if path.contains("RwLockReadGuard") {
        "RwRead"
    } else if path.contains("RwLockWriteGuard") {
        "RwWrite"
    } else {
        return None;
    };
    // Protected-data type position differs by library (lockbud): std/spin take
    // the first type arg, parking_lot/lock_api the second.
    // VERIFY: args.types() iterator order + the nth(1) choice for parking_lot.
    let data_ty = if lib_std || path.starts_with("spin") {
        args.types().next()
    } else {
        args.types().nth(1)
    };
    let data = data_ty.map(|t| t.to_string()).unwrap_or_else(|| "_".into());
    Some((kind, data))
}

/// Remove `::<...>` generic segments (balanced) from a def path, so
/// `std::sync::Mutex::<T>::lock` compares as `std::sync::Mutex::lock`.
/// def_path_str on this nightly renders impl-self generics INSIDE the path
/// (confirmed via WHORL_DEBUG), so suffix matching must strip them first.
fn strip_generics(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let b = p.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b':' && b.get(i + 1) == Some(&b':') && b.get(i + 2) == Some(&b'<') {
            let mut depth = 0usize;
            i += 2; // at '<'
            while i < b.len() {
                match b[i] {
                    b'<' => depth += 1,
                    b'>' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            i += 1; // past '>'
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

/// True if `did` is a lock-acquiring method. PRODUCTION form should use
/// clippy_utils PathLookup (value_path!/.matches) -- see README. This
/// def_path_str fallback is FRAGILE (path formatting) and only here so the
/// scaffold has no hard clippy_utils call site to break the build before you
/// wire PathLookup.
fn is_lock_acquire<'tcx>(tcx: TyCtxt<'tcx>, did: DefId) -> bool {
    let p = strip_generics(&tcx.def_path_str(did));
    p.ends_with("Mutex::lock")
        || p.ends_with("RwLock::read")
        || p.ends_with("RwLock::write")
        || (p.starts_with("parking_lot")
            && (p.ends_with("::lock")
                || p.ends_with("::read")
                || p.ends_with("::write")
                || p.ends_with("::read_recursive")
                || p.ends_with("::upgradable_read")))
        || (p.starts_with("spin") && (p.ends_with("::lock") || p.ends_with("::read") || p.ends_with("::write")))
}

/// True if `did` is `Result::unwrap`/`expect` -- the std lock guards are reached
/// through exactly this call (`lock().unwrap()`), so it links result to guard.
fn is_result_unwrap<'tcx>(tcx: TyCtxt<'tcx>, did: DefId) -> bool {
    let p = strip_generics(&tcx.def_path_str(did));
    p.ends_with("Result::unwrap") || p.ends_with("Result::expect")
}

/// True if a type is something that can be called: a generic parameter (which
/// may carry an Fn bound), a function pointer, or a trait object.
fn is_callable_shaped<'tcx>(ty: ty::Ty<'tcx>) -> bool {
    matches!(
        ty.kind(),
        ty::TyKind::Param(_) | ty::TyKind::FnPtr(..) | ty::TyKind::Dynamic(..)
    )
}

/// Follow borrow temps back to the place actually being referred to. Optimized
/// MIR freely inserts `_t = &SRC; ... (*_t)`, so both a bare temp and a deref of
/// one must resolve to SRC; otherwise the same lock renders differently
/// depending on how many temps the optimizer happened to introduce.
fn resolve_place_root<'tcx>(body: &Body<'tcx>, place: Place<'tcx>) -> Place<'tcx> {
    let mut cur = place;
    for _ in 0..8 {
        let idx = cur.local.as_usize();
        if idx >= 1 && idx <= body.arg_count {
            break; // rooted in a parameter: canonical
        }
        let deref_only = cur.projection.len() == 1
            && matches!(cur.projection[0], PlaceElem::Deref);
        if cur.projection.is_empty() || deref_only {
            match sole_borrow_source(body, cur.local) {
                Some(src) => {
                    cur = src;
                    continue;
                }
                None => break,
            }
        }
        break;
    }
    cur
}

/// Render a projection chain onto a starting type into (symbol, identity)
/// suffixes. Field components carry their SOURCE name, so the same field
/// reached two different ways renders identically.
fn render_projection<'tcx>(
    start_ty: ty::Ty<'tcx>,
    projection: &[PlaceElem<'tcx>],
) -> (String, String) {
    let mut cur_ty = start_ty;
    let (mut sym, mut id) = (String::new(), String::new());
    for elem in projection {
        match elem {
            PlaceElem::Field(f, fty) => {
                let name = match cur_ty.kind() {
                    ty::TyKind::Adt(adt, _) if adt.is_struct() => adt
                        .non_enum_variant()
                        .fields
                        .get(*f)
                        .map(|fd| fd.name.to_string()),
                    _ => None,
                };
                let name = name.unwrap_or_else(|| format!("f{}", f.as_u32()));
                let _ = write!(sym, ".{name}:{fty}");
                let _ = write!(id, ".{name}");
                cur_ty = *fty;
            }
            PlaceElem::Deref => {
                sym.push_str(".*");
                if let ty::TyKind::Ref(_, inner, _) = cur_ty.kind() {
                    cur_ty = *inner;
                }
            }
            _ => sym.push_str(".[]"),
        }
    }
    (sym, id)
}

/// Render a whole place: its base local's TYPE plus its projection chain.
fn render_place<'tcx>(body: &Body<'tcx>, place: Place<'tcx>) -> (String, String) {
    let base_ty = body.local_decls[place.local].ty;
    let (sym, id) = render_projection(base_ty, place.projection);
    (
        format!("{base_ty}{sym}"),
        format!("{base_ty}#{}{id}", place.local.as_u32()),
    )
}

/// If this body is a simple field accessor -- it returns a reference to a
/// projection of one of its parameters, and does so on exactly one path --
/// summarize it as (1-based parameter index, symbol suffix, identity suffix).
/// That lets a caller reach the SAME canonical class through the accessor as it
/// would by touching the field directly, instead of splitting the lock in two.
fn accessor_summary<'tcx>(body: &Body<'tcx>) -> Option<(usize, String, String)> {
    let mut found: Option<(usize, String, String)> = None;
    for l in move_chain(body, Local::from_u32(0)) {
        for data in body.basic_blocks.iter() {
            for stmt in &data.statements {
                if let StatementKind::Assign(boxed) = &stmt.kind {
                    let (dest, rvalue) = &**boxed;
                    if dest.local != l || !dest.projection.is_empty() {
                        continue;
                    }
                    if let Rvalue::Ref(_, _, src) = rvalue {
                        let src = resolve_place_root(body, *src);
                        let idx = src.local.as_usize();
                        if idx == 0 || idx > body.arg_count {
                            return None; // not rooted in a parameter
                        }
                        if found.is_some() {
                            return None; // several return paths: not simple
                        }
                        let start_ty = body.local_decls[src.local].ty;
                        let (sym, id) = render_projection(start_ty, src.projection);
                        found = Some((idx, sym, id));
                    }
                }
            }
        }
    }
    found
}

/// The Call terminator that defines `local`, with its callee and argument
/// places, if there is exactly one.
fn call_defining<'tcx>(
    body: &Body<'tcx>,
    local: Local,
) -> Option<(DefId, Vec<Place<'tcx>>)> {
    let mut found = None;
    for data in body.basic_blocks.iter() {
        if let TerminatorKind::Call { func, args, destination, .. } = &data.terminator().kind {
            if destination.local == local && destination.projection.is_empty() {
                if found.is_some() {
                    return None;
                }
                let (callee, _) = func.const_fn_def()?;
                found = Some((
                    callee,
                    args.iter().filter_map(|a| a.node.place()).collect::<Vec<_>>(),
                ));
            }
        }
    }
    found
}

/// True if `local` is defined by a Call terminator, i.e. it holds a value
/// returned by a function. The receiver resolution can only see through
/// borrows, so a lock reached this way has no canonical field path.
fn is_call_destination<'tcx>(body: &Body<'tcx>, local: Local) -> bool {
    body.basic_blocks.iter().any(|data| {
        matches!(
            &data.terminator().kind,
            TerminatorKind::Call { destination, .. }
                if destination.local == local && destination.projection.is_empty()
        )
    })
}

/// The chain of locals a value was moved through: `local` plus everything it
/// was moved OUT of. `mem::drop(g)` lowers to `_tmp = move _g; drop(_tmp)`, so
/// releasing only the temp would leave the real guard live forever.
fn move_chain<'tcx>(body: &Body<'tcx>, start: Local) -> Vec<Local> {
    let mut chain = vec![start];
    let mut cur = start;
    for _ in 0..8 {
        let mut src: Option<Local> = None;
        for data in body.basic_blocks.iter() {
            for stmt in &data.statements {
                if let StatementKind::Assign(boxed) = &stmt.kind {
                    let (dest, rvalue) = &**boxed;
                    if dest.local == cur && dest.projection.is_empty() {
                        if let Rvalue::Use(Operand::Move(p)) = rvalue {
                            if src.is_some() {
                                return chain; // several sources: stop, stay safe
                            }
                            src = Some(p.local);
                        }
                    }
                }
            }
        }
        match src {
            Some(s) => {
                chain.push(s);
                cur = s;
            }
            None => break,
        }
    }
    chain
}

/// True if `did` is `Deref::deref` / `DerefMut::deref_mut`. Semantically the
/// result IS `(*arg)`, so rendering it that way makes a lock reached through a
/// smart pointer unify with the same lock reached by a built-in deref, instead
/// of splitting it into a second class. Values of the same pointer type render
/// alike, so this MERGES rather than splits -- the sound direction.
fn is_deref_call<'tcx>(tcx: TyCtxt<'tcx>, did: DefId) -> bool {
    let p = strip_generics(&tcx.def_path_str(did));
    p.ends_with("Deref::deref") || p.ends_with("DerefMut::deref_mut")
}

/// True if `did` is `mem::drop`, which genuinely releases its argument.
fn is_mem_drop<'tcx>(tcx: TyCtxt<'tcx>, did: DefId) -> bool {
    let p = strip_generics(&tcx.def_path_str(did));
    p == "std::mem::drop" || p == "core::mem::drop" || p.ends_with("mem::drop")
}

/// True if `did` is one of the `Fn`/`FnMut`/`FnOnce` call shims, i.e. an
/// indirect call on a callable value rather than a statically known function.
fn is_fn_call_shim<'tcx>(tcx: TyCtxt<'tcx>, did: DefId) -> bool {
    let p = strip_generics(&tcx.def_path_str(did));
    p.ends_with("FnOnce::call_once") || p.ends_with("FnMut::call_mut") || p.ends_with("Fn::call")
}

/// Trace `local` back through `_a = move/copy _b` chains to a parameter of this
/// body, returning its 1-based index. Used to see that an indirect call is on a
/// callable the function received as an argument.
fn resolve_to_param<'tcx>(body: &Body<'tcx>, mut local: Local) -> Option<usize> {
    for _ in 0..8 {
        let idx = local.as_usize();
        if idx >= 1 && idx <= body.arg_count {
            return Some(idx);
        }
        let mut src: Option<Local> = None;
        for data in body.basic_blocks.iter() {
            for stmt in &data.statements {
                if let StatementKind::Assign(boxed) = &stmt.kind {
                    let (dest, rvalue) = &**boxed;
                    if dest.local == local && dest.projection.is_empty() {
                        if let Rvalue::Use(op) = rvalue {
                            if let Some(p) = op.place() {
                                if src.is_some() {
                                    return None; // multiple defs: give up
                                }
                                src = Some(p.local);
                            }
                        }
                    }
                }
            }
        }
        local = src?;
    }
    None
}

/// The single global critical-section resource (interrupts disabled). Every
/// `critical_section::Mutex` / `cortex_m::interrupt::Mutex` is guarded by THIS
/// one lock, so it is one reentrant class, not one class per Mutex. Entering it
/// while holding another lock, and taking another lock while inside it, are what
/// create ordering edges -- the classic single-core critical-section-vs-spinlock
/// deadlock.
const CS_CLASS: &str = "<critical-section>";

/// True if `did` enters a critical section (masks interrupts) and runs a closure
/// inside it: `critical_section::with` or `cortex_m::interrupt::free`.
fn is_critical_section_enter<'tcx>(tcx: TyCtxt<'tcx>, did: DefId) -> bool {
    let p = strip_generics(&tcx.def_path_str(did));
    p == "critical_section::with"
        || p.ends_with("::interrupt::free")
        || p.ends_with("interrupt::free")
}

/// The def path of the closure (or fn) an operand refers to, so a call edge can
/// target the masked region's body. Critical-section entries take the closure by
/// value, so its type is `ty::Closure(def_id, _)`.
fn callee_body_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    op: &Operand<'tcx>,
) -> Option<String> {
    match op.ty(&body.local_decls, tcx).kind() {
        ty::TyKind::Closure(did, _) | ty::TyKind::FnDef(did, _) => {
            Some(tcx.def_path_str(*did))
        }
        _ => None,
    }
}

fn analyze_body<'tcx>(
    tcx: TyCtxt<'tcx>,
    owner: LocalDefId,
    body: &Body<'tcx>,
    accessors: &HashMap<DefId, (usize, String, String)>,
) {
    let function = tcx.def_path_str(owner.to_def_id());

    // --- pass 1: guard locals by type -> their lock class symbol ---------------
    // class symbol = guard-kind + protected-data + receiver-base identity.
    let mut guard_kind: BTreeMap<Local, (&'static str, String)> = BTreeMap::new();
    for (local, decl) in body.local_decls.iter_enumerated() {
        if let Some(k) = guard_class_of_ty(tcx, decl.ty) {
            guard_kind.insert(local, k);
        }
    }

    // --- pass 2: lock Call terminators -> (acquire location, class, guard local)
    // We recover the receiver Place and canonicalize it into a class symbol.
    struct Acquire {
        loc: Location,
        class: String,
        base_id: String, // for instance counting
        guard_local: Option<Local>,
    }
    let mut acquires: Vec<Acquire> = Vec::new();
    // map each guard local -> its class, so the held-set can name held guards.
    let mut guard_class: BTreeMap<Local, (String, String)> = BTreeMap::new();
    // std lock() returns Result<Guard, _>; the guard local appears only after
    // .unwrap()/.expect(). Track lock-result locals so the unwrap call can hand
    // the class on to its destination (the actual guard local).
    let mut result_class: BTreeMap<Local, (String, String)> = BTreeMap::new();
    // call sites into functions of THIS crate: (location, callee path). The
    // held-set at each site is looked up after the liveness fixpoint below.
    let mut local_calls: Vec<(Location, String)> = Vec::new();
    // critical_section::with / interrupt::free sites: (location, closure path).
    // Entering masks interrupts (an acquire of CS_CLASS); the closure body runs
    // with CS held, so the critical section flows into it as a call edge.
    let mut cs_enters: Vec<(Location, String)> = Vec::new();
    // indirect call on our own parameter #n (a callable we received).
    let mut param_calls: Vec<(Location, usize)> = Vec::new();
    // (callee path, param index, closure path): a callable we pass on.
    let mut closure_args: Vec<(String, usize, String)> = Vec::new();
    // indirect calls we cannot resolve at all.
    let mut opaque_calls: Vec<Location> = Vec::new();
    // `mem::drop(g)` really does release; the generic Move kill no longer covers
    // it, so record these explicitly.
    let mut explicit_drops: Vec<(Location, Local)> = Vec::new();

    for (bb, data) in body.basic_blocks.iter_enumerated() {
        // VERIFY: body.basic_blocks is a FIELD on this nightly (was a method).
        let term = data.terminator();
        // Match with `..`: Call now also has call_source/target/unwind/fn_span.
        if let TerminatorKind::Call { func, args, destination, .. } = &term.kind {
            // const_fn_def() -> Option<(DefId, GenericArgsRef)>. (verified)
            let Some((callee, _generics)) = func.const_fn_def() else {
                // A call through a value: fn pointer or trait object. We cannot
                // see the body, so any ordering it establishes is invisible.
                opaque_calls.push(Location { block: bb, statement_index: data.statements.len() });
                continue;
            };
            if std::env::var("WHORL_DEBUG").is_ok() {
                eprintln!("whorl-debug: call {} in {}", tcx.def_path_str(callee), function);
            }
            if is_result_unwrap(tcx, callee) {
                // `_g = Result::unwrap(move _r)`: if _r came from a lock() call,
                // its destination _g is the guard for that lock's class.
                if let Some(arg_local) = args
                    .get(0)
                    .and_then(|a| a.node.place())
                    .map(|p| p.local)
                {
                    if let Some(cls) = result_class.get(&arg_local) {
                        guard_class.insert((*destination).local, cls.clone());
                    }
                }
                continue;
            }
            if is_mem_drop(tcx, callee) {
                let loc = Location { block: bb, statement_index: data.statements.len() };
                for a in args.iter() {
                    if let Some(p) = a.node.place() {
                        for l in move_chain(body, p.local) {
                            explicit_drops.push((loc, l));
                        }
                    }
                }
                continue;
            }
            if is_fn_call_shim(tcx, callee) {
                let loc = Location { block: bb, statement_index: data.statements.len() };
                let callable = args.first().map(|a| &a.node);
                if let Some(path) = callable.and_then(|op| callee_body_path(tcx, body, op)) {
                    // the callable is a concrete closure/fn: a normal call edge.
                    local_calls.push((loc, path));
                } else if let Some(p) = callable
                    .and_then(|op| op.place())
                    .and_then(|pl| resolve_to_param(body, pl.local))
                {
                    // an indirect call on a parameter: whoever passed the
                    // callable in decides which body runs (resolved on the
                    // stable side by joining with ClosureArg).
                    param_calls.push((loc, p));
                } else {
                    opaque_calls.push(loc);
                }
                continue;
            }
            if is_critical_section_enter(tcx, callee) {
                // The masked region is the closure argument. Record it so the
                // critical section can be flowed into that body below.
                let loc = Location { block: bb, statement_index: data.statements.len() };
                match args.first().and_then(|a| callee_body_path(tcx, body, &a.node)) {
                    Some(closure) => cs_enters.push((loc, closure)),
                    // Generic wrapper, boxed closure or fn pointer: we cannot see
                    // which body runs masked, so every lock it takes loses its
                    // `critical-section < lock` edge. Fail closed.
                    None => opaque_calls.push(loc),
                }
                continue;
            }
            if !is_lock_acquire(tcx, callee) {
                let loc = Location { block: bb, statement_index: data.statements.len() };
                // Note every callable we hand to this callee, so an indirect
                // call on the matching parameter can be resolved to it.
                for (i, a) in args.iter().enumerate() {
                    let ty = a.node.ty(&body.local_decls, tcx);
                    match callee_body_path(tcx, body, &a.node) {
                        Some(cl) => closure_args.push((tcx.def_path_str(callee), i + 1, cl)),
                        // Callable-shaped but unnameable (a generic F forwarded on,
                        // a fn pointer, a boxed dyn Fn). Record it as UNKNOWN so a
                        // sibling call passing a real closure cannot make this
                        // position look resolved.
                        None if is_callable_shaped(ty) => closure_args.push((
                            tcx.def_path_str(callee),
                            i + 1,
                            "<unknown>".to_string(),
                        )),
                        None => {}
                    }
                }
                if callee.is_local() {
                    // A trait method call stays virtual in MIR; def_path_str gives
                    // the TRAIT method path, which matches no analyzed body, so the
                    // edge would be silently dead. Treat it as opaque instead.
                    // A trait method's parent DefId is the trait itself; an
                    // inherent method's parent is the impl. Generic and virtual
                    // calls keep the TRAIT method here, unresolved to a body.
                    if matches!(tcx.def_kind(tcx.parent(callee)), DefKind::Trait) {
                        opaque_calls.push(loc);
                    } else {
                        local_calls.push((loc, tcx.def_path_str(callee)));
                    }
                }
                continue;
            }
            // receiver = first arg; args[i] is Spanned<Operand> => .node. (verified)
            let recv: Option<Place<'tcx>> = args.get(0).and_then(|a| a.node.place());
            // Back-track the borrow temp to its source Place, then canonicalize.
            let (class, base_id) = match recv {
                Some(p) => lock_class_of_receiver(tcx, body, p, accessors),
                None => ("<unknown-lock>".to_string(), "<unknown>".to_string()),
            };
            let loc = Location { block: bb, statement_index: data.statements.len() };
            // The guard local is the destination's local IF its type is a guard
            // (parking_lot/spin return the guard directly); for std the guard is
            // reached after unwrap, which result_class handles above.
            let dest_local = (*destination).local;
            let guard_local = guard_kind.get(&dest_local).map(|_| dest_local);
            result_class.insert(dest_local, (class.clone(), base_id.clone()));
            acquires.push(Acquire { loc, class: class.clone(), base_id: base_id.clone(), guard_local });
        }
    }

    // Link guard locals to a class. Heuristic: a guard local takes the class of
    // the nearest preceding acquire whose result flows into it. As a robust
    // first cut we map every guard local to the class of the acquire whose
    // destination-local equals it; guards reached via unwrap inherit the class
    // of the acquire in the same block. PRODUCTION: replace with proper
    // result->guard dataflow (the unwrap chain), see README.
    for a in &acquires {
        if let Some(gl) = a.guard_local {
            guard_class.insert(gl, (a.class.clone(), a.base_id.clone()));
        }
    }
    // A guard local we could not link to a lock class is a lock whose IDENTITY
    // is unknown -- e.g. a guard returned from a helper. Fabricating a unique
    // symbol for it (the old behaviour) is NOT conservative: a made-up name is a
    // graph node nothing else mentions, so it can never close a cycle, and the
    // real class it stands for is silently absent from every held-set that
    // follows. Coarsening a class is sound; renaming one is not. An unknown held
    // class could be ANY class, so the only sound treatments are to relate it to
    // everything or to stop claiming a conclusive verdict. We do the latter.
    // First let a class follow the value: `_tmp = move _g` gives _tmp the same
    // lock, so an unlinked temp inherits its source's class instead of looking
    // like an unknown lock.
    let unlinked: Vec<Local> = guard_kind
        .keys()
        .copied()
        .filter(|gl| !guard_class.contains_key(gl))
        .collect();
    for gl in unlinked {
        if let Some(cls) = move_chain(body, gl)
            .into_iter()
            .find_map(|l| guard_class.get(&l).cloned())
        {
            guard_class.insert(gl, cls);
        }
    }
    // A guard whose class is still unknown only costs us an edge if something
    // actually happens while it is held. Deciding that needs the liveness
    // solution, so record the set now and judge below.
    let unknown_guards: BTreeSet<Local> = guard_kind
        .keys()
        .copied()
        .filter(|gl| !guard_class.contains_key(gl))
        .collect();

    // --- pass 3: gen/kill liveness of guard locals -----------------------------
    let guard_locals: BTreeSet<Local> = guard_kind.keys().copied().collect();
    let mut gk = GuardGenKill { guards: &guard_locals, gens: BTreeMap::new(), kill: BTreeMap::new() };
    gk.visit_body(body);
    for (loc, local) in &explicit_drops {
        if guard_locals.contains(local) {
            gk.kill.entry(*loc).or_default().insert(*local);
        }
    }

    // Forward may-live fixpoint over basic blocks; join = UNION (sound upper
    // bound on the held-set, matching lockbud's join). We compute the live set
    // at every Location, then read it at each acquire Location.
    let live = solve_live(body, &gk);

    // Fail closed only where it matters: an unknown held lock is invisible in
    // the held-set, so any acquisition or call made while it is live loses an
    // ordering edge. If nothing happens under it (e.g. a guard merely passed
    // through a function), no edge is lost and the verdict stays conclusive.
    if !unknown_guards.is_empty() {
        let mut sites: Vec<Location> = acquires.iter().map(|a| a.loc).collect();
        sites.extend(local_calls.iter().map(|(l, _)| *l));
        sites.extend(param_calls.iter().map(|(l, _)| *l));
        sites.extend(cs_enters.iter().map(|(l, _)| *l));
        sites.extend(opaque_calls.iter().copied());
        for loc in sites {
            let here = live.get(&loc).cloned().unwrap_or_default();
            if let Some(gl) = here.iter().find(|l| unknown_guards.contains(l)) {
                let (kind, data) = &guard_kind[gl];
                mark_incomplete(format!(
                    "in {function}: a {kind}<{data}> guard whose lock class could not                      be determined is held at {}, so that held-set is missing an entry",
                    span_to_site(tcx, body, loc)
                ));
                break;
            }
        }
    }

    // --- emit one Event per acquire --------------------------------------------
    PROGRAM.with(|prog| {
        let mut prog = prog.borrow_mut();
        for a in &acquires {
            let here_live = live.get(&a.loc).cloned().unwrap_or_default();
            let mut held: BTreeSet<String> = BTreeSet::new();
            for gl in here_live {
                if Some(gl) == a.guard_local {
                    continue; // do not count the lock we are acquiring as held
                }
                if let Some((cls, _)) = guard_class.get(&gl) {
                    held.insert(cls.clone());
                }
            }
            let site = span_to_site(tcx, body, a.loc);
            prog.events.push(Event {
                function: function.clone(),
                site,
                held,
                acquires: a.class.clone(),
            });
            prog.class_instances
                .entry(a.class.clone())
                .or_default()
                .insert(a.base_id.clone());
        }
        for (loc, callee) in &local_calls {
            let mut held: BTreeSet<String> = BTreeSet::new();
            for gl in live.get(loc).cloned().unwrap_or_default() {
                if let Some((cls, _)) = guard_class.get(&gl) {
                    held.insert(cls.clone());
                }
            }
            prog.calls.push(CallEdge {
                function: function.clone(),
                callee: callee.clone(),
                held,
            });
        }
        for (loc, param) in &param_calls {
            let mut held: BTreeSet<String> = BTreeSet::new();
            for gl in live.get(loc).cloned().unwrap_or_default() {
                if let Some((cls, _)) = guard_class.get(&gl) {
                    held.insert(cls.clone());
                }
            }
            prog.param_calls.push(ParamCall {
                function: function.clone(),
                param: *param,
                held,
            });
        }
        for (callee, param, closure) in &closure_args {
            prog.closure_args.push(ClosureArg {
                function: function.clone(),
                callee: callee.clone(),
                param: *param,
                closure: closure.clone(),
            });
        }
        for loc in &opaque_calls {
            let mut held: BTreeSet<String> = BTreeSet::new();
            for gl in live.get(loc).cloned().unwrap_or_default() {
                if let Some((cls, _)) = guard_class.get(&gl) {
                    held.insert(cls.clone());
                }
            }
            // Do NOT drop records with an empty LOCAL held-set: a lock held by
            // a caller is only known after the interprocedural fixpoint, which
            // runs on the stable side. Emptiness is not ours to decide.
            prog.opaque_calls.push(OpaqueCall {
                function: function.clone(),
                site: span_to_site(tcx, body, *loc),
                held,
            });
        }
        for (loc, closure) in &cs_enters {
            // held locks at the point interrupts are masked: each `h` becomes an
            // ordering edge `h < CS` (e.g. spinlock < critical-section).
            let mut held: BTreeSet<String> = BTreeSet::new();
            for gl in live.get(loc).cloned().unwrap_or_default() {
                if let Some((cls, _)) = guard_class.get(&gl) {
                    held.insert(cls.clone());
                }
            }
            let site = span_to_site(tcx, body, *loc);
            prog.events.push(Event {
                function: function.clone(),
                site,
                held: held.clone(),
                acquires: CS_CLASS.to_string(),
            });
            // one global critical section => a single instance.
            prog.class_instances
                .entry(CS_CLASS.to_string())
                .or_default()
                .insert("<global>".to_string());
            // the closure runs with the critical section (and everything already
            // held) live, so a lock taken inside it gets `CS < that-lock`.
            let mut into = held;
            into.insert(CS_CLASS.to_string());
            prog.calls.push(CallEdge {
                function: function.clone(),
                callee: closure.clone(),
                held: into,
            });
        }
    });
}

/// Canonicalize the lock RECEIVER Place into (class symbol, base identity).
/// The class symbol is Whorl's field-path/static lock CLASS; base identity
/// distinguishes instances of the same class for class_instances counting.
fn lock_class_of_receiver<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    place: Place<'tcx>,
    accessors: &HashMap<DefId, (usize, String, String)>,
) -> (String, String) {
    // Resolve a borrow temp `_R` whose sole def is `_R = &SRC` back to SRC, so
    // `lock(move _R)` is attributed to the Mutex Place SRC. One hop is enough
    // for the common borrow-then-call pattern; loop-resolve for chains.
    let cur = resolve_place_root(body, place);
    // If the base local is the RESULT OF A CALL, the projection chain we can see
    // is not the lock's canonical path. When the callee is a simple field
    // accessor we can rebuild that path: substitute the argument it was called
    // with and append the accessor's own field path, so this route renders
    // IDENTICALLY to touching the field directly. Otherwise the lock would be
    // split into two class symbols that can never close a cycle, which is not
    // conservative, so the verdict stops being conclusive.
    if is_call_destination(body, cur.local) {
        // The reference returned by the accessor is dereferenced here; that
        // Deref is what the accessor's suffix already describes.
        let extra_ok = cur.projection.is_empty()
            || (cur.projection.len() == 1 && matches!(cur.projection[0], PlaceElem::Deref));
        if let (true, Some((callee, arg_places))) = (extra_ok, call_defining(body, cur.local)) {
            // `deref(x)` is `(*x)`: render it as the built-in deref would.
            if is_deref_call(tcx, callee) {
                if let Some(argp) = arg_places.first() {
                    let (base_sym, base_id) = render_place(body, *argp);
                    return (format!("{base_sym}.*"), format!("{base_id}.*"));
                }
            }
            if let Some((pix, suffix_sym, suffix_id)) = accessors.get(&callee) {
                if let Some(argp) = arg_places.get(pix.saturating_sub(1)) {
                    let (base_sym, base_id) = render_place(body, *argp);
                    return (
                        format!("{base_sym}{suffix_sym}"),
                        format!("{base_id}{suffix_id}"),
                    );
                }
            }
        }
        let via = call_defining(body, cur.local)
            .map(|(c, _)| tcx.def_path_str(c))
            .unwrap_or_else(|| "<unknown call>".to_string());
        mark_incomplete(format!(
            "a lock is reached through `{via}`, whose body is not a simple field              accessor, so it has no canonical class path and may not unify with              the same lock reached directly"
        ));
    }
    render_place(body, cur)
}

/// If `local` is defined by exactly one statement of the form `local = &SRC`
/// (or `&mut SRC`), return SRC. Used to see through the borrow temp that
/// `Mutex::lock(move _temp)` takes.
fn sole_borrow_source<'tcx>(body: &Body<'tcx>, local: Local) -> Option<Place<'tcx>> {
    let mut found: Option<Place<'tcx>> = None;
    for data in body.basic_blocks.iter() {
        for stmt in &data.statements {
            if let StatementKind::Assign(boxed) = &stmt.kind {
                let (dest, rvalue) = &**boxed;
                if dest.local == local && dest.projection.is_empty() {
                    // VERIFY: Rvalue::Ref variant shape { region, borrow_kind, place }
                    // -- field names/positions drift; match the Ref source Place.
                    if let Rvalue::Ref(_region, _kind, src) = rvalue {
                        if found.is_some() {
                            return None; // multiple defs => not a simple temp
                        }
                        found = Some(*src);
                    }
                }
            }
        }
    }
    found
}

/// Render an acquire Location to "file:line" for the Whorl `site`.
fn span_to_site<'tcx>(tcx: TyCtxt<'tcx>, body: &Body<'tcx>, loc: Location) -> String {
    let src_info = body
        .stmt_at(loc)
        .map_left(|s| s.source_info)
        .map_right(|t| t.source_info)
        .either(|l| l, |r| r);
    // VERIFY: body.stmt_at(loc) returns Either<&Statement,&Terminator>; the
    // Either combinators above (map_left/map_right/either) are from the `either`
    // re-export rustc uses -- confirm method names, or replace with manual
    // indexing: body.basic_blocks[loc.block].statements.get(idx).
    // FileName::prefer_local()/FileNameDisplayPreference are gone or private on
    // this nightly; match the Real variant directly and fall back to Debug.
    let sm = tcx.sess.source_map();
    let lo = sm.lookup_char_pos(src_info.span.lo());
    let file = match &lo.file.name {
        rustc_span::FileName::Real(real) => match real.local_path() {
            Some(p) => p.display().to_string(),
            None => format!("{real:?}"),
        },
        other => format!("{other:?}"),
    };
    format!("{}:{}", file, lo.line)
}

// ---- gen/kill MIR visitor (NOT rustc_mir_dataflow -- the engine trait is
// ---- version-fragile per the verifier; the bare Visitor is stable). ----------
struct GuardGenKill<'a> {
    guards: &'a BTreeSet<Local>,
    gens: BTreeMap<Location, BTreeSet<Local>>,
    kill: BTreeMap<Location, BTreeSet<Local>>,
}
impl<'a, 'tcx> Visitor<'tcx> for GuardGenKill<'a> {
    fn visit_local(&mut self, local: Local, ctx: PlaceContext, loc: Location) {
        if !self.guards.contains(&local) {
            return;
        }
        match ctx {
            PlaceContext::MutatingUse(MutatingUseContext::Store)
            | PlaceContext::MutatingUse(MutatingUseContext::Call) => {
                self.gens.entry(loc).or_default().insert(local);
            }
            // NOTE: a plain Move is a TRANSFER of the guard, not a release --
            // pushing a guard into a Vec, storing it in a struct, or returning it
            // all move it while the lock stays held. Killing on every Move drops
            // a still-held lock from later held-sets (a false negative). Only
            // Drop and StorageDead (below), plus an explicit mem::drop call
            // handled in the terminator scan, actually release.
            PlaceContext::MutatingUse(MutatingUseContext::Drop) => {
                self.kill.entry(loc).or_default().insert(local);
            }
            _ => {}
        }
    }
    fn visit_statement(&mut self, s: &rustc_middle::mir::Statement<'tcx>, loc: Location) {
        if let StatementKind::StorageDead(l) = s.kind {
            if self.guards.contains(&l) {
                self.kill.entry(loc).or_default().insert(l);
            }
        }
        self.super_statement(s, loc);
    }
    fn visit_terminator(&mut self, t: &rustc_middle::mir::Terminator<'tcx>, loc: Location) {
        // Drop now has fields { place, target, unwind, replace, drop } -> `..`.
        if let TerminatorKind::Drop { place, .. } = &t.kind {
            if self.guards.contains(&place.local) {
                self.kill.entry(loc).or_default().insert(place.local);
            }
        }
        self.super_terminator(t, loc);
    }
}

/// Forward may-live fixpoint: live-out join over successors is UNION. Returns
/// the live-IN set at every Location (the set of guards live just before that
/// point), which is the held-set we read at each acquire.
fn solve_live<'tcx>(
    body: &Body<'tcx>,
    gk: &GuardGenKill<'_>,
) -> BTreeMap<Location, BTreeSet<Local>> {
    use rustc_middle::mir::BasicBlock;
    // entry[bb] = union of exit[pred]. Iterate to fixpoint.
    let nblocks = body.basic_blocks.len();
    let mut entry: Vec<BTreeSet<Local>> = vec![BTreeSet::new(); nblocks];
    // predecessors() is provided by Body via the BasicBlocks cache.
    // VERIFY: body.basic_blocks.predecessors() returns &IndexVec<BB, SmallVec<[BB;..]>>
    let preds = body.basic_blocks.predecessors();
    let mut changed = true;
    let mut per_loc: BTreeMap<Location, BTreeSet<Local>> = BTreeMap::new();
    while changed {
        changed = false;
        per_loc.clear();
        for (bb, data) in body.basic_blocks.iter_enumerated() {
            let mut state: BTreeSet<Local> = BTreeSet::new();
            for &p in &preds[bb] {
                // exit state of pred = recompute by stepping it; cheap enough for
                // a scaffold. We recompute pred exit from its entry below in the
                // same pass by reading entry[p] and applying its gen/kill.
                state.extend(block_exit(body, gk, p, &entry[<BasicBlock as Into<usize>>::into(p)]));
            }
            if state != entry[<BasicBlock as Into<usize>>::into(bb)] {
                entry[<BasicBlock as Into<usize>>::into(bb)] = state.clone();
                changed = true;
            }
            // record live-IN at each statement/terminator location
            let mut cur = entry[<BasicBlock as Into<usize>>::into(bb)].clone();
            for idx in 0..=data.statements.len() {
                let loc = Location { block: bb, statement_index: idx };
                per_loc.insert(loc, cur.clone());
                if let Some(k) = gk.kill.get(&loc) {
                    for l in k {
                        cur.remove(l);
                    }
                }
                if let Some(g) = gk.gens.get(&loc) {
                    cur.extend(g.iter().copied());
                }
            }
        }
    }
    per_loc
}

/// Exit live set of a block given its entry: step KILL-then-GEN through all
/// statements + terminator.
fn block_exit<'tcx>(
    body: &Body<'tcx>,
    gk: &GuardGenKill<'_>,
    bb: rustc_middle::mir::BasicBlock,
    entry: &BTreeSet<Local>,
) -> BTreeSet<Local> {
    let data = &body.basic_blocks[bb];
    let mut cur = entry.clone();
    for idx in 0..=data.statements.len() {
        let loc = Location { block: bb, statement_index: idx };
        if let Some(k) = gk.kill.get(&loc) {
            for l in k {
                cur.remove(l);
            }
        }
        if let Some(g) = gk.gens.get(&loc) {
            cur.extend(g.iter().copied());
        }
    }
    cur
}

/// Serialize the accumulated Program to JSON (hand-rolled, no serde -- the
/// stable `whorl` crate is zero-dependency and reads this with a tiny parser).
fn write_events<'tcx>(_tcx: TyCtxt<'tcx>) {
    PROGRAM.with(|prog| {
        let prog = prog.borrow();
        let mut out = String::new();
        out.push_str("{\n  \"name\": \"mir\",\n  \"events\": [\n");
        for (i, e) in prog.events.iter().enumerate() {
            let held: Vec<String> = e.held.iter().map(|h| jstr(h)).collect();
            let _ = write!(
                out,
                "    {{ \"function\": {}, \"site\": {}, \"acquires\": {}, \"held\": [{}] }}{}\n",
                jstr(&e.function),
                jstr(&e.site),
                jstr(&e.acquires),
                held.join(", "),
                if i + 1 == prog.events.len() { "" } else { "," }
            );
        }
        out.push_str("  ],\n  \"class_instances\": {\n");
        let n = prog.class_instances.len();
        for (i, (cls, set)) in prog.class_instances.iter().enumerate() {
            let _ = write!(
                out,
                "    {}: {}{}\n",
                jstr(cls),
                set.len(),
                if i + 1 == n { "" } else { "," }
            );
        }
        out.push_str("  },
  \"calls\": [
");
        for (i, c) in prog.calls.iter().enumerate() {
            let held: Vec<String> = c.held.iter().map(|h| jstr(h)).collect();
            let _ = write!(
                out,
                "    {{ \"function\": {}, \"callee\": {}, \"held\": [{}] }}{}
",
                jstr(&c.function),
                jstr(&c.callee),
                held.join(", "),
                if i + 1 == prog.calls.len() { "" } else { "," }
            );
        }
        out.push_str("  ],
  \"param_calls\": [
");
        for (i, c) in prog.param_calls.iter().enumerate() {
            let held: Vec<String> = c.held.iter().map(|h| jstr(h)).collect();
            let _ = write!(
                out,
                "    {{ \"function\": {}, \"param\": {}, \"held\": [{}] }}{}
",
                jstr(&c.function),
                c.param,
                held.join(", "),
                if i + 1 == prog.param_calls.len() { "" } else { "," }
            );
        }
        out.push_str("  ],
  \"closure_args\": [
");
        for (i, c) in prog.closure_args.iter().enumerate() {
            let _ = write!(
                out,
                "    {{ \"function\": {}, \"callee\": {}, \"param\": {}, \"closure\": {} }}{}
",
                jstr(&c.function),
                jstr(&c.callee),
                c.param,
                jstr(&c.closure),
                if i + 1 == prog.closure_args.len() { "" } else { "," }
            );
        }
        out.push_str("  ],
  \"opaque_calls\": [
");
        for (i, c) in prog.opaque_calls.iter().enumerate() {
            let held: Vec<String> = c.held.iter().map(|h| jstr(h)).collect();
            let _ = write!(
                out,
                "    {{ \"function\": {}, \"site\": {}, \"held\": [{}] }}{}
",
                jstr(&c.function),
                jstr(&c.site),
                held.join(", "),
                if i + 1 == prog.opaque_calls.len() { "" } else { "," }
            );
        }
        out.push_str("  ]");
        if let Some(reason) = &prog.incomplete {
            let _ = write!(out, ",\n  \"incomplete\": {}", jstr(reason));
        }
        out.push_str("\n}\n");
        let path = std::env::var("WHORL_EVENTS_OUT")
            .unwrap_or_else(|_| "whorl-events.json".to_string());
        let _ = std::fs::write(path, out);
    });
}

fn jstr(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            _ => o.push(c),
        }
    }
    o.push('"');
    o
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
