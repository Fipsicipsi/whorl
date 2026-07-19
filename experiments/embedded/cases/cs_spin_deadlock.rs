// EXPECT: DEADLOCK
// DESC: single-core critical-section vs spinlock inversion. cs_then_spin enters
//       the critical section and takes a spinlock inside it (CS < spin);
//       spin_then_cs holds the spinlock and enters the critical section
//       (spin < CS). The cycle is a real deadlock: an interrupt can fire while
//       spin_then_cs holds the spinlock, and the handler path needs the spinlock
//       while the critical section is masked.
use core::cell::Cell;
use critical_section::Mutex;
static D: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));
static SPIN: spin::Mutex<u32> = spin::Mutex::new(0);
pub fn cs_then_spin() {
    critical_section::with(|cs| {
        let g = SPIN.lock();
        D.borrow(cs).set(*g);
    });
}
pub fn spin_then_cs() {
    let g = SPIN.lock();
    critical_section::with(|cs| {
        D.borrow(cs).set(*g);
    });
}
