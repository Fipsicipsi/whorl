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
use std::collections::{BTreeMap, BTreeSet};
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
#[derive(Default)]
struct ProgramOut {
    events: Vec<Event>,
    calls: Vec<CallEdge>,
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

impl<'tcx> LateLintPass<'tcx> for WhorlLockOrder {
    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        let tcx = cx.tcx;
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
            analyze_body(tcx, ldid, body);
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

fn analyze_body<'tcx>(tcx: TyCtxt<'tcx>, owner: LocalDefId, body: &Body<'tcx>) {
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

    for (bb, data) in body.basic_blocks.iter_enumerated() {
        // VERIFY: body.basic_blocks is a FIELD on this nightly (was a method).
        let term = data.terminator();
        // Match with `..`: Call now also has call_source/target/unwind/fn_span.
        if let TerminatorKind::Call { func, args, destination, .. } = &term.kind {
            // const_fn_def() -> Option<(DefId, GenericArgsRef)>. (verified)
            let Some((callee, _generics)) = func.const_fn_def() else { continue };
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
            if !is_lock_acquire(tcx, callee) {
                if callee.is_local() {
                    let loc = Location { block: bb, statement_index: data.statements.len() };
                    local_calls.push((loc, tcx.def_path_str(callee)));
                }
                continue;
            }
            // receiver = first arg; args[i] is Spanned<Operand> => .node. (verified)
            let recv: Option<Place<'tcx>> = args.get(0).and_then(|a| a.node.place());
            // Back-track the borrow temp to its source Place, then canonicalize.
            let (class, base_id) = match recv {
                Some(p) => lock_class_of_receiver(tcx, body, p),
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
    // Any guard local we could not link gets a placeholder so it still appears
    // in held-sets (sound: better to over-report a held lock than to drop it).
    for (&gl, (kind, data)) in &guard_kind {
        guard_class.entry(gl).or_insert_with(|| {
            (format!("{kind}<{data}>@unlinked:{}", gl.as_u32()), format!("unlinked:{}", gl.as_u32()))
        });
    }

    // --- pass 3: gen/kill liveness of guard locals -----------------------------
    let guard_locals: BTreeSet<Local> = guard_kind.keys().copied().collect();
    let mut gk = GuardGenKill { guards: &guard_locals, gens: BTreeMap::new(), kill: BTreeMap::new() };
    gk.visit_body(body);

    // Forward may-live fixpoint over basic blocks; join = UNION (sound upper
    // bound on the held-set, matching lockbud's join). We compute the live set
    // at every Location, then read it at each acquire Location.
    let live = solve_live(body, &gk);

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
    });
}

/// Canonicalize the lock RECEIVER Place into (class symbol, base identity).
/// The class symbol is Whorl's field-path/static lock CLASS; base identity
/// distinguishes instances of the same class for class_instances counting.
fn lock_class_of_receiver<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    place: Place<'tcx>,
) -> (String, String) {
    // Resolve a borrow temp `_R` whose sole def is `_R = &SRC` back to SRC, so
    // `lock(move _R)` is attributed to the Mutex Place SRC. One hop is enough
    // for the common borrow-then-call pattern; loop-resolve for chains.
    let mut cur = place;
    for _ in 0..8 {
        if let Some(src) = sole_borrow_source(body, cur.local) {
            // Compose: SRC's projection followed by cur's (minus the leading
            // autoref). For the scaffold we take SRC directly when cur has no
            // extra projection.
            cur = if cur.projection.is_empty() { src } else { break };
        } else {
            break;
        }
    }
    // Build a stable symbol from the base local's TYPE + the projection chain.
    // We walk the type alongside the projections so field components can carry
    // their SOURCE name (`.bal`) instead of a positional `.f0`.
    let base_ty = body.local_decls[cur.local].ty;
    let mut cur_ty = base_ty;
    let mut sym = String::new();
    let _ = write!(sym, "{}", base_ty);
    let mut base_id = format!("{}#{}", base_ty, cur.local.as_u32());
    for elem in cur.projection {
        match elem {
            // Field path is the heart of the class abstraction (the .0 in
            // `(*_1).0: Mutex<T>`). Resolve the field's source name from the
            // ADT definition; fall back to the positional index.
            PlaceElem::Field(f, fty) => {
                let name = match cur_ty.kind() {
                    ty::TyKind::Adt(adt, _) if adt.is_struct() => adt
                        .non_enum_variant()
                        .fields
                        .get(f)
                        .map(|fd| fd.name.to_string()),
                    _ => None,
                };
                let name = name.unwrap_or_else(|| format!("f{}", f.as_u32()));
                let _ = write!(sym, ".{name}:{fty}");
                let _ = write!(base_id, ".{name}");
                cur_ty = fty;
            }
            PlaceElem::Deref => {
                sym.push_str(".*");
                // Peel the reference/box so the next Field sees the ADT.
                if let ty::TyKind::Ref(_, inner, _) = cur_ty.kind() {
                    cur_ty = *inner;
                }
            }
            // Index/Subslice etc.: collapse (cannot distinguish instances soundly
            // here -> coarser class, still sound for ordering).
            _ => {
                sym.push_str(".[]");
            }
        }
    }
    // VERIFY: if the base local resolves to a `static`, prefer the static DefId
    // as identity. On this nightly a static read shows up as an Operand/Place
    // with a Static base via PlaceElem or a preceding `_x = &STATIC`; confirm the
    // exact representation and special-case it here (statics are a single global
    // instance => class_instances == 1).
    (sym, base_id)
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
            PlaceContext::MutatingUse(MutatingUseContext::Drop)
            | PlaceContext::NonMutatingUse(NonMutatingUseContext::Move) => {
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
