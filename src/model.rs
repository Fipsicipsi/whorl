//! The intermediate representation the solver consumes.
//!
//! The P1 frontend ([`crate::frontend`]) lowers source into a flat list of
//! lock-acquisition events; the solver ([`crate::solver`]) turns those into the
//! class-ordering graph. Keeping the IR separate from the surface language lets
//! the solver and its tests construct events directly.

use std::collections::HashMap;

/// A program reduced to the only thing the deadlock analysis cares about.
pub struct Program {
    pub name: String,
    pub events: Vec<Event>,
    /// How many distinct lock *instances* are declared for each class. Used to
    /// tell a genuine cross-instance ordering inversion (>= 2 instances) apart
    /// from single-instance reentrant re-acquisition when explaining a verdict.
    pub class_instances: HashMap<String, usize>,
    /// `Some(reason)` if the frontend stopped early (hit a scalability budget or
    /// depth limit) and the event list is therefore *partial*. The emitted
    /// events are still sound (every edge is real), so a deadlock found in them
    /// is real - but the *absence* of a deadlock is not conclusive.
    pub incomplete: Option<String>,
}

/// A single lock acquisition at one program point.
pub struct Event {
    pub function: String,
    pub site: String,
    /// Lock classes provably held when this acquisition happens.
    pub held: Vec<String>,
    /// The lock class being acquired here.
    pub acquires: String,
}
