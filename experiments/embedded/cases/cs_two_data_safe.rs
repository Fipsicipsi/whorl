// EXPECT: SAFE
// DESC: two critical_section::Mutex values accessed in OPPOSITE orders in two
//       functions -- but both only via borrow(cs) inside one critical section.
//       borrow(cs) is a data access under the already-held section, not a lock
//       acquisition, so there is no ordering between the two Mutexes and no
//       deadlock. This is the common, correct usage; it must stay clean.
use core::cell::Cell;
use critical_section::Mutex;
static A: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));
static B: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));
pub fn f() { critical_section::with(|cs| { A.borrow(cs).set(1); B.borrow(cs).set(2); }); }
pub fn g() { critical_section::with(|cs| { B.borrow(cs).set(3); A.borrow(cs).set(4); }); }
