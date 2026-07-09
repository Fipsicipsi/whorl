// EXPECT: SAFE
// DESC: same class as the two-account case, but each guard is dropped before the
//       next acquire, so the held-set is empty (held-set is liveness, not lexical).
use std::sync::Mutex;
struct Account { bal: Mutex<i64> }
pub fn seq(a: &Account, b: &Account) {
    { let x = a.bal.lock().unwrap(); let _ = *x; }
    { let y = b.bal.lock().unwrap(); let _ = *y; }
}
