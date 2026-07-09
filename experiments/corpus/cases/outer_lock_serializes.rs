// EXPECT: SAFE
// DESC: two same-class locks are only ever taken while holding a dedicated
//       outer lock, so the inner acquisitions are serialized and no concurrent
//       inversion can happen. The CLASSIC Havender false positive: a strict
//       class partial order rejects this correct program. Whorl's honest answer
//       is the escape hatch (with ordered(..) / trusted annotation), not a
//       changed verdict.
use std::sync::Mutex;
struct Bank { guard: Mutex<()>, a: Mutex<i64>, b: Mutex<i64> }
pub fn xfer(bank: &Bank) {
    let g = bank.guard.lock().unwrap();
    let x = bank.a.lock().unwrap();
    let y = bank.b.lock().unwrap();
    let _ = (&*g, *x, *y);
}
pub fn refx(bank: &Bank) {
    let g = bank.guard.lock().unwrap();
    let y = bank.b.lock().unwrap();
    let x = bank.a.lock().unwrap();
    let _ = (&*g, *x, *y);
}
