//! The analytic heart of Whorl - hand-written, zero dependencies.
//!
//! Havender's theorem: if every lock acquisition respects a single global
//! partial order over lock classes, no set of threads can form a circular
//! wait, so lock-ordering deadlock is impossible. We therefore:
//!
//!   1. Turn each acquisition of class `C` while holding class `H` into an
//!      ordering edge `H -> C` ("H must be acquired before C"), witnessed by
//!      the acquisition site. When `H == C` this is a self-edge: holding a lock
//!      of a class while acquiring another lock of the same class, which (for a
//!      class with two or more instances) is exactly the two-account inversion.
//!   2. Accumulate the edges into a directed graph over lock classes.
//!   3. If the graph is acyclic (a topological order exists), that order *is* a
//!      valid global lock order and the program is deadlock-free.
//!   4. If it has a cycle, that cycle is a lock-ordering inversion - a concrete
//!      chain of sites that order the classes into a ring. We report it as the
//!      witness.
//!
//! Iteration over the edge set is sorted so the chosen witness/order is stable
//! across runs (std hash maps iterate in a randomized order).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use crate::model::Program;

/// One ordering constraint `before < after`, plus the acquisition site that
/// induced it. This is the witness we report on failure.
#[derive(Clone)]
pub struct Constraint {
    pub before: String,
    pub after: String,
    pub function: String,
    pub site: String,
}

/// The verdict for one program.
pub enum Outcome {
    /// Acyclic - a global lock order exists; deadlock-free by Havender.
    DeadlockFree { order: Vec<String> },
    /// A circular wait is possible. `cycle` is the chain of constraints that
    /// close the ring (the last `after` equals the first `before`).
    Deadlock { cycle: Vec<Constraint> },
}

pub struct Report {
    pub program: String,
    pub event_count: usize,
    pub class_count: usize,
    pub constraint_count: usize,
    pub outcome: Outcome,
    /// True when the verdict is a *self-edge* deadlock on a class that has only
    /// one declared instance - i.e. reentrant re-acquisition rather than a
    /// cross-instance inversion. Still reported (a non-reentrant lock would
    /// self-deadlock), but explained differently.
    pub self_loop_single_instance: bool,
}

fn intern(idx: &mut HashMap<String, usize>, names: &mut Vec<String>, s: &str) -> usize {
    if let Some(&i) = idx.get(s) {
        i
    } else {
        let i = names.len();
        names.push(s.to_string());
        idx.insert(s.to_string(), i);
        i
    }
}

/// Build the class-ordering graph and check it for cycles.
pub fn analyze(prog: &Program) -> Report {
    let mut idx: HashMap<String, usize> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut witness: HashMap<(usize, usize), Constraint> = HashMap::new();

    for ev in &prog.events {
        let after = intern(&mut idx, &mut names, &ev.acquires);
        for h in &ev.held {
            let before = intern(&mut idx, &mut names, h);
            witness
                .entry((before, after))
                .or_insert_with(|| Constraint {
                    before: h.clone(),
                    after: ev.acquires.clone(),
                    function: ev.function.clone(),
                    site: ev.site.clone(),
                });
        }
    }

    let n = names.len();

    // Deterministic edge order so the reported witness/order is reproducible.
    let mut keys: Vec<(usize, usize)> = witness.keys().copied().collect();
    keys.sort_unstable();

    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg: Vec<usize> = vec![0; n];
    let mut self_loop: Option<usize> = None;
    for &(before, after) in &keys {
        if before == after {
            if self_loop.is_none() {
                self_loop = Some(before);
            }
            continue;
        }
        adj[before].push(after);
        indeg[after] += 1;
    }

    let mut self_loop_single_instance = false;

    let outcome = if let Some(s) = self_loop {
        let class = &names[s];
        self_loop_single_instance = prog.class_instances.get(class).copied().unwrap_or(2) < 2;
        Outcome::Deadlock {
            cycle: vec![witness[&(s, s)].clone()],
        }
    } else {
        // Kahn's algorithm: remove zero-in-degree classes until none remain.
        let mut indeg2 = indeg.clone();
        let mut queue: Vec<usize> = (0..n).filter(|&i| indeg2[i] == 0).collect();
        let mut order: Vec<usize> = Vec::with_capacity(n);
        let mut head = 0;
        while head < queue.len() {
            let u = queue[head];
            head += 1;
            order.push(u);
            for &v in &adj[u] {
                indeg2[v] -= 1;
                if indeg2[v] == 0 {
                    queue.push(v);
                }
            }
        }

        if order.len() == n {
            Outcome::DeadlockFree {
                order: order.into_iter().map(|i| names[i].clone()).collect(),
            }
        } else {
            let processed: HashSet<usize> = order.into_iter().collect();
            let remaining: HashSet<usize> = (0..n).filter(|i| !processed.contains(i)).collect();
            let ring = find_cycle(&adj, &remaining);
            let mut cycle = Vec::with_capacity(ring.len());
            for k in 0..ring.len() {
                let from = ring[k];
                let to = ring[(k + 1) % ring.len()];
                if let Some(c) = witness.get(&(from, to)) {
                    cycle.push(c.clone());
                }
            }
            Outcome::Deadlock { cycle }
        }
    };

    Report {
        program: prog.name.clone(),
        event_count: prog.events.len(),
        class_count: n,
        constraint_count: witness.len(),
        outcome,
        self_loop_single_instance,
    }
}

