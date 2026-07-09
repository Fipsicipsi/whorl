// EXPECT: DEADLOCK
// DESC: classic two-account transfer; both locks are class Account.bal and the
//       first guard is still held at the second acquire (cross-instance, same class).
use std::sync::Mutex;
struct Account { bal: Mutex<i64> }
pub fn transfer(from: &Account, to: &Account) {
    let f = from.bal.lock().unwrap();
    let t = to.bal.lock().unwrap();
    let _ = (*f, *t);
}
