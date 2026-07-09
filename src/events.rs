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

    let incomplete = match top.get("incomplete") {
        Some(Value::Str(r)) => Some(r.clone()),
        _ => None,
    };

    Ok(Program {
        name,
        events,
        class_instances,
        incomplete,
    })
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
