//! P7 frontend: lower the AST to the solver's event IR with a **summary-based**
//! (may-be-held) interprocedural analysis.
//!
//! Earlier phases memoized on the *exact* held-set at each function entry, which
//! is exponential in the number of lock classes (a function reachable under many
//! conditional combinations of held locks blows up to 2^classes contexts and a
//! sound-but-useless `[INCOMPLETE]` verdict).
//!
//! P7 propagates, per `(function, masked)`, the **union** of every held-set the
//! function can be entered with - a monotone fixpoint over the call graph. This
//! is *exact at the edge level*, not an over-approximation: every ordering edge
//! `h < C` is pairwise, so the set of edges from a `with C` site is exactly
//! `{ h < C : h may be held there }`, and the union of edges over all entry
//! contexts equals the edges computed from the union of entry held-sets. Because
//! the may-held set only grows (bounded by the class count), each function is
//! re-analyzed at most O(classes) times - polynomial, no more `[INCOMPLETE]` on
//! the held-set axis.
//!
//! Retained from earlier phases: 0-CFA callback flow (P5), hand-over-hand
//! `couple` (P3), `extern` FFI contracts (P4), and interrupt-preemption via the
//! synthetic `<cpu>` resource (P6). Soundness is the spine throughout. The known
//! imprecisions are all over-approximations - false positives, never missed
//! deadlocks: (1) 0-CFA merges a callback's targets across call sites; (2) the
//! `couple` entry guard is conservative across (not within) functions; (3) every
//! function is treated as a potential concurrent task entry running with
//! interrupts enabled, so a helper called only inside `mask { }` is still
//! analyzed as an unmasked entry and may get a preemption edge. (3) is the
//! deliberate price of soundness: dropping the entry assumption to remove it
//! would risk missing a real task entry - a false negative.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ast::{Ast, Func, Item, Stmt};
use crate::model::{Event, Program};

const MAX_DEPTH: usize = 512;
/// Backstop on total function-body analyses. With the may-held fixpoint this is
/// polynomial and never reached in practice; it remains a sound fail-closed cap.
const MAX_ANALYSES: usize = 2_000_000;
const SYNTH_CPU: &str = "<cpu>";

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Target {
    Func(String),
    Extern(String),
}

type Flow = HashMap<(String, String), BTreeSet<Target>>;
type Coupled = BTreeMap<String, Option<usize>>;

pub fn lower(ast: &Ast, file: &str) -> Result<Program, Vec<String>> {
    let mut lock_class: HashMap<String, String> = HashMap::new();
    let mut lock_index: HashMap<String, usize> = HashMap::new();
    let mut funcs: HashMap<String, Func> = HashMap::new();
    let mut externs: HashMap<String, Vec<String>> = HashMap::new();
    let mut class_instances: HashMap<String, usize> = HashMap::new();
    let mut errors: Vec<String> = Vec::new();

    for item in &ast.items {
        match item {
            Item::Lock { name, class } => {
                let next_idx = lock_index.len();
                lock_index.entry(name.clone()).or_insert(next_idx);
                if lock_class.insert(name.clone(), class.clone()).is_some() {
                    errors.push(format!("lock '{name}' declared more than once"));
                }
                *class_instances.entry(class.clone()).or_insert(0) += 1;
            }
            Item::Func(f) => {
                if funcs.insert(f.name.clone(), f.clone()).is_some() {
                    errors.push(format!("function '{}' declared more than once", f.name));
                }
                let mut seen = HashSet::new();
                for p in &f.params {
                    if !seen.insert(p.clone()) {
                        errors.push(format!(
                            "function '{}' declares parameter '{}' more than once",
                            f.name, p
                        ));
                    }
                }
            }
            Item::Extern { name, acquires } => {
                if externs.insert(name.clone(), acquires.clone()).is_some() {
                    errors.push(format!("extern '{name}' declared more than once"));
                }
            }
        }
    }

    let classes: HashSet<String> = lock_class.values().cloned().collect();
    for (name, acquires) in &externs {
        if funcs.contains_key(name) {
            errors.push(format!(
                "'{name}' is declared as both a function and an extern"
            ));
        }
        for c in acquires {
            if !classes.contains(c) {
                errors.push(format!("extern '{name}' acquires unknown class '{c}'"));
            }
        }
    }

    let flow = compute_flow(&funcs, &externs);
    let has_isr = funcs.values().any(|f| f.is_isr);

    let mut low = Low {
        lock_class,
        lock_index,
        funcs,
        externs,
        classes,
        flow,
        has_isr,
        file: file.to_string(),
        events: Vec::new(),
        entry_may: HashMap::new(),
        queue: Vec::new(),
        queued: HashSet::new(),
        analyses: 0,
        errors,
        aborted: false,
        incomplete: None,
    };
    low.run();

    if low.errors.is_empty() {
        Ok(Program {
            name: file.to_string(),
            events: low.events,
            class_instances,
            incomplete: low.incomplete,
        })
    } else {
        let mut seen = HashSet::new();
        low.errors.retain(|e| seen.insert(e.clone()));
        Err(low.errors)
    }
}

