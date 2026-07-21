//! Reader for pre-lowered lock-acquisition events in JSON form.
//!
//! The MIR front-end (`experiments/whorl_lint`) analyzes real Rust and emits
//! the same `Event` list the Whorl frontend produces from `.whorl` source:
//!
//! ```text
//! {
//!   "name": "mir",
//!   "events": [
//!     { "function": "f", "site": "lib.rs:7", "acquires": "C", "held": ["B"] }
//!   ],
//!   "class_instances": { "C": 2, "B": 1 },
//!   "incomplete": "optional reason"
//! }
//! ```
//!
//! Parsing it here keeps the boundary clean: the nightly-only lint never links
//! the solver, and this crate stays zero-dependency, so the JSON is read by the
//! same kind of hand-written recursive-descent code as the surface language.

use std::collections::HashMap;

use crate::model::{Event, Program};

/// Parse the JSON emitted by the MIR front-end into a solver-ready [`Program`].
pub fn parse(src: &str) -> Result<Program, String> {
    let mut p = Json {
        s: src.as_bytes(),
        i: 0,
    };
    p.ws();
    let top = p.object()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("trailing data at byte {}", p.i));
    }

    let name = match top.get("name") {
        Some(Value::Str(n)) => n.clone(),
        _ => "events".to_string(),
    };

    let mut events = Vec::new();
    match top.get("events") {
        Some(Value::Arr(items)) => {
            for (idx, item) in items.iter().enumerate() {
                let Value::Obj(ev) = item else {
                    return Err(format!("events[{idx}] is not an object"));
                };
                events.push(Event {
                    function: str_field(ev, "function", idx)?,
                    site: str_field(ev, "site", idx)?,
                    acquires: str_field(ev, "acquires", idx)?,
                    held: match ev.get("held") {
                        Some(Value::Arr(hs)) => {
                            let mut held = Vec::new();
                            for h in hs {
                                match h {
                                    Value::Str(s) => held.push(s.clone()),
                                    _ => {
                                        return Err(format!(
                                            "events[{idx}].held contains a non-string"
                                        ))
                                    }
                                }
                            }
                            held
                        }
                        _ => return Err(format!("events[{idx}] is missing \"held\"")),
                    },
                });
            }
        }
        _ => return Err("top-level \"events\" array is missing".to_string()),
    }

    let mut class_instances = HashMap::new();
    if let Some(Value::Obj(ci)) = top.get("class_instances") {
        for (class, v) in ci {
            match v {
                Value::Num(n) => {
                    class_instances.insert(class.clone(), *n);
                }
                _ => return Err(format!("class_instances[\"{class}\"] is not a number")),
            }
        }
    }
    // Any class seen in an event but absent from the map is assumed to have at
    // least 2 instances. That is the conservative direction: the solver only
    // uses the count to soften the *explanation* (reentrancy vs cross-instance),
    // never the verdict.
    for e in &events {
        for c in e.held.iter().chain(std::iter::once(&e.acquires)) {
            class_instances.entry(c.clone()).or_insert(2);
        }
    }

    // Interprocedural held-sets. The MIR front-end is per-body: a guard held
    // across a call is invisible inside the callee, which would be a false
    // negative. It therefore also emits call edges (caller, callee, held at the
    // call site); here we run the same monotone entry-may fixpoint as the
    // .whorl frontend and fold each function's entry-may set into its events.
    let mut edges: Vec<(String, String, Vec<String>)> = Vec::new();
    if let Some(Value::Arr(calls)) = top.get("calls") {
        for (idx, c) in calls.iter().enumerate() {
            let Value::Obj(c) = c else {
                return Err(format!("calls[{idx}] is not an object"));
            };
            edges.push((
                str_field(c, "function", idx)?,
                str_field(c, "callee", idx)?,
                held_field(c, "calls", idx)?,
            ));
        }
    }

    // Indirect calls. A function may invoke a callable it received
    // (`param_calls`); which body that runs is decided by whoever passed it in
    // (`closure_args`). Joining the two resolves the call. An unresolved
    // indirect call made while holding locks loses ordering edges, so it must
    // force [INCOMPLETE] rather than a wrong [SAFE].
    let mut param_calls: Vec<(String, usize, Vec<String>)> = Vec::new();
    if let Some(Value::Arr(pcs)) = top.get("param_calls") {
        for (idx, c) in pcs.iter().enumerate() {
            let Value::Obj(c) = c else {
                return Err(format!("param_calls[{idx}] is not an object"));
            };
            param_calls.push((
                str_field(c, "function", idx)?,
                num_field(c, "param", idx)?,
                held_field(c, "param_calls", idx)?,
            ));
        }
    }
    let mut closure_args: Vec<(String, usize, String)> = Vec::new();
    if let Some(Value::Arr(cas)) = top.get("closure_args") {
        for (idx, c) in cas.iter().enumerate() {
            let Value::Obj(c) = c else {
                return Err(format!("closure_args[{idx}] is not an object"));
            };
            closure_args.push((
                str_field(c, "callee", idx)?,
                num_field(c, "param", idx)?,
                str_field(c, "closure", idx)?,
            ));
        }
    }

    let mut entry_may: HashMap<String, Vec<String>> = HashMap::new();
    let mut changed = true;
    while changed {
        changed = false;
        for (caller, callee, held) in &edges {
            let mut incoming = held.clone();
            if let Some(from_caller) = entry_may.get(caller) {
                incoming.extend(from_caller.iter().cloned());
            }
            changed |= merge(&mut entry_may, callee, incoming);
        }
        // resolve `g` calling its parameter #i to every closure passed there.
        for (g, i, held_at_call) in &param_calls {
            for (callee, param, closure) in &closure_args {
                if callee == g && param == i {
                    let mut incoming = held_at_call.clone();
                    if let Some(from_g) = entry_may.get(g) {
                        incoming.extend(from_g.iter().cloned());
                    }
                    changed |= merge(&mut entry_may, closure, incoming);
                }
            }
        }
    }
    for e in &mut events {
        if let Some(extra) = entry_may.get(&e.function) {
            for c in extra {
                if !e.held.contains(c) {
                    e.held.push(c.clone());
                    class_instances.entry(c.clone()).or_insert(2);
                }
            }
        }
    }

    // Fail closed: an indirect call under a held lock whose callee we could not
    // resolve, or an outright opaque call (fn pointer, trait object), means the
    // event list is missing edges. A deadlock found anyway is still real; the
    // ABSENCE of one is not conclusive.
    let mut unresolved: Option<String> = None;
    for (g, i, held_at_call) in &param_calls {
        // "Is a lock held here?" is an INTERPROCEDURAL question: a caller's lock
        // reaches this site through entry_may. Testing the local held-set alone
        // silently skips a callback invoked under a caller's lock.
        let mut effective = held_at_call.clone();
        if let Some(from_callers) = entry_may.get(g) {
            effective.extend(from_callers.iter().cloned());
        }
        if effective.is_empty() {
            continue; // genuinely nothing held: no ordering edge can be lost
        }
        // Resolution must be UNIVERSAL, not existential. "<unknown>" marks a
        // callable that reached this position unnameable; a sibling call passing
        // a real closure must not make the whole position look resolved.
        let any_named = closure_args
            .iter()
            .any(|(callee, p, cl)| callee == g && p == i && cl != "<unknown>");
        let any_unknown = closure_args
            .iter()
            .any(|(callee, p, cl)| callee == g && p == i && cl == "<unknown>");
        if !any_named || any_unknown {
            unresolved = Some(format!(
                "{g} calls its parameter #{i} while holding {effective:?}, and not \
                 every callable passed there could be resolved"
            ));
            break;
        }
    }
    if unresolved.is_none() {
        if let Some(Value::Arr(ocs)) = top.get("opaque_calls") {
            for (idx, c) in ocs.iter().enumerate() {
                let Value::Obj(c) = c else {
                    return Err(format!("opaque_calls[{idx}] is not an object"));
                };
                let f = str_field(c, "function", idx)?;
                let s = str_field(c, "site", idx)?;
                let mut effective = held_field(c, "opaque_calls", idx)?;
                if let Some(from_callers) = entry_may.get(&f) {
                    effective.extend(from_callers.iter().cloned());
                }
                if effective.is_empty() {
                    continue;
                }
                unresolved = Some(format!(
                    "unresolved indirect call (fn pointer, trait object or \
                     unnameable callable) at {s} in {f} while holding {effective:?}"
                ));
                break;
            }
        }
    }

    // The critical section is reentrant on a single core: entering it while
    // already inside it is not a deadlock, so it must never self-edge. The
    // interprocedural widening above can put "<critical-section>" into the
    // held-set of a nested entry; drop it there. Cross-class edges (a spinlock
    // held across it, or taken inside it) are untouched.
    for e in &mut events {
        if e.acquires == "<critical-section>" {
            e.held.retain(|h| h != "<critical-section>");
        }
    }

    let incomplete = match top.get("incomplete") {
        Some(Value::Str(r)) => Some(r.clone()),
        _ => unresolved,
    };

    Ok(Program {
        name,
        events,
        class_instances,
        incomplete,
    })
}

