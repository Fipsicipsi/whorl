// EXPECT: DEADLOCK
// DESC: the A < B edge exists ONLY inside a closure that is invoked while A is
//       held. with_a locks A and calls its closure parameter; path1 passes a
//       closure that locks B, so A < B. path2 takes B then A directly, so
//       B < A. A per-body analysis that cannot see through the indirect call
//       f() misses the A < B edge entirely and reports SAFE -- a false negative.
use std::sync::Mutex;
struct Sys { a: Mutex<i32>, b: Mutex<i32> }

fn with_a<F: FnOnce()>(s: &Sys, f: F) {
    let _g = s.a.lock().unwrap();
    f();
}

pub fn path1(s: &Sys) {
    with_a(s, || {
        let _h = s.b.lock().unwrap();
    });
}

pub fn path2(s: &Sys) {
    let _h = s.b.lock().unwrap();
    let _g = s.a.lock().unwrap();
}
