//! For each program with a `bench` check, runs its `run` checks in the
//! evaluator as a backend would get it, and compares what each run did
//! with `<name>.bench`: steps, allocations, frees, the most objects live at
//! once, count ops, reuse, and cycle collections. These are the same every
//! time, unlike times, so a change shows up as a diff, like a snapshot.
//!
//! Each run has two lines: `optimized`, after the optimizer, without
//! counts, so nothing is freed; and `counted`, after the optimizer, reuse,
//! counts and specialized drops, with cycles collected, and once more at
//! the end. A program whose handlers need evidence passing has its
//! effects lowered first.

use conduit_eval::{Machine, Stats, Value};
use conduit_ir::verify::verify_module;
use conduit_ir::{Atom, Module, text};
use conduit_test::{CheckKind, Program};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    p.has(|k| matches!(k, CheckKind::Bench)) && p.has_runs()
}

fn test(p: &Program) -> Result<(), String> {
    let mut optimized = p.module()?;
    if has_general_handler(&optimized) {
        conduit_opt::effects::lower_effects(&mut optimized)?;
    }
    conduit_opt::optimize_module(&mut optimized);
    let mut counted = optimized.clone();
    conduit_opt::ownership::insert_reuse(&mut counted);
    conduit_opt::ownership::insert_counts(&mut counted)?;
    conduit_opt::ownership::specialize_drops(&mut counted);
    for (m, when) in [(&optimized, "optimized"), (&counted, "counted")] {
        verify_module(m).map_err(|e| format!("{when}: {e:?}\n\n{}", text::print(m)))?;
    }
    let mut out = String::new();
    for run in p.runs(&optimized)? {
        let args: Vec<String> = run.args.iter().map(|&l| Atom::Lit(l).to_string()).collect();
        out.push_str(&format!("{}({})\n", run.name, args.join(", ")));
        for (m, when, counting) in [
            (&optimized, "optimized", false),
            (&counted, "counted", true),
        ] {
            let stats = stats(m, run.func, run.args, counting)
                .map_err(|e| format!("{}: {when}: {e}", run.name))?;
            out.push_str(&format!("  {when}: {stats}\n"));
        }
    }
    p.snapshot("bench", &out)
}

/// What calling `func` with `args` does in `m`.
fn stats(
    m: &Module,
    func: conduit_ir::FuncId,
    args: &[conduit_ir::Lit],
    counting: bool,
) -> Result<Stats, String> {
    let mut machine = Machine::new(m, conduit_test::eval::TestHost);
    machine.fuel = Some(100_000_000);
    machine.counting = counting;
    machine.cycles = counting;
    let args = args.iter().map(|&l| Value::from(l)).collect();
    // A run that traps did what it did up to the trap.
    let _ = machine.call(func, args);
    machine.collect_cycles();
    Ok(machine.stats)
}

fn has_general_handler(m: &Module) -> bool {
    m.handlers.values().any(|h| {
        h.ops.iter().any(|c| {
            m.funcs.get(c).is_some_and(|f| {
                conduit_ir::clause::plain_call(f).is_none()
                    && conduit_ir::clause::aborting(f).is_none()
            })
        })
    })
}
