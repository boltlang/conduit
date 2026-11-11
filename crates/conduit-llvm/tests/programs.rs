//! Compiles each test program that only uses what conduit-llvm handles,
//! with `clang`, and runs its `run` checks as a native program: as written
//! and after the optimizer, or, for a program with handlers that aren't all
//! tail-resumptive or aborting, after lowering its effects, and then after
//! the optimizer. `conduit_llvm::native`'s
//! `main` picks the check by its number, calls the function, and prints
//! the bits of each result. A trap is a signal. Skipped, with a note, when
//! `clang` isn't installed.
//!
//! Each is also run with counts, and then with cycles collected too: each
//! run must then leave as many objects live as the evaluator's run does,
//! counting the same way, with cycles collected at the end when they are,
//! not counting its continuations, which native code doesn't make for the
//! handlers it lowers itself.

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;

use conduit_ir::Module;
use conduit_llvm::Options;
use conduit_llvm::native;
use conduit_test::{CheckKind, Outcome, Program, Run, Target};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_)))
        && p.has_runs()
        && p.module_for(Target::C)
            .is_ok_and(|m| conduit_llvm::emit_module(&without_handlers(m)).is_ok())
}

/// The module, with its effects lowered if it has handlers that need
/// evidence passing, which this backend leaves to it.
fn without_handlers(mut m: Module) -> Module {
    if !conduit_ir::clause::direct_handlers(&m) {
        // A module it can't lower is reported by the effects' own tests.
        let _ = conduit_effects::lower_effects(&mut m);
    }
    m
}

fn test(p: &Program) -> Result<(), String> {
    if !native::clang() {
        eprintln!("{}: skipped, since clang isn't installed", p.name);
        return Ok(());
    }
    // Built for C, and for the evaluator, which the counted runs compare
    // with: the same passes make them the same but for their links.
    let m = without_handlers(p.module_for(Target::C)?);
    let emu = without_handlers(p.module()?);
    let optimized = |mut m: Module| {
        conduit_opt::optimize_module(&mut m);
        m
    };
    let versions = [
        ("as written", m.clone(), emu.clone()),
        ("after optimizing", optimized(m), optimized(emu)),
    ];
    for (when, m, emu) in versions {
        check(p, &m, &emu, when, Options::default()).map_err(|e| format!("{when}: {e}"))?;
        // With counts too, which must free all the evaluator does.
        let (c, emu) = (with_counts(p, m)?, with_counts(p, emu)?);
        for cycles in [false, true] {
            let counted = format!(
                "{when}, with counts{}",
                if cycles { " and cycles" } else { "" }
            );
            let options = Options {
                counts: true,
                cycles,
            };
            check(p, &c, &emu, &counted, options).map_err(|e| format!("{counted}: {e}"))?;
        }
    }
    Ok(())
}

/// `m` with reuse and counts, unless the program counts by hand.
fn with_counts(p: &Program, mut m: Module) -> Result<Module, String> {
    if !p.has(|k| matches!(k, CheckKind::Counted)) {
        conduit_opt::ownership::insert_reuse(&mut m);
        conduit_opt::ownership::insert_counts(&mut m)?;
        conduit_opt::ownership::specialize_drops(&mut m);
    }
    Ok(m)
}

/// Compiles `m` with the test host, and runs its `run` checks. With
/// counts, each must leave as many objects live as the evaluator does on
/// `emu`, the program built for it.
fn check(
    p: &Program,
    m: &Module,
    emu: &Module,
    when: &str,
    options: Options,
) -> Result<(), String> {
    let runs = p.runs(m)?;
    let counts = options.counts;
    let calls: Vec<native::Run<'_>> = (runs.iter())
        .map(|r| native::Run {
            func: r.func,
            args: r.args,
        })
        .collect();
    let mut module = conduit_llvm::lower_module_with(m, &options).map_err(|e| e.to_string())?;
    module.items.extend(native::print_runs(m, &calls, counts)?);
    module.items.extend(native::bench_runs(m, &calls)?);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("llvm");
    let stem = format!(
        "{}_{}",
        p.name.replace('/', "_"),
        when.replace([' ', ','], "_")
    );
    // One for each program, since the tests run at the same time.
    let main = dir.join(format!("{stem}_timer.c"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(&main, native::TIMER).map_err(|e| e.to_string())?;
    let host = conduit_test::programs_dir().join("host/c.c");
    let exe = native::build(&dir, &stem, &module.to_string(), &[&main, &host])?;
    for (i, run) in runs.iter().enumerate() {
        let results = &m.funcs[&run.func].results;
        let (mut live, mut hosts) = (None, None);
        let got = Command::new(&exe)
            .args(["check", &i.to_string()])
            .output()
            .map_err(|e| e.to_string())
            .and_then(|o| match o.status.signal() {
                Some(s) => Ok(Outcome::Trap(format!("signal {s}"))),
                None if o.status.success() => {
                    let out = String::from_utf8_lossy(&o.stdout);
                    let mut words: Vec<&str> = out.split_whitespace().collect();
                    if counts {
                        let mut last = || words.pop().and_then(|w| i64::from_str_radix(w, 16).ok());
                        hosts = last();
                        live = last();
                    }
                    native::parse_results(&words, results).map(Outcome::Values)
                }
                None => Err(format!("exited with {}", o.status)),
            });
        let gave = matches!(got, Ok(Outcome::Values(_)));
        run.check(p, got)?;
        if counts && gave {
            let (expected, expected_hosts) = evaluator_live(emu, run, options.cycles)?;
            let shown = |n: Option<i64>| n.map_or("an unknown number of".into(), |n| n.to_string());
            if live != Some(expected) {
                return Err(format!(
                    "{}: {}: native code leaves {} objects live, but the evaluator {expected}",
                    p.at(run.line),
                    run.name,
                    shown(live)
                ));
            }
            // Each box holds one reference to a value of its own, which the
            // host lets go of when the box releases it.
            if hosts != Some(expected_hosts) {
                return Err(format!(
                    "{}: {}: the C host holds {} host values, but the evaluator {expected_hosts} boxes of them",
                    p.at(run.line),
                    run.name,
                    shown(hosts)
                ));
            }
        }
    }
    Ok(())
}

/// How many objects the evaluator leaves live after `run`, counting, with
/// cycles collected at the end if `cycles`, as the native code does, and
/// how many of them are host values.
fn evaluator_live(m: &Module, run: &Run<'_>, cycles: bool) -> Result<(i64, i64), String> {
    let mut machine = conduit_eval::Machine::new(m, conduit_test::eval::TestHost);
    machine.counting = true;
    machine.cycles = cycles;
    let args = run
        .args
        .iter()
        .map(|&l| conduit_eval::Value::from(l))
        .collect();
    machine.call(run.func, args).map_err(|e| e.to_string())?;
    if cycles {
        machine.collect_cycles();
    }
    let live: Vec<_> = (machine.objects())
        .filter(|(id, o)| {
            matches!(machine.count(*id), Some(conduit_eval::Count::Live(_)))
                && !matches!(o, conduit_eval::Object::Cont(_))
        })
        .collect();
    let hosts = (live.iter())
        .filter(|(_, o)| matches!(o, conduit_eval::Object::Host(_)))
        .count();
    Ok((live.len() as i64, hosts as i64))
}
