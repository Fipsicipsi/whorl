// EXPECT: DEADLOCK
// DESC: adversarial-review finding. `W` has a user-defined Deref returning a
//       reference to a FIELD, so `w.lock()` and `w.inner.lock()` are the SAME
//       physical mutex. Rendering deref(x) as "(*x)" invents a symbol that can
//       never equal the field path, splitting one lock into two graph nodes so
//       the AB/BA cycle never closes. The deref shortcut is now restricted to
//       pointee-preserving wrappers (Arc/Rc/Box/Lazy); anything else fails
//       closed.
use std::ops::Deref;
use std::sync::Mutex;
pub struct W { pub inner: Mutex<i32> }
impl Deref for W {
    type Target = Mutex<i32>;
    fn deref(&self) -> &Mutex<i32> { &self.inner }
}
pub struct S { pub a: W, pub b: Mutex<i32> }
pub fn t1(s: &S) {
    let g = s.a.lock().unwrap();
    let h = s.b.lock().unwrap();
    let _ = (*g, *h);
}
pub fn t2(s: &S) {
    let h = s.b.lock().unwrap();
    let g = s.a.inner.lock().unwrap();
    let _ = (*g, *h);
}
