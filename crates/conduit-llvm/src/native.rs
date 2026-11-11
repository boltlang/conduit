//! Running a module natively, for tests and benchmarks: functions that call
//! given functions of the module with given arguments, compiled with
//! `clang` into an executable with the C files that provide its externs.
//!
//! - [`print_runs`] makes `conduit_print_run(k)`, which calls the `k`th of
//!   the calls and prints the bits of each result in hex, then, with
//!   counts, how many objects are left live.
//! - [`bench_runs`] makes `conduit_bench_run(k)`, which makes the same
//!   call and gives back the bits of its results, all xor-ed together, so
//!   that the C code that times it can keep them, and LLVM can't leave the
//!   call out.
//! - [`TIMER`] is a C `main` for a benchmark, which times `conduit_bench_run`.

use std::path::{Path, PathBuf};
use std::process::Command;

use conduit_ir::{FuncId, Lit, Module, Ty};

use crate::ir::{Call, CallConv, CastOp, Declare, FnBuilder, Global, Item, Label, Type, Value};

/// A call of function `func` with `args`.
#[derive(Debug, Clone, Copy)]
pub struct Run<'a> {
    pub func: FuncId,
    pub args: &'a [Lit],
}

/// Whether `clang` can be run.
pub fn clang() -> bool {
    Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Compiles `module`, LLVM IR text, with `c_files`, into the executable
/// `dir/stem`, with `-O2`, keeping the IR as `dir/stem.ll`.
pub fn build(dir: &Path, stem: &str, module: &str, c_files: &[&Path]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let ll = dir.join(format!("{stem}.ll"));
    let exe = dir.join(stem);
    std::fs::write(&ll, module).map_err(|e| e.to_string())?;
    let out = Command::new("clang")
        .args(["-O2", "-Wno-override-module", "-o"])
        .arg(&exe)
        .arg(&ll)
        .args(c_files)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "clang failed on {}:\n{}",
            ll.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(exe)
}

/// `conduit_print_run(i32 k)`, which makes the `k`th of `runs`, and
/// prints the bits of each result in hex, each followed by a space; then,
/// if `counts`, the number of objects left live, once cycles are collected,
/// and the number of host values the test host holds, from its
/// `conduit_test_hosts_live`.
pub fn print_runs(m: &Module, runs: &[Run<'_>], counts: bool) -> Result<Vec<Item>, String> {
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
    let params = vec![(Type::I32, "k".to_string())];
    let mut b = FnBuilder::new(CallConv::C, Type::Void, "conduit_print_run", params);
    let k = b.param(0);
    dispatch(&mut b, m, runs, &k, |b, bits| {
        for v in bits {
            printf(b, v);
        }
        if counts {
            b.call(Call::c(
                Type::Void,
                Value::global("conduit_collect"),
                vec![],
            ));
            let live = b.load(Type::I64, Value::global("conduit_live"));
            printf(b, live);
            let hosts = Call::c(Type::I64, Value::global("conduit_test_hosts_live"), vec![]);
            let hosts = b.call_value(hosts);
            printf(b, hosts);
        }
        b.ret_void();
    })?;
    let mut items = vec![
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
    ];
    if counts {
        items.push(Declare::new(Type::I64, "conduit_test_hosts_live", vec![]).into());
    }
    items.push(b.finish().into());
    Ok(items)
}

/// `i64 conduit_bench_run(i32 k)`, which makes the `k`th of `runs`, and
/// gives back the bits of its results, xor-ed together.
pub fn bench_runs(m: &Module, runs: &[Run<'_>]) -> Result<Vec<Item>, String> {
    let params = vec![(Type::I32, "k".to_string())];
    let mut b = FnBuilder::new(CallConv::C, Type::I64, "conduit_bench_run", params);
    let k = b.param(0);
    dispatch(&mut b, m, runs, &k, |b, bits| {
        let mut all = Value::Int(0);
        for v in bits {
            all = b.bin(crate::ir::BinOp::Xor, Type::I64, all, v);
        }
        b.ret(Some(Type::I64.val(all)));
    })?;
    Ok(vec![b.finish().into()])
}

/// Branches on `k` to a block for each of `runs`, which makes its call and
/// gives `then` the bits of each result, as an `i64`, to end the block
/// with. Any other `k` returns, or traps if the function gives something.
fn dispatch(
    b: &mut FnBuilder,
    m: &Module,
    runs: &[Run<'_>],
    k: &Value,
    mut then: impl FnMut(&mut FnBuilder, Vec<Value>),
) -> Result<(), String> {
    let labels: Vec<Label> = runs.iter().map(|_| b.new_block()).collect();
    let other = b.new_block();
    let cases = labels.iter().cloned().enumerate();
    b.switch(
        Type::I32.val(k),
        &other,
        cases.map(|(i, l)| (i as i64, l)).collect(),
    );
    for (run, label) in runs.iter().zip(&labels) {
        b.start(label);
        let f = &m.funcs[&run.func];
        let ret = crate::ret_ty(&f.results)?;
        let args = (f.params.iter().zip(run.args))
            .map(|(v, a)| Ok(crate::ty(f.var_ty(*v))?.val(crate::lit(a))))
            .collect::<Result<_, String>>()?;
        let r = b.call(Call::tailcc(ret.clone(), crate::symbol(run.func), args));
        let mut bits = Vec::with_capacity(f.results.len());
        for (j, t) in f.results.iter().enumerate() {
            let r = r.clone().expect("a call with results");
            let v = if f.results.len() == 1 {
                r
            } else {
                b.extract_value(ret.clone(), r, j as u32)
            };
            bits.push(match t {
                Ty::I64 => v,
                Ty::F64 => b.cast(CastOp::Bitcast, Type::Double, v, Type::I64),
                Ty::F32 => {
                    let c = b.cast(CastOp::Bitcast, Type::Float, v, Type::I32);
                    b.cast(CastOp::ZExt, Type::I32, c, Type::I64)
                }
                _ => b.cast(CastOp::ZExt, crate::ty(t)?, v, Type::I64),
            });
        }
        then(b, bits);
    }
    b.start(&other);
    b.unreachable();
    Ok(())
}

/// The results [`print_runs`] printed, as literals of their types.
pub fn parse_results(words: &[&str], results: &[Ty]) -> Result<Vec<Lit>, String> {
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

/// A C `main` for a benchmark, linked with a module that has
/// [`print_runs`] and [`bench_runs`]:
///
/// - `exe check K` prints what call `K` gives, with `conduit_print_run`;
/// - `exe time K N` times `N` calls of `conduit_bench_run(K)`, and prints
///   each one's nanoseconds on a line.
///
/// Each timed call runs in a process of its own, forked, so that each
/// starts from the same memory, as a fresh Wasm instance does: what one
/// leaves behind, when nothing is freed, doesn't slow down the next. The
/// process makes the call once before the one it times, so that the
/// pages that call writes are its own already: the first write to a page
/// after a fork copies it, which would otherwise take several
/// microseconds of every call, however little it does. The fork isn't
/// timed. On Linux, it stays on one core, as do the processes it forks,
/// so that each starts with the caches the one before warmed.
pub const TIMER: &str = r#"
#ifdef __linux__
#define _GNU_SOURCE
#include <sched.h>
#endif
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

void conduit_print_run(int32_t k);
int64_t conduit_bench_run(int32_t k);

static volatile int64_t sink;

static int64_t now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (int64_t)t.tv_sec * 1000000000 + t.tv_nsec;
}

int main(int argc, char **argv) {
  if (argc == 3 && strcmp(argv[1], "check") == 0) {
    conduit_print_run(atoi(argv[2]));
    return 0;
  }
  if (argc != 4 || strcmp(argv[1], "time") != 0) {
    fprintf(stderr, "usage: %s check K | time K N\n", argv[0]);
    return 2;
  }
  int32_t k = atoi(argv[2]);
  int n = atoi(argv[3]);
#ifdef __linux__
  // On the core it's on, which each process it forks inherits, so that
  // each starts with that core's caches, not another's.
  cpu_set_t one;
  CPU_ZERO(&one);
  CPU_SET(sched_getcpu(), &one);
  sched_setaffinity(0, sizeof one, &one);
#endif
  for (int i = 0; i < n; i++) {
    int fd[2];
    if (pipe(fd) != 0) return 1;
    pid_t pid = fork();
    if (pid < 0) return 1;
    if (pid == 0) {
      close(fd[0]);
      sink = conduit_bench_run(k);
      int64_t start = now();
      sink = conduit_bench_run(k);
      int64_t took = now() - start;
      _exit(write(fd[1], &took, sizeof took) == sizeof took ? 0 : 1);
    }
    close(fd[1]);
    int64_t took;
    ssize_t got = read(fd[0], &took, sizeof took);
    close(fd[0]);
    int status;
    waitpid(pid, &status, 0);
    if (got != sizeof took || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 1;
    printf("%lld\n", (long long)took);
  }
  return 0;
}
"#;