fn flow_targets(
    name: &str,
    fname: &str,
    flow: &Flow,
    funcs: &HashMap<String, Func>,
    externs: &HashMap<String, Vec<String>>,
) -> BTreeSet<Target> {
    if let Some(f) = funcs.get(fname) {
        if f.params.iter().any(|p| p == name) {
            return flow
                .get(&(fname.to_string(), name.to_string()))
                .cloned()
                .unwrap_or_default();
        }
    }
    if funcs.contains_key(name) {
        let mut s = BTreeSet::new();
        s.insert(Target::Func(name.to_string()));
        return s;
    }
    if externs.contains_key(name) {
        let mut s = BTreeSet::new();
        s.insert(Target::Extern(name.to_string()));
        return s;
    }
    BTreeSet::new()
}

fn compute_flow(funcs: &HashMap<String, Func>, externs: &HashMap<String, Vec<String>>) -> Flow {
    let mut calls: Vec<(String, String, Vec<String>)> = Vec::new();
    for (fname, f) in funcs {
        collect_calls(&f.body, fname, &mut calls);
    }
    let mut flow: Flow = HashMap::new();
    loop {
        let mut changed = false;
        for (fname, callee, args) in &calls {
            for ct in flow_targets(callee, fname, &flow, funcs, externs) {
                let Target::Func(tname) = ct else { continue };
                let Some(tf) = funcs.get(&tname) else {
                    continue;
                };
                if tf.params.len() != args.len() {
                    continue;
                }
                let bindings: Vec<(String, BTreeSet<Target>)> = tf
                    .params
                    .iter()
                    .zip(args.iter())
                    .map(|(p, a)| (p.clone(), flow_targets(a, fname, &flow, funcs, externs)))
                    .collect();
                for (p, ats) in bindings {
                    let entry = flow.entry((tname.clone(), p)).or_default();
                    for at in ats {
                        if entry.insert(at) {
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    flow
}

fn collect_calls(stmts: &[Stmt], fname: &str, out: &mut Vec<(String, String, Vec<String>)>) {
    for s in stmts {
        match s {
            Stmt::With { body, .. }
            | Stmt::Ordered { body, .. }
            | Stmt::Couple { body, .. }
            | Stmt::Mask { body, .. } => collect_calls(body, fname, out),
            Stmt::Call { callee, args, .. } => {
                out.push((fname.to_string(), callee.clone(), args.clone()))
            }
        }
    }
}

struct Low {
    lock_class: HashMap<String, String>,
    lock_index: HashMap<String, usize>,
    funcs: HashMap<String, Func>,
    externs: HashMap<String, Vec<String>>,
    classes: HashSet<String>,
    flow: Flow,
    has_isr: bool,
    file: String,
    events: Vec<Event>,
    /// `(function, masked)` -> union of every held-set it can be entered with.
    entry_may: HashMap<(String, bool), BTreeSet<String>>,
    queue: Vec<(String, bool)>,
    queued: HashSet<(String, bool)>,
    analyses: usize,
    errors: Vec<String>,
    aborted: bool,
    incomplete: Option<String>,
}

impl Low {
    /// Seed every function as a potential entry point, then run the may-held
    /// fixpoint to convergence.
    fn run(&mut self) {
        let mut names: Vec<String> = self.funcs.keys().cloned().collect();
        names.sort();
        for name in &names {
            if self.funcs[name].is_isr {
                // An interrupt handler runs holding the CPU, interrupts masked.
                let mut h = BTreeSet::new();
                h.insert(SYNTH_CPU.to_string());
                self.seed(name, true, h);
            } else {
                self.seed(name, false, BTreeSet::new());
            }
        }
        while let Some((fname, masked)) = self.pop() {
            if self.aborted {
                break;
            }
            self.analyze_body(&fname, masked);
        }
    }

    fn seed(&mut self, name: &str, masked: bool, held: BTreeSet<String>) {
        let e = self
            .entry_may
            .entry((name.to_string(), masked))
            .or_default();
        for h in held {
            e.insert(h);
        }
        self.enqueue(name, masked);
    }

    fn enqueue(&mut self, name: &str, masked: bool) {
        let key = (name.to_string(), masked);
        if self.queued.insert(key.clone()) {
            self.queue.push(key);
        }
    }

    fn pop(&mut self) -> Option<(String, bool)> {
        let k = self.queue.pop()?;
        self.queued.remove(&k);
        Some(k)
    }

    /// Grow a callee's may-held entry set by the caller's held-set at the call
    /// site; (re-)enqueue it if the context is new or anything was added.
    /// A *newly created* context must be enqueued even when the incoming
    /// held-set is empty, or a context reached only via a zero-held call (e.g.
    /// an interrupt handler invoked unmasked as a subroutine) would never be
    /// analyzed and its edges would be silently dropped.
    fn grow_entry(&mut self, callee: &str, masked: bool, held: &BTreeSet<String>) {
        let key = (callee.to_string(), masked);
        let mut changed = !self.entry_may.contains_key(&key);
        let e = self.entry_may.entry(key).or_default();
        for h in held {
            if e.insert(h.clone()) {
                changed = true;
            }
        }
        if changed {
            self.enqueue(callee, masked);
        }
    }

    fn analyze_body(&mut self, name: &str, masked: bool) {
        self.analyses += 1;
        if self.analyses > MAX_ANALYSES {
            self.aborted = true;
            self.incomplete = Some(format!(
                "the analysis exceeded {MAX_ANALYSES} function analyses before converging \
                 (a scalability backstop, not a malformed program)"
            ));
            return;
        }
        let held = self
            .entry_may
            .get(&(name.to_string(), masked))
            .cloned()
            .unwrap_or_default();
        let f = match self.funcs.get(name) {
            Some(f) => f.clone(),
            None => return,
        };
        self.analyze_block(&f.body, &held, name, &f.params, &Coupled::new(), masked, 0);
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_block(
        &mut self,
        stmts: &[Stmt],
        held: &BTreeSet<String>,
        curfn: &str,
        cur_params: &[String],
        coupled: &Coupled,
        masked: bool,
        depth: usize,
    ) {
        if self.aborted {
            return;
        }
        if depth > MAX_DEPTH {
            self.aborted = true;
            self.incomplete = Some(format!(
                "lock nesting deeper than {MAX_DEPTH} was not fully analyzed"
            ));
            return;
        }
        for s in stmts {
            match s {
                Stmt::With { lock, line, body } => {
                    let class = match self.lock_class.get(lock) {
                        Some(c) => c.clone(),
                        None => {
                            self.errors.push(format!(
                                "{}:{}: undeclared lock '{}'",
                                self.file, line, lock
                            ));
                            continue;
                        }
                    };
                    let idx = self.lock_index.get(lock).copied().unwrap_or(0);
                    let mut next = held.clone();
                    next.insert(class.clone());

                    let coupled_here = coupled.get(&class).copied();
                    let mut nc = coupled.clone();
                    match coupled_here {
                        Some(cur) => {
                            let monotone = match cur {
                                None => true,
                                Some(i) => idx > i,
                            };
                            if monotone {
                                let mut hfe = held.clone();
                                hfe.remove(&class);
                                self.emit(curfn, *line, &hfe, &class);
                            } else {
                                self.emit(curfn, *line, held, &class);
                            }
                            nc.insert(class.clone(), Some(idx));
                        }
                        None => self.emit(curfn, *line, held, &class),
                    }

                    self.emit_preemption(curfn, *line, &class, masked);

                    let next_coupled = if coupled_here.is_some() { &nc } else { coupled };
                    self.analyze_block(
                        body,
                        &next,
                        curfn,
                        cur_params,
                        next_coupled,
                        masked,
                        depth + 1,
                    );
                }
                Stmt::Ordered { locks, line, body } => {
                    let mut classes = Vec::new();
                    let mut ok = true;
                    for l in locks {
                        match self.lock_class.get(l) {
                            Some(c) => classes.push(c.clone()),
                            None => {
                                self.errors.push(format!(
                                    "{}:{}: undeclared lock '{}'",
                                    self.file, line, l
                                ));
                                ok = false;
                            }
                        }
                    }
                    if !ok || classes.is_empty() {
                        continue;
                    }
                    let class = classes[0].clone();
                    if !classes.iter().all(|c| *c == class) {
                        self.errors.push(format!(
                            "{}:{}: ordered(...) requires all locks to share one class, found {:?}",
                            self.file, line, classes
                        ));
                        continue;
                    }
                    self.emit(curfn, *line, held, &class);
                    self.emit_preemption(curfn, *line, &class, masked);
                    let mut next = held.clone();
                    next.insert(class);
                    self.analyze_block(body, &next, curfn, cur_params, coupled, masked, depth + 1);
                }
                Stmt::Couple { class, line, body } => {
                    if !self.classes.contains(class) {
                        self.errors.push(format!(
                            "{}:{}: unknown lock class '{}' in couple",
                            self.file, line, class
                        ));
                        continue;
                    }
                    if held.contains(class) && !coupled.contains_key(class) {
                        self.errors.push(format!(
                            "{}:{}: couple of class '{}' while a lock of that class is already held \
                             outside a couple",
                            self.file, line, class
                        ));
                        continue;
                    }
                    let mut nc = coupled.clone();
                    nc.entry(class.clone()).or_insert(None);
                    self.analyze_block(body, held, curfn, cur_params, &nc, masked, depth + 1);
                }
                Stmt::Mask { body, .. } => {
                    self.analyze_block(body, held, curfn, cur_params, coupled, true, depth + 1);
                }
                Stmt::Call { callee, args, line } => {
                    self.propagate_call(callee, args, *line, held, curfn, cur_params, masked);
                }
            }
        }
    }

    fn emit_preemption(&mut self, curfn: &str, line: usize, class: &str, masked: bool) {
        if self.has_isr && !masked {
            let mut one = BTreeSet::new();
            one.insert(class.to_string());
            self.emit(curfn, line, &one, SYNTH_CPU);
        }
    }

    /// Resolve a call's targets and propagate the current held-set into each
    /// callee's may-held entry (functions) or emit the contract (externs).
    #[allow(clippy::too_many_arguments)]
    fn propagate_call(
        &mut self,
        callee: &str,
        args: &[String],
        line: usize,
        held: &BTreeSet<String>,
        curfn: &str,
        cur_params: &[String],
        masked: bool,
    ) {
        if cur_params.iter().any(|p| p == callee) {
            for a in args {
                if !self.name_defined(a, cur_params) {
                    self.errors.push(format!(
                        "{}:{}: undefined argument '{}'",
                        self.file, line, a
                    ));
                }
            }
            let targets = self
                .flow
                .get(&(curfn.to_string(), callee.to_string()))
                .cloned()
                .unwrap_or_default();
            if targets.len() == 1 {
                if let Some(Target::Func(f)) = targets.iter().next() {
                    if let Some(tf) = self.funcs.get(f) {
                        if tf.params.len() != args.len() {
                            self.errors.push(format!(
                                "{}:{}: '{}' expects {} argument(s), got {}",
                                self.file,
                                line,
                                f,
                                tf.params.len(),
                                args.len()
                            ));
                        }
                    }
                }
            }
            for t in targets {
                match t {
                    Target::Func(g) => self.grow_entry(&g, masked, held),
                    Target::Extern(e) => self.apply_extern(&e, args, line, held, masked),
                }
            }
            return;
        }
        if self.funcs.contains_key(callee) {
            let np = self.funcs[callee].params.len();
            if args.len() != np {
                self.errors.push(format!(
                    "{}:{}: '{}' expects {} argument(s), got {}",
                    self.file,
                    line,
                    callee,
                    np,
                    args.len()
                ));
                return;
            }
            for a in args {
                if !self.name_defined(a, cur_params) {
                    self.errors.push(format!(
                        "{}:{}: undefined argument '{}'",
                        self.file, line, a
                    ));
                }
            }
            self.grow_entry(callee, masked, held);
            return;
        }
        if self.externs.contains_key(callee) {
            self.apply_extern(callee, args, line, held, masked);
            return;
        }
        self.errors.push(format!(
            "{}:{}: call to undefined function '{}'",
            self.file, line, callee
        ));
    }

    fn apply_extern(
        &mut self,
        name: &str,
        args: &[String],
        line: usize,
        held: &BTreeSet<String>,
        masked: bool,
    ) {
        if !args.is_empty() {
            self.errors.push(format!(
                "{}:{}: extern '{}' takes no arguments",
                self.file, line, name
            ));
            return;
        }
        let acquires = self.externs.get(name).cloned().unwrap_or_default();
        let mut h = held.clone();
        for c in &acquires {
            self.emit(name, line, &h, c);
            self.emit_preemption(name, line, c, masked);
            h.insert(c.clone());
        }
    }

    fn name_defined(&self, name: &str, cur_params: &[String]) -> bool {
        cur_params.iter().any(|p| p == name)
            || self.funcs.contains_key(name)
            || self.externs.contains_key(name)
    }

    fn emit(&mut self, function: &str, line: usize, held: &BTreeSet<String>, class: &str) {
        self.events.push(Event {
            function: function.to_string(),
            site: format!("{}:{}", self.file, line),
            held: held.iter().cloned().collect(),
            acquires: class.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;
    use crate::solver::{analyze, Outcome, Report};

    fn report(src: &str) -> Report {
        let ast = parser::parse(src).expect("parse");
        let prog = lower(&ast, "test").expect("lower");
        analyze(&prog)
    }
    fn verdict(src: &str) -> Outcome {
        report(src).outcome
    }

    // --- P1 ---
    #[test]
    fn nested_same_class_deadlocks() {
        let src = "lock a: Account\nlock b: Account\nfn t() { with a { with b { } } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn ordered_alone_is_safe() {
        let src = "lock a: Account\nlock b: Account\nfn t() { with ordered(a, b) { } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn ordered_under_enclosing_same_class_is_deadlock() {
        let src = "lock a: Account\nlock b: Account\nlock c: Account\nlock d: Account\n\
                   fn f() { with c { with ordered(a, b) { } } }\n\
                   fn h() { with a { with ordered(c, d) { } } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn deadlock_across_a_direct_call() {
        let src = "lock acct: Account\nlock risk: RiskBook\n\
                   fn take() { with risk { } }\n\
                   fn agg() { with risk { with acct { } } }\n\
                   fn reb() { with acct { take() } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn consistent_hierarchy_is_safe() {
        let src = "lock bank: BankLock\nlock acct: Account\n\
                   fn f() { with bank { with acct { } } }\n\
                   fn g() { with bank { with acct { } } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }

    // --- P2: callbacks ---
    #[test]
    fn deadlock_through_callback() {
        let src = "lock table: Table\nlock row: Row\n\
                   fn helper(body) { with table { body() } }\n\
                   fn lockrow() { with row { } }\n\
                   fn noop() { }\n\
                   fn import() { helper(lockrow) }\n\
                   fn refresh() { with row { helper(noop) } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn consistent_callback_use_is_safe() {
        let src = "lock table: Table\nlock row: Row\n\
                   fn helper(body) { with table { body() } }\n\
                   fn lockrow() { with row { } }\n\
                   fn a() { helper(lockrow) }\n\
                   fn b() { helper(lockrow) }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn callback_passed_through_two_helpers() {
        let src = "lock table: Table\nlock row: Row\n\
                   fn outer(cb) { with table { inner(cb) } }\n\
                   fn inner(cb) { cb() }\n\
                   fn lockrow() { with row { } }\n\
                   fn use_it() { outer(lockrow) }\n\
                   fn lock_table() { with table { } }\n\
                   fn refresh() { with row { lock_table() } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn duplicate_parameter_is_rejected() {
        let ast = parser::parse("fn helper(cb, cb) { cb() }").unwrap();
        assert!(lower(&ast, "test").is_err());
    }
    #[test]
    fn unbound_callback_entry_is_not_an_error() {
        let src = "lock table: Table\nfn helper(body) { with table { body() } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }

    // --- P3: couple ---
    #[test]
    fn couple_makes_hand_over_hand_safe() {
        let src = "lock root: BTreeNode\nlock child: BTreeNode\nlock leaf: BTreeNode\n\
                   fn descend() { couple BTreeNode { with root { with child { with leaf { } } } } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn without_couple_same_class_nesting_is_deadlock() {
        let src = "lock root: BTreeNode\nlock child: BTreeNode\n\
                   fn descend() { with root { with child { } } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn couple_does_not_hide_cross_class_deadlock() {
        let src = "lock node: BTreeNode\nlock catalog: Catalog\n\
                   fn descend() { couple BTreeNode { with node { with catalog { } } } }\n\
                   fn reindex() { with catalog { with node { } } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn couple_while_holding_same_class_outside_is_error() {
        let ast =
            parser::parse("lock a: N\nlock b: N\nfn f() { with a { couple N { with b { } } } }")
                .unwrap();
        assert!(lower(&ast, "test").is_err());
    }
    #[test]
    fn couple_unknown_class_is_error() {
        let ast = parser::parse("fn f() { couple Nope { } }").unwrap();
        assert!(lower(&ast, "test").is_err());
    }
    #[test]
    fn opposite_direction_couples_are_deadlock() {
        let src = "lock a: Node\nlock b: Node\n\
                   fn fwd() { couple Node { with a { with b { } } } }\n\
                   fn bwd() { couple Node { with b { with a { } } } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn consistent_direction_couples_are_safe() {
        let src = "lock a: Node\nlock b: Node\n\
                   fn p() { couple Node { with a { with b { } } } }\n\
                   fn q() { couple Node { with a { with b { } } } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn single_instance_couple_reentrant_is_deadlock() {
        let src = "lock only: Node\nfn descend() { couple Node { with only { with only { } } } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }

    // --- P4: extern ---
    #[test]
    fn extern_contract_participates_in_cycle() {
        let src = "lock db: Db\nlock cache: Cache\n\
                   extern fn vendor_flush acquires Cache, Db\n\
                   fn write() { with db { with cache { } } }\n\
                   fn sync() { vendor_flush() }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn extern_contract_consistent_is_safe() {
        let src = "lock db: Db\nlock cache: Cache\n\
                   extern fn vendor_flush acquires Db, Cache\n\
                   fn write() { with db { with cache { } } }\n\
                   fn sync() { vendor_flush() }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn extern_as_callback_participates_in_cycle() {
        let src = "lock table: Table\nlock row: Row\n\
                   extern fn vendor acquires Row\n\
                   fn helper(body) { with table { body() } }\n\
                   fn import() { helper(vendor) }\n\
                   fn refresh() { with row { with table { } } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn extern_as_callback_consistent_is_safe() {
        let src = "lock table: Table\nlock row: Row\n\
                   extern fn vendor acquires Row\n\
                   fn helper(body) { with table { body() } }\n\
                   fn import() { helper(vendor) }\n\
                   fn other() { with table { with row { } } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn extern_with_no_contract_is_safe() {
        let src = "lock db: Db\nextern fn log_line\nfn f() { with db { log_line() } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn extern_unknown_class_is_error() {
        let ast = parser::parse("lock db: Db\nextern fn x acquires Nope").unwrap();
        assert!(lower(&ast, "test").is_err());
    }
    #[test]
    fn extern_name_colliding_with_fn_is_error() {
        let ast = parser::parse("extern fn dup\nfn dup() { }").unwrap();
        assert!(lower(&ast, "test").is_err());
    }

    // --- P5: scalable callbacks ---
    #[test]
    fn shared_callback_across_sites_stays_correct() {
        let src = "lock table: Table\nlock row: Row\n\
                   fn helper(body) { with table { body() } }\n\
                   fn lockrow() { with row { } }\n\
                   fn a() { helper(lockrow) }\n\
                   fn b() { helper(lockrow) }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn callback_undefined_arg_is_error() {
        let ast = parser::parse(
            "lock row: Row\nfn lockrow() { with row { } }\n\
             fn helper(body) { body(nope) }\nfn start() { helper(lockrow) }",
        )
        .unwrap();
        assert!(lower(&ast, "test").is_err());
    }
    #[test]
    fn callback_wrong_arity_is_error() {
        let ast = parser::parse(
            "lock row: Row\nfn lockrow() { with row { } }\n\
             fn helper(body) { body(lockrow) }\nfn start() { helper(lockrow) }",
        )
        .unwrap();
        assert!(lower(&ast, "test").is_err());
    }
    #[test]
    fn zero_cfa_known_false_positive_is_pinned() {
        let src = "lock a1: A\nlock a2: A\nlock b: B\n\
                   fn apply(cb) { cb() }\n\
                   fn get_b() { with b { } }\n\
                   fn get_a2() { with a2 { } }\n\
                   fn safe_path() { with a1 { apply(get_b) } }\n\
                   fn other_path() { apply(get_a2) }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }

    // --- P6: interrupt preemption ---
    #[test]
    fn isr_and_unmasked_task_share_lock_is_deadlock() {
        let src =
            "lock shared: Buf\nisr fn on_rx() { with shared { } }\nfn task() { with shared { } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn isr_shared_lock_masked_in_task_is_safe() {
        let src = "lock shared: Buf\nisr fn on_rx() { with shared { } }\nfn task() { mask { with shared { } } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn isr_on_different_lock_is_safe() {
        let src = "lock stat: Stat\nlock buf: Buf\n\
                   isr fn on_rx() { with stat { } }\nfn task() { with buf { } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn no_isr_means_no_preemption_machinery() {
        let src = "lock shared: Buf\nfn a() { with shared { } }\nfn b() { with shared { } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn isr_deadlock_through_a_call_is_caught() {
        let src = "lock shared: Buf\n\
                   fn touch() { with shared { } }\n\
                   isr fn on_rx() { touch() }\n\
                   fn task() { with shared { } }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn isr_lock_via_extern_unmasked_is_deadlock() {
        let src = "lock shared: Buf\nextern fn vendor_touch acquires Buf\n\
                   isr fn on_rx() { with shared { } }\nfn task() { vendor_touch() }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }
    #[test]
    fn isr_lock_via_extern_masked_is_safe() {
        let src = "lock shared: Buf\nextern fn vendor_touch acquires Buf\n\
                   isr fn on_rx() { with shared { } }\nfn task() { mask { vendor_touch() } }";
        assert!(matches!(verdict(src), Outcome::DeadlockFree { .. }));
    }
    #[test]
    fn isr_invoked_unmasked_as_subroutine_is_deadlock() {
        // P7 regression: a lazily-created (isr, unmasked) context must still be
        // analyzed so its task-side preemption edge is emitted.
        let src = "lock ring: RingBuf\nisr fn on_rx() { with ring { } }\nfn poll() { on_rx() }";
        assert!(matches!(verdict(src), Outcome::Deadlock { .. }));
    }

    // --- P7: summary-based scalable held-set analysis ---
    #[test]
    fn held_set_blowup_resolves_definitively() {
        // 2^k distinct held-sets reach `sink` under exact enumeration; the
        // may-held fixpoint analyzes it in polynomial time with a definitive
        // verdict (no [INCOMPLETE]).
        let k = 22;
        let mut src = String::from("lock target: T\n");
        for j in 0..k {
            src.push_str(&format!("lock l{j}: C{j}\n"));
        }
        src.push_str("fn sink() { with target { } }\n");
        for j in 0..k {
            src.push_str(&format!(
                "fn stage{j}() {{ with l{j} {{ stage{}() }} stage{}() }}\n",
                j + 1,
                j + 1
            ));
        }
        src.push_str(&format!("fn stage{k}() {{ sink() }}\n"));

        let ast = parser::parse(&src).unwrap();
        let prog = lower(&ast, "test").unwrap();
        assert!(
            prog.incomplete.is_none(),
            "must be definitive, not INCOMPLETE"
        );
        assert!(matches!(
            analyze(&prog).outcome,
            Outcome::DeadlockFree { .. }
        ));
    }
    #[test]
    fn held_set_blowup_still_finds_a_real_deadlock() {
        // Same staged fan-out, but the sink inverts an order another path uses,
        // so a real cycle exists and must still be found despite the blowup.
        let k = 22;
        let mut src = String::from("lock x: X\nlock y: Y\n");
        for j in 0..k {
            src.push_str(&format!("lock l{j}: C{j}\n"));
        }
        // sink: X then Y. inverter: Y then X. => X<Y and Y<X cycle.
        src.push_str("fn sink() { with x { with y { } } }\n");
        src.push_str("fn inverter() { with y { with x { } } }\n");
        for j in 0..k {
            src.push_str(&format!(
                "fn stage{j}() {{ with l{j} {{ stage{}() }} stage{}() }}\n",
                j + 1,
                j + 1
            ));
        }
        src.push_str(&format!("fn stage{k}() {{ sink() }}\n"));

        let ast = parser::parse(&src).unwrap();
        let prog = lower(&ast, "test").unwrap();
        assert!(
            prog.incomplete.is_none(),
            "must be definitive, not INCOMPLETE"
        );
        assert!(matches!(analyze(&prog).outcome, Outcome::Deadlock { .. }));
    }
}
