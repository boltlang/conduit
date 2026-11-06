//! Inserts reference counts into every test program with `run` checks,
//! then checks that the result verifies and still passes the `run` checks
//! in the evaluator with counting on, with every count right: nothing
//! undropped, dropped too often or used after it's freed. Cycles, and what
//! a continuation that's never resumed holds, may still be left.
//!
//! It does so for the program as written, after the optimizer, and after
//! lowering effects by evidence passing and then optimizing, each time with
//! the counts as inserted and again with their `drop`s specialized.

use conduit_eval::Leaks;
use conduit_ir::Module;
use conduit_ir::text;
use conduit_ir::verify::verify_module;
use conduit_test::{CheckKind, Program};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_) | CheckKind::Counted)) && p.has_runs()
}

fn test(p: &Program) -> Result<(), String> {
    let m = p.module()?;
    check(p, m.clone(), "as written")?;
    let mut opt = m.clone();
    conduit_opt::optimize_module(&mut opt);
    check(p, opt, "after optimizing")?;
    let mut lowered = m;
    conduit_opt::effects::lower_effects(&mut lowered)?;
    conduit_opt::optimize_module(&mut lowered);
    check(p, lowered, "after lowering effects and optimizing")
}

fn check(p: &Program, mut m: Module, when: &str) -> Result<(), String> {
    conduit_opt::ownership::insert_counts(&mut m).map_err(|e| format!("{when}: {e}"))?;
    run(p, &m, when)?;
    conduit_opt::ownership::specialize_drops(&mut m);
    run(p, &m, &format!("{when}, with drops specialized"))
}

fn run(p: &Program, m: &Module, when: &str) -> Result<(), String> {
    let m = m.clone();
    if let Err(errors) = verify_module(&m) {
        let errors: Vec<String> = errors.iter().map(|e| e.named(&m)).collect();
        return Err(format!(
            "{when}, with counts, the program doesn't verify:\n{}\n\n{}",
            errors.join("\n"),
            text::print(&m)
        ));
    }
    conduit_test::eval::check_runs_with_counts(p, &m, counts_ok)
        .map_err(|e| format!("{when}, with counts: {e}\n\n{}", text::print(&m)))
}

fn counts_ok(leaks: &Leaks) -> Result<(), String> {
    match leaks.counts_ok() {
        true => Ok(()),
        false => Err(leaks.to_string()),
    }
}
