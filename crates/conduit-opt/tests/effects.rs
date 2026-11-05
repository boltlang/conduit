//! Lowers the effects of every test program with `run` checks by evidence
//! passing, then checks that the result verifies, has no effects left, and
//! still passes the `run` checks in the evaluator, and again after the
//! optimizer.

use conduit_ir::text;
use conduit_ir::verify::verify_module;
use conduit_ir::{Module, Op, Tail};
use conduit_test::{CheckKind, Program};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_))) && p.has_runs()
}

fn test(p: &Program) -> Result<(), String> {
    let mut m = p.module()?;
    conduit_opt::effects::lower_effects(&mut m)?;
    check(p, &m, "after lowering effects")?;
    conduit_opt::optimize_module(&mut m);
    check(p, &m, "after lowering effects and optimizing")
}

fn check(p: &Program, m: &Module, after: &str) -> Result<(), String> {
    if let Err(errors) = verify_module(m) {
        let errors: Vec<String> = errors.iter().map(|e| e.named(m)).collect();
        return Err(format!(
            "the program {after} doesn't verify:\n{}\n\n{}",
            errors.join("\n"),
            text::print(m)
        ));
    }
    if let Some(left) = effects_left(m) {
        return Err(format!("the program {after} still has {left}"));
    }
    conduit_test::eval::check_runs(p, m).map_err(|e| format!("{after}: {e}"))
}

/// What's left of effects in `m`, if anything.
fn effects_left(m: &Module) -> Option<String> {
    if !m.handlers.is_empty() {
        return Some("handlers".into());
    }
    for (id, f) in &m.funcs {
        if !f.row.is_nothing() {
            return Some(format!("a row on {id}"));
        }
        let mut todo = vec![&f.body];
        while let Some(b) = todo.pop() {
            for l in &b.lets {
                if matches!(
                    l.op,
                    Op::Handle(..) | Op::Perform(..) | Op::Resume | Op::Discontinue(..)
                ) {
                    return Some(format!("an effect op in {id}"));
                }
            }
            match &b.tail {
                Tail::Join { defs, body, .. } => {
                    todo.extend(defs.iter().map(|d| &d.body));
                    todo.push(body);
                }
                Tail::Case { arms, default, .. } => {
                    todo.extend(arms.iter().map(|a| &a.body));
                    todo.extend(default.as_deref());
                }
                Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
            }
        }
    }
    None
}
