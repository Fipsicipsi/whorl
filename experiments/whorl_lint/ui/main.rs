// ui_test fixture for whorl_lint. `cargo test` compiles this under the lint and
// compares emitted diagnostics against main.stderr (generate it the first time
// with: env BLESS=1 cargo test). For now the lint primarily WRITES whorl-events.json;
// once you also emit span_lint diagnostics on detected cycles, capture them here.
use std::sync::Mutex;

struct Account {
    balance: Mutex<i64>,
}

// Two-account transfer: locks `from` then `to`. Called with (a,b) on one thread
// and (b,a) on another, this is the textbook ABBA lock-ordering deadlock. The
// held-set at the second `.lock()` contains the first guard; both acquire class
// `Mutex<i64>`, so the solver sees a self-edge on a >=2-instance class.
fn transfer(from: &Account, to: &Account, amount: i64) {
    let mut from_g = from.balance.lock().unwrap();
    let mut to_g = to.balance.lock().unwrap(); // held = {from_g}, acquires Mutex<i64>
    *from_g -= amount;
    *to_g += amount;
}

fn main() {
    let a = Account { balance: Mutex::new(100) };
    let b = Account { balance: Mutex::new(0) };
    transfer(&a, &b, 10);
}