/// Render the class-ordering graph as Graphviz DOT, with the edges of a
/// reported deadlock cycle highlighted in red. Read-only; the verdict itself is
/// produced by [`analyze`].
pub fn to_dot(prog: &Program) -> String {
    let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
    for ev in &prog.events {
        for h in &ev.held {
            edges.insert((h.clone(), ev.acquires.clone()));
        }
    }
    let cycle: HashSet<(String, String)> = match analyze(prog).outcome {
        Outcome::Deadlock { cycle } => cycle.into_iter().map(|c| (c.before, c.after)).collect(),
        Outcome::DeadlockFree { .. } => HashSet::new(),
    };

    let mut out = String::new();
    out.push_str("digraph whorl {\n  rankdir=LR;\n  node [shape=box, fontname=\"monospace\"];\n");
    for (a, b) in &edges {
        if cycle.contains(&(a.clone(), b.clone())) {
            let _ = writeln!(out, "  {a:?} -> {b:?} [color=red, penwidth=2];");
        } else {
            let _ = writeln!(out, "  {a:?} -> {b:?};");
        }
    }
    out.push_str("}\n");
    out
}

/// Find one cycle (a node ring) confined to `allowed`, via an *iterative* DFS
/// (no recursion, so a long cycle cannot overflow the stack). The closing edge
/// is `last -> first`.
fn find_cycle(adj: &[Vec<usize>], allowed: &HashSet<usize>) -> Vec<usize> {
    let mut starts: Vec<usize> = allowed.iter().copied().collect();
    starts.sort_unstable();
    let mut visited: HashSet<usize> = HashSet::new();
    for start in starts {
        if visited.contains(&start) {
            continue;
        }
        if let Some(ring) = dfs_iter(adj, allowed, start, &mut visited) {
            return ring;
        }
    }
    Vec::new()
}

fn dfs_iter(
    adj: &[Vec<usize>],
    allowed: &HashSet<usize>,
    start: usize,
    visited: &mut HashSet<usize>,
) -> Option<Vec<usize>> {
    // Each stack frame is (node, index of next neighbour to try).
    let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
    let mut path: Vec<usize> = vec![start];
    let mut on_path: HashSet<usize> = HashSet::new();
    on_path.insert(start);
    visited.insert(start);

    while let Some(&(u, _)) = stack.last() {
        let i = stack.last().unwrap().1;
        if i < adj[u].len() {
            stack.last_mut().unwrap().1 += 1;
            let v = adj[u][i];
            if !allowed.contains(&v) {
                continue;
            }
            if on_path.contains(&v) {
                let pos = path.iter().position(|&x| x == v).unwrap();
                return Some(path[pos..].to_vec());
            }
            if !visited.contains(&v) {
                visited.insert(v);
                on_path.insert(v);
                path.push(v);
                stack.push((v, 0));
            }
        } else {
            stack.pop();
            on_path.remove(&u);
            path.pop();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, Program};

    fn ev(function: &str, site: &str, held: &[&str], acquires: &str) -> Event {
        Event {
            function: function.into(),
            site: site.into(),
            held: held.iter().map(|s| s.to_string()).collect(),
            acquires: acquires.into(),
        }
    }

    fn prog(events: Vec<Event>, instances: &[(&str, usize)]) -> Program {
        Program {
            name: "t".into(),
            events,
            class_instances: instances.iter().map(|(c, n)| (c.to_string(), *n)).collect(),
            incomplete: None,
        }
    }

    #[test]
    fn two_account_transfer_deadlocks() {
        let p = prog(
            vec![
                ev("transfer", "t:7", &[], "Account"),
                ev("transfer", "t:8", &["Account"], "Account"),
            ],
            &[("Account", 2)],
        );
        let r = analyze(&p);
        assert!(matches!(r.outcome, Outcome::Deadlock { .. }));
        assert!(!r.self_loop_single_instance);
    }

    #[test]
    fn single_instance_self_acquire_is_flagged_but_marked() {
        let p = prog(
            vec![
                ev("a", "t:1", &[], "Singleton"),
                ev("b", "t:2", &["Singleton"], "Singleton"),
            ],
            &[("Singleton", 1)],
        );
        let r = analyze(&p);
        assert!(matches!(r.outcome, Outcome::Deadlock { .. }));
        assert!(r.self_loop_single_instance);
    }

    #[test]
    fn three_lock_cycle_deadlocks() {
        let p = prog(
            vec![
                ev("f", "f:3", &["A"], "B"),
                ev("g", "g:5", &["B"], "C"),
                ev("h", "h:9", &["C"], "A"),
            ],
            &[("A", 1), ("B", 1), ("C", 1)],
        );
        match analyze(&p).outcome {
            Outcome::Deadlock { cycle } => assert_eq!(cycle.len(), 3),
            _ => panic!("expected a deadlock"),
        }
    }

    #[test]
    fn consistent_hierarchy_is_safe_and_ordered() {
        let p = prog(
            vec![
                ev("open", "b:12", &["BankLock"], "Account"),
                ev("post", "b:20", &["BankLock", "Account"], "AuditLog"),
                ev("post", "b:21", &["Account"], "AuditLog"),
            ],
            &[("BankLock", 1), ("Account", 1), ("AuditLog", 1)],
        );
        match analyze(&p).outcome {
            Outcome::DeadlockFree { order } => {
                let pos = |c: &str| order.iter().position(|x| x == c).unwrap();
                assert!(pos("BankLock") < pos("Account"));
                assert!(pos("Account") < pos("AuditLog"));
            }
            _ => panic!("expected deadlock-free"),
        }
    }
}
