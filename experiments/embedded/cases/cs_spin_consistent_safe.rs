// EXPECT: SAFE
// DESC: the same two primitives, but both paths take them in one order
//       (critical section, then spinlock). A single global order exists.
use core::cell::Cell;
use critical_section::Mutex;
static D: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));
static SPIN: spin::Mutex<u32> = spin::Mutex::new(0);
pub fn f() {
    critical_section::with(|cs| { let g = SPIN.lock(); D.borrow(cs).set(*g); });
}
pub fn g() {
    critical_section::with(|cs| { let g = SPIN.lock(); D.borrow(cs).set(*g); });
}
