//! Compiles each test program that only uses what conduit-llvm handles,
//! with `clang`, and runs its `run` checks as a native program: as written
//! and after the optimizer, or, for a program with handlers, after lowering
//! its effects, and then after the optimizer. A `main` of the test's own
//! picks the check by its number, calls the function, and prints the bits
//! of each result. A trap is a signal. Skipped, with a note, when `clang` isn't installed.
//!
//! Each is also run with counts, and then with cycles collected too: each
//! run must then leave as many objects live as the evaluator's run does,
//! counting the same way, with cycles collected at the end when they are.

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;

use conduit_ir::{Lit, Module, Ty};
use conduit_llvm::Options;
use conduit_llvm::ir::{
    Call, CallConv, CastOp, Declare, FnBuilder, Global, Item, Label, Type, Value,
};
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

/// The module, with its effects lowered if it has handlers, which this
/// backend leaves to evidence passing.
fn without_handlers(mut m: Module) -> Module {
    if !m.handlers.is_empty() || !m.effects.is_empty() {
        // A module it can't lower is reported by the effects' own tests.
        let _ = conduit_effects::lower_effects(&mut m);
    }
    m
}

fn clang() -> bool {
    Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn test(p: &Program) -> Result<(), String> {
    if !clang() {
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
    let mut module = conduit_llvm::lower_module_with(m, &options).map_err(|e| e.to_string())?;
    module.items.extend(driver(m, &runs, counts)?);
    let text = module.to_string();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("llvm");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = format!(
        "{}_{}",
        p.name.replace('/', "_"),
        when.replace([' ', ','], "_")
    );
    let ll = dir.join(format!("{stem}.ll"));
    let exe = dir.join(stem);
    std::fs::write(&ll, &text).map_err(|e| e.to_string())?;
    let out = Command::new("clang")
        .args(["-O2", "-Wno-override-module", "-o"])
        .arg(&exe)
        .arg(&ll)
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/host.c"))
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "clang failed on {}:\n{}",
            ll.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    for (i, run) in runs.iter().enumerate() {
        let results = &m.funcs[&run.func].results;
        let (mut live, mut hosts) = (None, None);
        let got = Command::new(&exe)
            .arg(i.to_string())
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
                    parse(&words, results).map(Outcome::Values)
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
        .filter(|(id, _)| matches!(machine.count(*id), Some(conduit_eval::Count::Live(_))))
        .collect();
    let hosts = (live.iter())
        .filter(|(_, o)| matches!(o, conduit_eval::Object::Host(_)))
        .count();
    Ok((live.len() as i64, hosts as i64))
}

/// A C `main` that calls the function of run check `argv[1]` and prints
/// the bits of what it gives, in hex.
fn driver(m: &Module, runs: &[Run<'_>], counts: bool) -> Result<Vec<Item>, String> {
    let fmt = Value::global(".fmt");
    let printf = |b: &mut FnBuilder, bits: Value| {
        let call = Call {
            variadic: Some(vec![Type::Ptr]),
            ..Call::c(
                Type::I32,
                Value::global("printf"),
                vec![Type::Ptr.val(&fmt), Type::I64.val(bits)],
            )
        };
        b.call(call);
    };
    let params = vec![(Type::I32, "argc".into()), (Type::Ptr, "argv".into())];
    let mut b = FnBuilder::new(CallConv::C, Type::I32, "main", params);
    let argv = b.param(1);
    let p = b.gep(Type::Ptr, argv, vec![Type::I64.val(1)]);
    let a = b.load(Type::Ptr, p);
    let k = b.call_value(Call::c(
        Type::I32,
        Value::global("atoi"),
        vec![Type::Ptr.val(a)],
    ));
    let labels: Vec<Label> = runs.iter().map(|_| b.new_block()).collect();
    let done = b.new_block();
    let cases = labels.iter().cloned().enumerate();
    b.switch(
        Type::I32.val(k),
        &done,
        cases.map(|(i, l)| (i as i64, l)).collect(),
    );
    for (run, label) in runs.iter().zip(&labels) {
        b.start(label);
        let f = &m.funcs[&run.func];
        let ret = conduit_llvm::ret_ty(&f.results)?;
        let args = (f.params.iter().zip(run.args))
            .map(|(v, a)| Ok(conduit_llvm::ty(f.var_ty(*v))?.val(conduit_llvm::lit(a))))
            .collect::<Result<_, String>>()?;
        let r = b.call(Call::tailcc(
            ret.clone(),
            conduit_llvm::symbol(run.func),
            args,
        ));
        for (j, t) in f.results.iter().enumerate() {
            let r = r.clone().expect("a call with results");
            let v = if f.results.len() == 1 {
                r
            } else {
                b.extract_value(ret.clone(), r, j as u32)
            };
            let bits = match t {
                Ty::I64 => v,
                Ty::F64 => b.cast(CastOp::Bitcast, Type::Double, v, Type::I64),
                Ty::F32 => {
                    let c = b.cast(CastOp::Bitcast, Type::Float, v, Type::I32);
                    b.cast(CastOp::ZExt, Type::I32, c, Type::I64)
                }
                _ => b.cast(CastOp::ZExt, conduit_llvm::ty(t)?, v, Type::I64),
            };
            printf(&mut b, bits);
        }
        // Then how many objects are left live, once cycles are collected,
        // if they are.
        if counts {
            b.call(Call::c(
                Type::Void,
                Value::global("conduit_collect"),
                vec![],
            ));
            let live = b.load(Type::I64, Value::global("conduit_live"));
            printf(&mut b, live);
            let hosts = Call::c(Type::I64, Value::global("conduit_test_hosts_live"), vec![]);
            let hosts = b.call_value(hosts);
            printf(&mut b, hosts);
        }
        b.br(&done);
    }
    b.start(&done);
    b.ret(Some(Type::I32.val(0)));
    Ok(vec![
        Global::constant(
            ".fmt",
            Type::Array(6, Box::new(Type::I8)),
            Value::Bytes(b"%llx \0".to_vec()),
        )
        .into(),
        Declare {
            variadic: true,
            ..Declare::new(Type::I32, "printf", vec![Type::Ptr])
        }
        .into(),
        Declare::new(Type::I32, "atoi", vec![Type::Ptr]).into(),
        Declare::new(Type::I64, "conduit_test_hosts_live", vec![]).into(),
        b.finish().into(),
    ])
}

/// The results the driver printed, as literals of their types.
fn parse(words: &[&str], results: &[Ty]) -> Result<Vec<Lit>, String> {
    if words.len() != results.len() {
        return Err(format!("printed {words:?} for {} results", results.len()));
    }
    words
        .iter()
        .zip(results)
        .map(|(w, t)| {
            let b = u64::from_str_radix(w, 16).map_err(|e| e.to_string())?;
            Ok(match t {
                Ty::I8 => Lit::I8(b as u8 as i8),
                Ty::I16 => Lit::I16(b as u16 as i16),
                Ty::I32 => Lit::I32(b as u32 as i32),
                Ty::I64 => Lit::I64(b as i64),
                Ty::F32 => Lit::F32(b as u32),
                Ty::F64 => Lit::F64(b),
                other => return Err(format!("a result of type {other}")),
            })
        })
        .collect()
}
