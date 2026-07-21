// EXPECT: DEADLOCK
// DESC: adversarial-review finding. `a()` reaches lock A through the field
//       path, `via_accessor()` reaches the SAME lock through a method that
//       returns &Mutex. If those two routes get different class symbols, the
//       cycle A < B (here) and B < A (there) never closes and the tool reports
//       SAFE -- a false negative caused by SPLITTING one physical lock into two
//       classes. Coarsening a class is sound; splitting one is not.
use std::sync::Mutex;
pub struct Sys { a: Mutex<i32>, b: Mutex<i32> }
impl Sys {
    fn lock_a(&self) -> &Mutex<i32> { &self.a }
}
pub fn field_route(s: &Sys) {
    let _x = s.a.lock().unwrap();
    let _y = s.b.lock().unwrap();
}
pub fn accessor_route(s: &Sys) {
    let _y = s.b.lock().unwrap();
    let _x = s.lock_a().lock().unwrap();
}
