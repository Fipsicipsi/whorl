// EXPECT: SAFE
// DESC: two distinct classes acquired in the same order in both functions.
use std::sync::Mutex;
struct Sys { a: Mutex<i32>, b: Mutex<i32> }
pub fn p(s: &Sys) { let x = s.a.lock().unwrap(); let y = s.b.lock().unwrap(); let _ = (*x, *y); }
pub fn q(s: &Sys) { let x = s.a.lock().unwrap(); let y = s.b.lock().unwrap(); let _ = (*x, *y); }