/// Union `incoming` into `entry_may[target]`; true if anything was added.
fn merge(
    entry_may: &mut HashMap<String, Vec<String>>,
    target: &str,
    incoming: Vec<String>,
) -> bool {
    let into = entry_may.entry(target.to_string()).or_default();
    let mut changed = false;
    for c in incoming {
        if !into.contains(&c) {
            into.push(c);
            changed = true;
        }
    }
    changed
}

fn held_field(obj: &HashMap<String, Value>, what: &str, idx: usize) -> Result<Vec<String>, String> {
    match obj.get("held") {
        Some(Value::Arr(hs)) => {
            let mut held = Vec::new();
            for h in hs {
                match h {
                    Value::Str(s) => held.push(s.clone()),
                    _ => return Err(format!("{what}[{idx}].held contains a non-string")),
                }
            }
            Ok(held)
        }
        _ => Err(format!("{what}[{idx}] is missing \"held\"")),
    }
}

fn num_field(obj: &HashMap<String, Value>, key: &str, idx: usize) -> Result<usize, String> {
    match obj.get(key) {
        Some(Value::Num(n)) => Ok(*n),
        _ => Err(format!("entry[{idx}] is missing number field \"{key}\"")),
    }
}

fn str_field(obj: &HashMap<String, Value>, key: &str, idx: usize) -> Result<String, String> {
    match obj.get(key) {
        Some(Value::Str(s)) => Ok(s.clone()),
        _ => Err(format!("events[{idx}] is missing string field \"{key}\"")),
    }
}

