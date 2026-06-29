//! Whorl P1 - command-line driver.
//!
//! Reads `.whorl` source files, parses them, lowers to lock-acquisition events
//! by propagating held-sets through the call graph, runs the lock-ordering
//! analysis, and prints a verdict pointing at real source lines. Exit code is
//! non-zero if any program contains a potential deadlock, so it is usable as a
//! CI gate.

mod ast;
mod frontend;
mod model;
mod parser;
mod solver;

use std::process::ExitCode;

use solver::{analyze, Outcome, Report};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let dot_mode = matches!(args.first().map(String::as_str), Some("--dot"));
    if dot_mode {
        args.remove(0);
    }

    if args.is_empty() {
        eprintln!("whorl - compile-time freedom from lock-ordering deadlocks\n");
        eprintln!("usage: whorl [--dot] <program.whorl> [more.whorl ...]");
        eprintln!("  --dot   emit the lock-order graph as Graphviz DOT (cycle edges in red)");
        eprintln!("\nsee the examples/ directory for the language.");
        return ExitCode::from(2);
    }

    if dot_mode {
        for path in &args {
            match dot_one(path) {
                Ok(s) => print!("{s}"),
                Err(e) => {
                    eprintln!("error: {path}:\n  {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        return ExitCode::SUCCESS;
    }

    let mut any_deadlock = false;
    for path in &args {
        match run_one(path) {
            Ok(true) => any_deadlock = true,
            Ok(false) => {}
            Err(e) => {
                eprintln!("error: {path}:\n  {e}");
                any_deadlock = true;
            }
        }
        println!();
    }

    if any_deadlock {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn dot_one(path: &str) -> Result<String, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file = basename(path);
    let ast = parser::parse(&src)?;
    let prog = frontend::lower(&ast, &file).map_err(|errs| errs.join("\n  "))?;
    Ok(solver::to_dot(&prog))
}

fn run_one(path: &str) -> Result<bool, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file = basename(path);
    let ast = parser::parse(&src)?;
    let prog = frontend::lower(&ast, &file).map_err(|errs| errs.join("\n  "))?;
    let report = analyze(&prog);
    // A deadlock found in a partial analysis is still real and sound. Only the
    // *absence* of a deadlock is inconclusive when the analysis was truncated.
    let deadlock = matches!(report.outcome, Outcome::Deadlock { .. });
    let bad = deadlock || prog.incomplete.is_some();
    print_report(&report, prog.incomplete.as_deref());
    Ok(bad)
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Pretty-print a class name, rendering the synthetic preemption resource
/// (`<cpu>`) readably in witnesses and orders.
fn disp(class: &str) -> &str {
    if class == "<cpu>" {
        "CPU(preemption)"
    } else {
        class
    }
}

fn print_report(r: &Report, incomplete: Option<&str>) {
    println!("== whorl :: lock-ordering analysis :: {} ==", r.program);
    let ev = if r.event_count == 1 {
        "event"
    } else {
        "events"
    };
    let cl = if r.class_count == 1 {
        "lock class"
    } else {
        "lock classes"
    };
    let co = if r.constraint_count == 1 {
        "ordering constraint"
    } else {
        "ordering constraints"
    };
    println!(
        "   {} {}, {} {}, {} {}",
        r.event_count, ev, r.class_count, cl, r.constraint_count, co
    );
    println!();

    match &r.outcome {
        Outcome::DeadlockFree { order } => {
            if let Some(reason) = incomplete {
                println!(
                    "   [INCOMPLETE]  analysis stopped early -- the verdict is NOT conclusive"
                );
                println!();
                println!("   {reason}");
                println!("   no deadlock was found in the partial analysis, but undetected ones may remain.");
                return;
            }
            println!(
                "   [SAFE]  the lock graph is acyclic -- no lock-ordering deadlock is possible"
            );
            println!();
            println!("   a valid global lock order:");
            for (i, c) in order.iter().enumerate() {
                println!("     {}. {}", i + 1, disp(c));
            }
            println!("   any acquisition consistent with this order cannot form a circular wait. (Havender)");
        }
        Outcome::Deadlock { cycle } => {
            println!("   [DEADLOCK]  a lock-ordering cycle exists -- a circular wait is possible");
            println!();
            for c in cycle {
                println!(
                    "     {}  <  {}   via {} @ {}",
                    disp(&c.before),
                    disp(&c.after),
                    c.function,
                    c.site
                );
            }
            println!();
            if cycle.len() == 1 && cycle[0].before == cycle[0].after {
                let cls = &cycle[0].before;
                if r.self_loop_single_instance {
                    println!(
                        "   the single lock of class '{cls}' is re-acquired while already held."
                    );
                    println!(
                        "   if that lock is not reentrant, a thread deadlocks against itself here."
                    );
                    println!("   fix: avoid re-acquiring the lock, or make it reentrant.");
                } else {
                    println!("   two locks of class '{cls}' are acquired without a defined order.");
                    println!("   one thread may hold x and wait for y while another holds y and waits for x.");
                    println!("   fix: acquire same-class locks together with `with ordered(a, b) {{ .. }}`.");
                }
            } else {
                println!(
                    "   these constraints form a cycle: no consistent global lock order exists,"
                );
                println!("   so threads acquiring these locks can wait on each other in a ring.");
            }
        }
    }
}
