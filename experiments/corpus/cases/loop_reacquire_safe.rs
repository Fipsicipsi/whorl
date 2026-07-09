// EXPECT: SAFE
// DESC: lock acquired and dropped once per loop iteration; never two held at
//       once. Exercises the dataflow fixpoint over the loop back-edge.
use std::sync::Mutex;
struct S { m: Mutex<i32> }
pub fn pump(s: &S, n: u32) {
    for _ in 0..n {
        let g = s.m.lock().unwrap();
        let _ = *g;
    }
}
