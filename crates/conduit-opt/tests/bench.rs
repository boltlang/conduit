//! For each program with a `bench` check, runs its `run` checks in the
//! evaluator as a backend would get it, and compares what each run did
//! with `<name>.bench`: steps, allocations, frees, the most objects live at
//! once, count ops, reuse, and cycle collections. These are the same every
//! time, unlike times, so a change shows up as a diff, like a snapshot.
//!
//! Each run has a line for each way of compiling it, each adding one thing
//! to the one before, after the optimizer:
//!
//! - `optimized`: no counts, so nothing is freed.
//! - `counts`: counts, every parameter owned.
//! - `+borrows`: borrowed parameters inferred.
//! - `+reuse`: reuse, before the counts.
//! - `+specialized`: drops specialized.
//! - `+cycles`: cycles collected, and once more at the end.
//!
//! So the table shows what each one is worth. A program whose handlers
//! need evidence passing has its effects lowered first.

use conduit_eval::{Machine, Stats, Value};
use conduit_ir::verify::verify_module;
use conduit_ir::{Atom, FuncId, Lit, Module, text};
use conduit_opt::ownership::{
    Borrows, insert_counts, insert_counts_with, insert_reuse, specialize_drops,
};
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
    let mut counts = optimized.clone();
    insert_counts_with(&mut counts, &Borrows::default())?;
    let mut borrows = optimized.clone();
    insert_counts(&mut borrows)?;
    let mut reuse = optimized.clone();
    insert_reuse(&mut reuse);
    insert_counts(&mut reuse)?;
    let mut specialized = reuse.clone();
    specialize_drops(&mut specialized);
    // How each is compiled, and whether it's run counting and collecting
    // cycles.
    let configs = [
        ("optimized", &optimized, false, false),
        ("counts", &counts, true, false),
        ("+borrows", &borrows, true, false),
        ("+reuse", &reuse, true, false),
        ("+specialized", &specialized, true, false),
        ("+cycles", &specialized, true, true),
    ];
    for (when, m, ..) in &configs {
        verify_module(m).map_err(|e| format!("{when}: {e:?}\n\n{}", text::print(m)))?;
    }
    let row = |name: &str, cells: [String; 8]| {
        format!(
            "  {name:<14}{:>10}{:>9}{:>9}{:>7}{:>11}{:>8}{:>7}{:>7}\n",
            cells[0], cells[1], cells[2], cells[3], cells[4], cells[5], cells[6], cells[7]
        )
    };
    let mut out = String::new();
    for run in p.runs(&optimized)? {
        let args: Vec<String> = run.args.iter().map(|&l| Atom::Lit(l).to_string()).collect();
        out.push_str(&format!("{}({})\n", run.name, args.join(", ")));
        let heads = [
            "steps",
            "allocs",
            "frees",
            "peak",
            "count ops",
            "reused",
            "fresh",
            "cycles",
        ];
        out.push_str(&row("", heads.map(String::from)));
        for &(when, m, counting, cycles) in &configs {
            let s = stats(m, run.func, run.args, counting, cycles);
            let cells = [
                s.steps,
                s.allocs,
                s.frees,
                s.peak_live,
                s.count_ops,
                s.reused,
                s.fresh,
                s.collections,
            ];
            out.push_str(&row(when, cells.map(|n| n.to_string())));
        }
    }
    p.snapshot("bench", &out)
}

/// What calling `func` with `args` does in `m`.
fn stats(m: &Module, func: FuncId, args: &[Lit], counting: bool, cycles: bool) -> Stats {
    let mut machine = Machine::new(m, conduit_test::eval::TestHost);
    machine.fuel = Some(100_000_000);
    machine.counting = counting;
    machine.cycles = cycles;
    let args = args.iter().map(|&l| Value::from(l)).collect();
    // A run that traps did what it did up to the trap.
    let _ = machine.call(func, args);
    if cycles {
        machine.collect_cycles();
    }
    machine.stats
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
