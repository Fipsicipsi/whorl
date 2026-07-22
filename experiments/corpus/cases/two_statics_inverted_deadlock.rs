// EXPECT: DEADLOCK
// DESC: the same two statics, taken in OPPOSITE orders. Distinguishing them by
//       identity must not lose the real cycle: A < B here and B < A there.
use std::sync::Mutex;
static A: Mutex<i32> = Mutex::new(0);
static B: Mutex<i32> = Mutex::new(0);
pub fn f() { let x = A.lock().unwrap(); let y = B.lock().unwrap(); let _ = (*x, *y); }
pub fn g() { let y = B.lock().unwrap(); let x = A.lock().unwrap(); let _ = (*x, *y); }
