// EXPECT: SAFE
// DESC: guard released via std::mem::drop before the second acquire. ADVERSARIAL
//       vs our PoC: mem::drop MOVES the guard into a call, so no visible
//       drop(_g) terminator remains; a tool keying only on Drop terminators
//       will over-hold and report a false positive. Sound, but imprecise.
use std::sync::Mutex;
struct Account { bal: Mutex<i64> }
pub fn seq(a: &Account, b: &Account) {
    let x = a.bal.lock().unwrap();
    let _ = *x;
    drop(x);
    let y = b.bal.lock().unwrap();
    let _ = *y;
}
