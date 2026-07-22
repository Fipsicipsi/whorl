// EXPECT: SAFE
// DESC: two DISTINCT statics of the same type, always taken in the same order.
//       A class derived from the receiver's TYPE alone merges them, so the
//       consistent nesting reads as a same-class self-edge -- a false positive.
//       Statics are named, so the class is derived from the static itself.
use std::sync::Mutex;
static A: Mutex<i32> = Mutex::new(0);
static B: Mutex<i32> = Mutex::new(0);
pub fn f() { let x = A.lock().unwrap(); let y = B.lock().unwrap(); let _ = (*x, *y); }
pub fn g() { let x = A.lock().unwrap(); let y = B.lock().unwrap(); let _ = (*x, *y); }
