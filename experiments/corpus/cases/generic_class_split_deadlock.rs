// EXPECT: DEADLOCK
// DESC: adversarial-review finding. Class symbols are rendered TYPE TEXT, and a
//       generic body is analyzed un-monomorphized. So `Pair<T>` renders a lock
//       as ...Mutex<T> while the concrete body renders the same lock as
//       ...Mutex<i32> -- one lock, two symbols, and the a<b / b<a ring never
//       closes. A polymorphic symbol now fails closed.
use std::sync::Mutex;
pub struct Pair<T> { pub a: Mutex<T>, pub b: Mutex<T> }
pub fn lock_a_then_b<T>(p: &Pair<T>) {
    let x = p.a.lock().unwrap();
    let y = p.b.lock().unwrap();
    let _ = (&*x, &*y);
}
pub fn lock_b_then_a(p: &Pair<i32>) {
    let y = p.b.lock().unwrap();
    let x = p.a.lock().unwrap();
    let _ = (*x, *y);
}
