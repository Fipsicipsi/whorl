// EXPECT: DEADLOCK
// DESC: the cycle spans a call: locker_a locks A and CALLS helper, which locks
//       B while A is still held (invisible inside helper's own body). locker_b
//       locks B then A directly. A sound analysis needs interprocedural
//       held-sets to see the A < B edge; a per-body analysis reports SAFE here,
//       which is a false negative.
use std::sync::Mutex;
pub struct Sys { pub a: Mutex<i32>, pub b: Mutex<i32> }

fn helper(s: &Sys) {
    let y = s.b.lock().unwrap();
    let _ = *y;
}

pub fn locker_a(s: &Sys) {
    let x = s.a.lock().unwrap();
    helper(s);
    let _ = *x;
}

pub fn locker_b(s: &Sys) {
    let y = s.b.lock().unwrap();
    let x = s.a.lock().unwrap();
    let _ = (*x, *y);
}
