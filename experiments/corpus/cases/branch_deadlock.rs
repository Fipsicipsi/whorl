// EXPECT: DEADLOCK
// DESC: the inner lock is acquired (while the outer is held) only on one branch;
//       a sound may-be-held dataflow must still see the outer as held there.
use std::sync::Mutex;
struct Account { bal: Mutex<i64> }
pub fn branchy(a: &Account, b: &Account, c: bool) {
    let g = a.bal.lock().unwrap();
    if c {
        let h = b.bal.lock().unwrap();
        let _ = *h;
    }
    let _ = *g;
}
