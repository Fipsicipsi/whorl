use std::sync::{Mutex, RwLock};
struct Account { bal: Mutex<i64> }
struct Cfg { data: RwLock<i32> }

pub fn branchy(a: &Account, b: &Account, c: bool) {
    let g = a.bal.lock().unwrap();
    if c {
        let h = b.bal.lock().unwrap();
        let _ = *h;
    }
    let _ = *g;
}

pub fn rw(a: &Account, cfg: &Cfg) {
    let g = a.bal.lock().unwrap();
    let r = cfg.data.read().unwrap();
    let _ = (*g, *r);
}
