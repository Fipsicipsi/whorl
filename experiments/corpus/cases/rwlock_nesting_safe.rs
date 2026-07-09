// EXPECT: SAFE
// DESC: a Mutex and an RwLock acquired in one consistent order across functions;
//       a clean cross-class nesting must NOT be flagged.
use std::sync::{Mutex, RwLock};
struct S { m: Mutex<i64>, r: RwLock<i32> }
pub fn a(s: &S) { let g = s.m.lock().unwrap(); let rr = s.r.read().unwrap();  let _ = (*g, *rr); }
pub fn b(s: &S) { let g = s.m.lock().unwrap(); let rr = s.r.write().unwrap(); let _ = (*g, *rr); }