enum Value {
    Str(String),
    Num(usize),
    Arr(Vec<Value>),
    Obj(HashMap<String, Value>),
}

/// A minimal recursive-descent JSON reader for the shape above. Strings support
/// the standard escapes; numbers are non-negative integers, which is all the
/// format contains.
struct Json<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Json<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        self.ws();
        if self.i < self.s.len() && self.s[self.i] == b {
            self.i += 1;
            Ok(())
        } else {
            Err(format!(
                "expected '{}' at byte {}, found {:?}",
                b as char,
                self.i,
                self.s.get(self.i).map(|&c| c as char)
            ))
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'[') => self.array(),
            Some(b'{') => Ok(Value::Obj(self.object()?)),
            Some(c) if c.is_ascii_digit() => self.number(),
            other => Err(format!(
                "unexpected {:?} at byte {}",
                other.map(|c| c as char),
                self.i
            )),
        }
    }

    fn object(&mut self) -> Result<HashMap<String, Value>, String> {
        self.expect(b'{')?;
        let mut map = HashMap::new();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(map);
        }
        loop {
            let key = self.string()?;
            self.expect(b':')?;
            let val = self.value()?;
            map.insert(key, val);
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(map);
                }
                other => {
                    return Err(format!(
                        "expected ',' or '}}' at byte {}, found {:?}",
                        self.i,
                        other.map(|c| c as char)
                    ))
                }
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Value::Arr(items));
        }
        loop {
            items.push(self.value()?);
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Arr(items));
                }
                other => {
                    return Err(format!(
                        "expected ',' or ']' at byte {}, found {:?}",
                        self.i,
                        other.map(|c| c as char)
                    ))
                }
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            match self.s.get(self.i) {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    match self.s.get(self.i) {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'n') => out.push('\n'),
                        Some(b't') => out.push('\t'),
                        Some(b'r') => out.push('\r'),
                        Some(b'u') => {
                            let hex = self
                                .s
                                .get(self.i + 1..self.i + 5)
                                .ok_or("truncated \\u escape")?;
                            let hex = std::str::from_utf8(hex).map_err(|_| "bad \\u escape")?;
                            let code =
                                u32::from_str_radix(hex, 16).map_err(|_| "bad \\u escape")?;
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                            self.i += 4;
                        }
                        other => {
                            return Err(format!(
                                "unknown escape {:?} at byte {}",
                                other.map(|&c| c as char),
                                self.i
                            ))
                        }
                    }
                    self.i += 1;
                }
                Some(_) => {
                    // Multi-byte UTF-8 is copied through verbatim.
                    let start = self.i;
                    self.i += 1;
                    while self.i < self.s.len() && self.s[self.i] & 0xC0 == 0x80 {
                        self.i += 1;
                    }
                    out.push_str(
                        std::str::from_utf8(&self.s[start..self.i])
                            .map_err(|_| "invalid UTF-8 in string")?,
                    );
                }
            }
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|t| t.parse().ok())
            .map(Value::Num)
            .ok_or_else(|| format!("bad number at byte {start}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solver::{analyze, Outcome};

    const DEADLOCK_JSON: &str = r#"{
  "name": "mir",
  "events": [
    { "function": "transfer", "site": "lib.rs:7", "acquires": "Mutex<i64>::bal", "held": [] },
    { "function": "transfer", "site": "lib.rs:8", "acquires": "Mutex<i64>::bal", "held": ["Mutex<i64>::bal"] }
  ],
  "class_instances": {
    "Mutex<i64>::bal": 2
  }
}"#;

    #[test]
    fn parses_and_finds_the_two_account_deadlock() {
        let prog = parse(DEADLOCK_JSON).unwrap();
        assert_eq!(prog.events.len(), 2);
        assert_eq!(prog.class_instances["Mutex<i64>::bal"], 2);
        let report = analyze(&prog);
        assert!(matches!(report.outcome, Outcome::Deadlock { .. }));
        assert!(!report.self_loop_single_instance);
    }

    #[test]
    fn parses_a_safe_program() {
        let src = r#"{
  "events": [
    { "function": "p", "site": "a.rs:1", "acquires": "A", "held": [] },
    { "function": "p", "site": "a.rs:2", "acquires": "B", "held": ["A"] }
  ],
  "class_instances": { "A": 1, "B": 1 }
}"#;
        let prog = parse(src).unwrap();
        let report = analyze(&prog);
        assert!(matches!(report.outcome, Outcome::DeadlockFree { .. }));
    }

    #[test]
    fn missing_class_counts_default_to_two_instances() {
        let src = r#"{ "events": [
            { "function": "f", "site": "s:1", "acquires": "C", "held": ["C"] }
        ] }"#;
        let prog = parse(src).unwrap();
        assert_eq!(prog.class_instances["C"], 2);
        let report = analyze(&prog);
        assert!(matches!(report.outcome, Outcome::Deadlock { .. }));
        // Two assumed instances => cross-instance explanation, not reentrancy.
        assert!(!report.self_loop_single_instance);
    }

    #[test]
    fn call_edges_propagate_held_sets_into_callees() {
        // locker_a holds A and calls helper, which locks B; locker_b locks B
        // then A. Without the call edge this is (unsoundly) safe; with it the
        // helper's B acquire happens while A is held -> cycle A < B < A.
        let src = r#"{
  "events": [
    { "function": "locker_a", "site": "s:1", "acquires": "A", "held": [] },
    { "function": "helper",   "site": "s:2", "acquires": "B", "held": [] },
    { "function": "locker_b", "site": "s:3", "acquires": "B", "held": [] },
    { "function": "locker_b", "site": "s:4", "acquires": "A", "held": ["B"] }
  ],
  "class_instances": { "A": 1, "B": 1 },
  "calls": [
    { "function": "locker_a", "callee": "helper", "held": ["A"] }
  ]
}"#;
        let prog = parse(src).unwrap();
        let helper_event = prog.events.iter().find(|e| e.function == "helper").unwrap();
        assert_eq!(helper_event.held, vec!["A".to_string()]);
        let report = analyze(&prog);
        assert!(matches!(report.outcome, Outcome::Deadlock { .. }));
    }

    #[test]
    fn entry_may_is_transitive_over_call_chains() {
        // a holds L and calls b; b calls c; c locks M. The L-held context must
        // reach c through the chain.
        let src = r#"{
  "events": [
    { "function": "a", "site": "s:1", "acquires": "L", "held": [] },
    { "function": "c", "site": "s:2", "acquires": "M", "held": [] }
  ],
  "calls": [
    { "function": "b", "callee": "c", "held": [] },
    { "function": "a", "callee": "b", "held": ["L"] }
  ]
}"#;
        let prog = parse(src).unwrap();
        let c_event = prog.events.iter().find(|e| e.function == "c").unwrap();
        assert!(c_event.held.contains(&"L".to_string()));
    }

    #[test]
    fn closure_passed_to_a_callback_taker_is_resolved() {
        // with_a holds A and calls its parameter #2; path1 passes a closure that
        // locks B. Joining the two gives A < B, which closes a cycle with
        // path2's B < A. Without the join this reads SAFE (a false negative).
        let src = r#"{
  "events": [
    { "function": "with_a",             "site": "s:1", "acquires": "A", "held": [] },
    { "function": "path1::{closure#0}", "site": "s:2", "acquires": "B", "held": [] },
    { "function": "path2",              "site": "s:3", "acquires": "B", "held": [] },
    { "function": "path2",              "site": "s:4", "acquires": "A", "held": ["B"] }
  ],
  "param_calls":  [ { "function": "with_a", "param": 2, "held": ["A"] } ],
  "closure_args": [ { "function": "path1", "callee": "with_a", "param": 2,
                      "closure": "path1::{closure#0}" } ]
}"#;
        let prog = parse(src).unwrap();
        let in_closure = prog
            .events
            .iter()
            .find(|e| e.function == "path1::{closure#0}")
            .unwrap();
        assert!(in_closure.held.contains(&"A".to_string()));
        assert!(prog.incomplete.is_none(), "the call was resolved");
        assert!(matches!(analyze(&prog).outcome, Outcome::Deadlock { .. }));
    }

    #[test]
    fn unresolved_callback_under_a_held_lock_forces_incomplete() {
        // Nobody passes a callable to g's parameter, so the body it runs is
        // unknown. Edges are lost, so SAFE must not be claimed.
        let src = r#"{
  "events": [ { "function": "g", "site": "s:1", "acquires": "L", "held": [] } ],
  "param_calls": [ { "function": "g", "param": 1, "held": ["L"] } ]
}"#;
        let prog = parse(src).unwrap();
        assert!(prog.incomplete.is_some());
    }

    #[test]
    fn one_benign_closure_does_not_whitewash_an_unknown_callable() {
        // Adversarial review finding: the resolution test used to be existential,
        // so a do-nothing closure passed at (with_a, #2) made the position look
        // resolved even though another caller forwarded an unnameable callable
        // to the same position. Resolution must be universal.
        let src = r#"{
  "events": [ { "function": "with_a", "site": "s:1", "acquires": "A", "held": [] } ],
  "param_calls":  [ { "function": "with_a", "param": 2, "held": ["A"] } ],
  "closure_args": [
    { "function": "benign",  "callee": "with_a", "param": 2, "closure": "benign::{closure#0}" },
    { "function": "forward", "callee": "with_a", "param": 2, "closure": "<unknown>" }
  ]
}"#;
        let prog = parse(src).unwrap();
        assert!(
            prog.incomplete.is_some(),
            "an unknown callable at the same position must still force incomplete"
        );
    }

    #[test]
    fn a_lock_held_by_the_caller_counts_at_an_unresolved_callback() {
        // Adversarial review finding: the emptiness test used to read the LOCAL
        // held-set only. run_hook holds nothing itself, but every caller reaches
        // it holding L, which the entry-may fixpoint knows.
        let src = r#"{
  "events": [ { "function": "under_l", "site": "s:1", "acquires": "L", "held": [] } ],
  "calls":       [ { "function": "under_l", "callee": "run_hook", "held": ["L"] } ],
  "param_calls": [ { "function": "run_hook", "param": 1, "held": [] } ]
}"#;
        let prog = parse(src).unwrap();
        let reason = prog.incomplete.expect("caller's lock must count");
        assert!(reason.contains("run_hook"));
    }

    #[test]
    fn an_opaque_call_under_a_callers_lock_counts_too() {
        let src = r#"{
  "events": [ { "function": "under_l", "site": "s:1", "acquires": "L", "held": [] } ],
  "calls":        [ { "function": "under_l", "callee": "dispatch", "held": ["L"] } ],
  "opaque_calls": [ { "function": "dispatch", "site": "d.rs:3", "held": [] } ]
}"#;
        let prog = parse(src).unwrap();
        assert!(prog.incomplete.is_some());
    }

    #[test]
    fn opaque_indirect_call_forces_incomplete() {
        let src = r#"{
  "events": [],
  "opaque_calls": [ { "function": "f", "site": "a.rs:9", "held": ["L"] } ]
}"#;
        let prog = parse(src).unwrap();
        let reason = prog.incomplete.expect("must be incomplete");
        assert!(reason.contains("a.rs:9"));
    }

    #[test]
    fn callback_with_nothing_held_does_not_force_incomplete() {
        // No lock is held at the indirect call, so no ordering edge is lost:
        // the callee's own internal ordering is captured when its body is
        // analyzed on its own.
        let src = r#"{
  "events": [ { "function": "g", "site": "s:1", "acquires": "L", "held": [] } ],
  "param_calls": [ { "function": "g", "param": 1, "held": [] } ]
}"#;
        let prog = parse(src).unwrap();
        assert!(prog.incomplete.is_none());
    }

    #[test]
    fn critical_section_vs_spinlock_cycle() {
        // cs_then_spin: enter CS, then lock spin inside the closure -> CS < spin.
        // spin_then_cs: hold spin, then enter CS -> spin < CS. Cycle => deadlock.
        let src = r#"{
  "events": [
    { "function": "cs_then_spin",             "site": "s:1", "acquires": "<critical-section>", "held": [] },
    { "function": "cs_then_spin::{closure#0}", "site": "s:2", "acquires": "spin",               "held": [] },
    { "function": "spin_then_cs",              "site": "s:3", "acquires": "spin",               "held": [] },
    { "function": "spin_then_cs",              "site": "s:4", "acquires": "<critical-section>", "held": ["spin"] }
  ],
  "calls": [
    { "function": "cs_then_spin", "callee": "cs_then_spin::{closure#0}", "held": ["<critical-section>"] }
  ]
}"#;
        let prog = parse(src).unwrap();
        let report = analyze(&prog);
        assert!(matches!(report.outcome, Outcome::Deadlock { .. }));
    }

    #[test]
    fn nested_critical_sections_are_reentrant() {
        // A `with` inside a `with`: the inner entry inherits <critical-section>
        // from the outer via the call edge, but re-entering is not a deadlock.
        let src = r#"{
  "events": [
    { "function": "outer::{closure#0}", "site": "s:2", "acquires": "<critical-section>", "held": [] }
  ],
  "calls": [
    { "function": "f",                  "callee": "outer::{closure#0}", "held": ["<critical-section>"] }
  ]
}"#;
        let prog = parse(src).unwrap();
        let e = &prog.events[0];
        assert!(!e.held.contains(&"<critical-section>".to_string()));
        let report = analyze(&prog);
        assert!(matches!(report.outcome, Outcome::DeadlockFree { .. }));
    }

    #[test]
    fn incomplete_reason_is_carried_through() {
        let src = r#"{ "events": [], "incomplete": "budget" }"#;
        let prog = parse(src).unwrap();
        assert_eq!(prog.incomplete.as_deref(), Some("budget"));
    }

    #[test]
    fn escapes_and_rejects() {
        let src = r#"{ "events": [
            { "function": "a\"b\\c", "site": "s\n:1", "acquires": "C", "held": [] }
        ] }"#;
        let prog = parse(src).unwrap();
        assert_eq!(prog.events[0].function, "a\"b\\c");
        assert!(parse("{").is_err());
        assert!(parse("{}").is_err()); // no events array
        assert!(parse("{ \"events\": [ 1 ] }").is_err());
    }
}
