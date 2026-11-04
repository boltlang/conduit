//! Optimizes every test program, then checks that it still verifies and
//! that its `run` checks still pass in the evaluator.
//!
//! A program with an `opt` check must also match its `<name>.opt.conduit`
//! snapshot. `//# opt` snapshots the program after all passes, and
//! `//# opt: dce, copies` after only those passes, which are verified and
//! run on their own too. With several `opt` checks, the snapshot has a
//! section for each, in order, headed `// after <passes>`.

use conduit_ir::Module;
use conduit_ir::text;
use conduit_ir::verify::verify_module;
use conduit_opt::Pass;
use conduit_test::{CheckKind, Program};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_)))
        && (p.runs().next().is_some() || p.has(|k| matches!(k, CheckKind::Opt(_))))
}

fn test(p: &Program) -> Result<(), String> {
    let opts: Vec<(u32, &Option<Vec<String>>)> = p
        .checks
        .iter()
        .filter_map(|c| match &c.kind {
            CheckKind::Opt(passes) => Some((c.line, passes)),
            _ => None,
        })
        .collect();
    for (i, (line, passes)) in opts.iter().enumerate() {
        if let Some((first, _)) = opts[..i].iter().find(|(_, other)| other == passes) {
            return Err(format!(
                "line {line}: the same `opt` check as on line {first}"
            ));
        }
    }

    let all = optimized(p, conduit_opt::PASSES)?;
    let mut sections = Vec::new();
    for (_, passes) in &opts {
        let (header, module) = match passes {
            None => ("all passes".to_string(), text::print(&all)),
            Some(names) => {
                let passes = names
                    .iter()
                    .map(|n| conduit_opt::pass(n).ok_or_else(|| unknown_pass(n)))
                    .collect::<Result<Vec<_>, _>>()?;
                (names.join(", "), text::print(&optimized(p, &passes)?))
            }
        };
        sections.push((header, module));
    }
    match sections.as_slice() {
        [] => {}
        // A single check needs no header.
        [(_, module)] => p.snapshot("opt.conduit", module)?,
        _ => {
            let text: Vec<String> = sections
                .iter()
                .map(|(header, module)| format!("// after {header}\n\n{module}"))
                .collect();
            p.snapshot("opt.conduit", &text.join("\n"))?;
        }
    }
    Ok(())
}

/// The program after `passes`, once it's checked that it verifies and
/// passes the `run` checks.
fn optimized(p: &Program, passes: &[&dyn Pass]) -> Result<Module, String> {
    let mut m = p.module()?;
    conduit_opt::run_passes_on_module(passes, &mut m);
    let names: Vec<&str> = passes.iter().map(|p| p.name()).collect();
    let after = format!("after {}", names.join(", "));
    if let Err(errors) = verify_module(&m) {
        let errors: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
        return Err(format!(
            "the program {after} doesn't verify:\n{}\n\n{}",
            errors.join("\n"),
            text::print(&m)
        ));
    }
    conduit_test::eval::check_runs(p, &m).map_err(|e| format!("{after}: {e}"))?;
    Ok(m)
}

fn unknown_pass(name: &str) -> String {
    let known: Vec<&str> = conduit_opt::PASSES.iter().map(|p| p.name()).collect();
    format!(
        "`opt` names an unknown pass `{name}`: the passes are {}",
        known.join(", ")
    )
}
