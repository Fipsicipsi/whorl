// EXPECT: DEADLOCK
// DESC: adversarial-review finding. `pick` has TWO return paths: one is a
//       statement `&self.a`, the other a TAIL CALL to `b_ref`. A summary that
//       scans only statements sees one path, labels `pick` as returning `a`,
//       and so relabels lock `b` as lock `a` -- which severs the real b<c, c<b
//       ring. The single-path guard now covers terminators too.
use std::sync::Mutex;
pub struct W { pub a: Mutex<i32>, pub b: Mutex<i32>, pub c: Mutex<i32> }
impl W {
    fn b_ref(&self) -> &Mutex<i32> { &self.b }
    fn pick(&self, which: bool) -> &Mutex<i32> {
        if which { &self.a } else { self.b_ref() }
    }
}
pub fn t1(w: &W) {
    let g = w.pick(false).lock().unwrap();
    let h = w.c.lock().unwrap();
    let _ = (*g, *h);
}
pub fn t2(w: &W) {
    let h = w.c.lock().unwrap();
    let g = w.b.lock().unwrap();
    let _ = (*g, *h);
}
