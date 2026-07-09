use std::sync::Mutex;
struct Sys { a: Mutex<i32>, b: Mutex<i32> }
pub fn p(s: &Sys) { let x = s.a.lock().unwrap(); let y = s.b.lock().unwrap(); let _=(*x,*y); }
pub fn r(s: &Sys) { let y = s.b.lock().unwrap(); let x = s.a.lock().unwrap(); let _=(*x,*y); }
