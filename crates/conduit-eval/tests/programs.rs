//! Runs the `run` checks of every test program with the evaluator.

use conduit_test::{CheckKind, Program};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    p.runs().next().is_some() && !p.has(|k| matches!(k, CheckKind::VerifyError(_)))
}

fn test(p: &Program) -> Result<(), String> {
    conduit_test::eval::check_runs(p, &p.module()?)
}
