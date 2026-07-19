// EXPECT: SAFE
// DESC: a critical section entered inside a critical section. On a single core
//       this is reentrant (interrupts are simply kept masked), so it must not
//       self-deadlock.
use core::cell::Cell;
use critical_section::Mutex;
static D: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));
pub fn f() {
    critical_section::with(|_outer| {
        critical_section::with(|cs| {
            D.borrow(cs).set(1);
        });
    });
}
