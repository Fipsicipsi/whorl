use std::sync::Mutex;
struct Account { bal: Mutex<i64> }

// Real two-account deadlock: 'to.bal' acquired while 'from.bal' (SAME class
// Account.bal) is still held -> self-cycle -> DEADLOCK.
pub fn transfer(from: &Account, to: &Account) {
    let f = from.bal.lock().unwrap();
    let t = to.bal.lock().unwrap();
    let _ = (*f, *t);
}

// SAME class, but each guard is dropped before the next lock (held-set empty at
// the 2nd acquire) -> SAFE. Verdict differs only by GUARD LIVENESS.
pub fn sequential(a: &Account, b: &Account) {
    { let x = a.bal.lock().unwrap(); let _ = *x; }
    { let y = b.bal.lock().unwrap(); let _ = *y; }
}
