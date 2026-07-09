// EXPECT: DEADLOCK
// DESC: the same mutex locked twice on one path (std Mutex is not reentrant);
//       lockbud's classic DoubleLock case, self-deadlock.
use std::sync::Mutex;
struct S { m: Mutex<i32> }
pub fn double(s: &S) {
    let a = s.m.lock().unwrap();
    let b = s.m.lock().unwrap();
    let _ = (*a, *b);
}
