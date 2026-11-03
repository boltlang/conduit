//! Optimizes every test program, then checks that it still verifies, that
//! its `run` checks still pass in the evaluator, and that it matches its
//! `<name>.opt.conduit` snapshot if it has an `opt` check.

use conduit_ir::text;
use conduit_ir::verify::verify_module;
use conduit_test::{CheckKind, Program};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_)))
        && (p.runs().next().is_some() || p.has(|k| *k == CheckKind::Opt))
}

fn test(p: &Program) -> Result<(), String> {
    let mut m = p.module()?;
    conduit_opt::optimize_module(&mut m);
    let printed = text::print(&m);
    if let Err(errors) = verify_module(&m) {
        let errors: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
        return Err(format!(
            "the optimized program doesn't verify:\n{}\n\n{printed}",
            errors.join("\n")
        ));
    }
    if p.has(|k| *k == CheckKind::Opt) {
        p.snapshot("opt.conduit", &printed)?;
    }
    conduit_test::eval::check_runs(p, &m)
}
