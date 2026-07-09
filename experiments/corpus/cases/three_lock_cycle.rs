// EXPECT: DEADLOCK
// DESC: a three-node cycle a<b, b<c, c<a -> no global order exists.
use std::sync::Mutex;
struct Sys3 { a: Mutex<i32>, b: Mutex<i32>, c: Mutex<i32> }
pub fn f1(s: &Sys3) { let x = s.a.lock().unwrap(); let y = s.b.lock().unwrap(); let _ = (*x, *y); }
pub fn f2(s: &Sys3) { let y = s.b.lock().unwrap(); let z = s.c.lock().unwrap(); let _ = (*y, *z); }
pub fn f3(s: &Sys3) { let z = s.c.lock().unwrap(); let x = s.a.lock().unwrap(); let _ = (*z, *x); }
