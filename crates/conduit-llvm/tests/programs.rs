//! Compiles each test program that only uses what conduit-llvm handles,
//! with `clang`, and runs its `run` checks as a native program: as written
//! and after the optimizer, or, for a program with handlers, after lowering
//! its effects, and then after the optimizer. A `main` of the test's own
//! picks the check by its number, calls the function, and prints the bits
//! of each result. A trap is a signal. Skipped, with a note, when `clang` isn't installed.

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;

use conduit_ir::{Lit, Module, Ty};
use conduit_test::{CheckKind, Outcome, Program, Run};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_)))
        && p.has_runs()
        && p.module()
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
    let m = without_handlers(p.module()?);
    let mut optimized = m.clone();
    conduit_opt::optimize_module(&mut optimized);
    for (when, m) in [("as written", m), ("after optimizing", optimized)] {
        check(p, &m, when).map_err(|e| format!("{when}: {e}"))?;
    }
    Ok(())
}

fn check(p: &Program, m: &Module, when: &str) -> Result<(), String> {
    let runs = p.runs(m)?;
    let mut text = conduit_llvm::emit_module(m).map_err(|e| e.to_string())?;
    text.push_str(&driver(m, &runs)?);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("llvm");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = format!("{}_{}", p.name.replace('/', "_"), when.replace(' ', "_"));
    let ll = dir.join(format!("{stem}.ll"));
    let exe = dir.join(stem);
    std::fs::write(&ll, &text).map_err(|e| e.to_string())?;
    let out = Command::new("clang")
        .args(["-O2", "-Wno-override-module", "-o"])
        .arg(&exe)
        .arg(&ll)
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
        let got = Command::new(&exe)
            .arg(i.to_string())
            .output()
            .map_err(|e| e.to_string())
            .and_then(|o| match o.status.signal() {
                Some(s) => Ok(Outcome::Trap(format!("signal {s}"))),
                None if o.status.success() => {
                    parse(&String::from_utf8_lossy(&o.stdout), results).map(Outcome::Values)
                }
                None => Err(format!("exited with {}", o.status)),
            });
        run.check(p, got)?;
    }
    Ok(())
}

/// A C `main` that calls the function of run check `argv[1]` and prints
/// the bits of what it gives, in hex.
fn driver(m: &Module, runs: &[Run<'_>]) -> Result<String, String> {
    let mut s = String::from(
        "\n@.fmt = private unnamed_addr constant [6 x i8] c\"%llx \\00\"\n\
         declare i32 @printf(ptr, ...)\n\
         declare i32 @atoi(ptr)\n\n\
         define i32 @main(i32 %argc, ptr %argv) {\n\
         entry:\n  \
         %p = getelementptr ptr, ptr %argv, i64 1\n  \
         %a = load ptr, ptr %p\n  \
         %k = call i32 @atoi(ptr %a)\n",
    );
    let cases: Vec<String> = (0..runs.len())
        .map(|i| format!("i32 {i}, label %c{i}"))
        .collect();
    s.push_str(&format!(
        "  switch i32 %k, label %done [ {} ]\n",
        cases.join(" ")
    ));
    for (i, run) in runs.iter().enumerate() {
        let f = &m.funcs[&run.func];
        let ret = conduit_llvm::ret_ty(&f.results)?;
        let args: Vec<String> = (f.params.iter().zip(run.args))
            .map(|(v, a)| {
                Ok(format!(
                    "{} {}",
                    conduit_llvm::ty(f.var_ty(*v))?,
                    conduit_llvm::lit(a)
                ))
            })
            .collect::<Result<_, String>>()?;
        s.push_str(&format!("c{i}:\n"));
        let call = format!(
            "call tailcc {ret} {}({})",
            conduit_llvm::symbol(run.func),
            args.join(", ")
        );
        if f.results.is_empty() {
            s.push_str(&format!("  {call}\n"));
        } else {
            s.push_str(&format!("  %r{i} = {call}\n"));
        }
        for (j, t) in f.results.iter().enumerate() {
            let v = if f.results.len() == 1 {
                format!("%r{i}")
            } else {
                s.push_str(&format!("  %x{i}_{j} = extractvalue {ret} %r{i}, {j}\n"));
                format!("%x{i}_{j}")
            };
            let tt = conduit_llvm::ty(t)?;
            let bits = match t {
                Ty::I64 => v,
                Ty::F64 => {
                    s.push_str(&format!("  %b{i}_{j} = bitcast double {v} to i64\n"));
                    format!("%b{i}_{j}")
                }
                Ty::F32 => {
                    s.push_str(&format!("  %c{i}_{j} = bitcast float {v} to i32\n"));
                    s.push_str(&format!("  %b{i}_{j} = zext i32 %c{i}_{j} to i64\n"));
                    format!("%b{i}_{j}")
                }
                _ => {
                    s.push_str(&format!("  %b{i}_{j} = zext {tt} {v} to i64\n"));
                    format!("%b{i}_{j}")
                }
            };
            s.push_str(&format!(
                "  call i32 (ptr, ...) @printf(ptr @.fmt, i64 {bits})\n"
            ));
        }
        s.push_str("  br label %done\n");
    }
    s.push_str("done:\n  ret i32 0\n}\n");
    Ok(s)
}

/// The results the driver printed, as literals of their types.
fn parse(out: &str, results: &[Ty]) -> Result<Vec<Lit>, String> {
    let words: Vec<&str> = out.split_whitespace().collect();
    if words.len() != results.len() {
        return Err(format!("printed {out:?} for {} results", results.len()));
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
