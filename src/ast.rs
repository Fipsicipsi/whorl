//! Abstract syntax for the Whorl P2 surface language.
//!
//! P2 adds function-valued parameters (callbacks) so held-sets can propagate
//! through abstraction, not just direct calls:
//!
//! ```text
//! lock table : Table
//! lock row   : Row
//!
//! fn with_table_locked(body) {   // `body` is a function-valued parameter
//!     with table {
//!         body()                 // calling the callback while holding Table
//!     }
//! }
//!
//! fn lock_a_row() { with row { } }
//!
//! fn import() { with_table_locked(lock_a_row) }   // pass a function as the callback
//! ```

#[derive(Clone, Debug)]
pub enum Item {
    /// `lock <name> : <class>`
    Lock { name: String, class: String },
    /// `fn <name>(<params>) { .. }`
    Func(Func),
    /// `extern fn <name> [acquires <Class>, ..]` - a foreign function whose body
    /// Whorl cannot see, but whose lock-acquisition contract is declared: it may
    /// acquire the listed classes, nested in that order.
    Extern { name: String, acquires: Vec<String> },
}

#[derive(Clone, Debug)]
pub struct Func {
    pub name: String,
    /// Function-valued parameter names (callbacks).
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    /// True for `isr fn` - an interrupt handler. Its body runs holding the CPU
    /// (it can preempt tasks) with interrupts masked (run-to-completion).
    pub is_isr: bool,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    /// `with <lock> { .. }` - hold one lock for the block.
    With {
        lock: String,
        line: usize,
        body: Vec<Stmt>,
    },
    /// `with ordered(<lock>, ..) { .. }` - acquire same-class locks in a
    /// defined total order, as one combined acquisition.
    Ordered {
        locks: Vec<String>,
        line: usize,
        body: Vec<Stmt>,
    },
    /// `<callee>(<args>)` - a call. `callee` may be a top-level function or a
    /// function-valued parameter in scope; `args` are function/parameter names
    /// bound positionally to the callee's parameters.
    Call {
        callee: String,
        args: Vec<String>,
        line: usize,
    },
    /// `couple <Class> { .. }` - assert that, within the block, acquisitions of
    /// `<Class>` form a monotonic hand-over-hand chain (lock coupling), so
    /// same-class acquisitions of that class are safe and do not self-edge.
    /// Cross-class edges, and edges against a same-class lock already held
    /// *outside* the couple, are still enforced.
    Couple {
        class: String,
        line: usize,
        body: Vec<Stmt>,
    },
    /// `mask { .. }` - interrupts are disabled within the block, so a lock held
    /// here cannot be preempted by an interrupt handler.
    Mask { body: Vec<Stmt> },
}

#[derive(Clone, Debug)]
pub struct Ast {
    pub items: Vec<Item>,
}
