//! Runs the programs in `tests/programs` that have a `bench` check, as Wasm
//! in Wasmtime and, with `--backend`, as Wasm with stack switching or as
//! native code (see [`conduit_bench::Backend`]), and measures each of their
//! `run` checks, compiled each of the ways the `bench` snapshots compare
//! ([`conduit_bench::CONFIGS`]):
//!
//! - **fuel**: what Wasmtime counts as it runs the code, about one per Wasm
//!   instruction. It's the same every time, so it can be compared exactly.
//! - **time**: the median wall-clock time of the call, with how far the
//!   runs spread around it, over enough runs to fill `--time`. The module
//!   is compiled once, and each run gets a fresh instance, outside the
//!   timing. Times are only worth comparing on a quiet machine, with
//!   `--release`. Native code is timed by `conduit_llvm::native::TIMER`,
//!   each call in a forked process, which a call before it warms up, a
//!   batch of calls each time the configs take turns.
//! - **code**: the size of the module, in bytes, and **pages**: how much
//!   memory it has after the call, in 64 KiB pages. Native code has
//!   neither, and no fuel.
//!
//! Each call's results must be what its `run` check says.
//!
//! ```sh
//! cargo run --release -p conduit-bench -- [options] [filter]
//! ```
//!
//! - `filter`: only programs whose name contains it, like `nqueens`.
//! - `--config NAME,..`: only those ways of compiling: `optimized`,
//!   `counts`, `+borrows`, `+reuse`, `+specialized`, `+cycles`.
//! - `--backend NAME,..`: the backends to run on: `wasm`, the default,
//!   `wasm-switch` and `native`. Results show each backend's time next to
//!   Wasm's, as a ratio. And `c` and `rust`, which aren't Conduit: a
//!   program written by hand in C or Rust next to the benchmark, as
//!   `<name>.c` or `<name>.rs`, with the same runs, as the floor to compare
//!   with, compiled with `clang -O2` or `rustc -C opt-level=3` and timed as
//!   native code is, in a row `hand-written`.
//! - `--fuel`: no times, which is quick and the same every time, and only
//!   Wasm, which counts fuel.
//! - `--time MS`: how long to time each call for, 300 by default.
//! - `--json FILE`: also write the results to `FILE`, with the commit.
//! - `--compare FILE`: compare with results `--json` wrote, and show what
//!   changed: any change in fuel, and times more than 5% and more than
//!   their spread apart.
//! - `--markdown`: print a Markdown table instead, of only the fuel that
//!   changed with `--compare`, as CI shows in a pull request's summary.
//! - `--check PCT`: with `--compare`, fail if any fuel went up by more
//!   than `PCT` percent, which CI does for a pull request.
//! - `--badges DIR`: write badges for the README to `DIR`, in the JSON
//!   that shields.io's endpoint badges read, which CI publishes for each
//!   push to `main`. They summarize the benchmarks of data structures in
//!   `bench/`, each by its largest run: what counting costs in fuel, with
//!   everything on, over no counts, how many fewer objects reuse
//!   allocates, both as geometric means over the benchmarks, and how many
//!   benchmarks there are (see `badges`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use conduit_bench::{Backend, CONFIGS, Config, prepare, prepare_for};
use conduit_ir::{Atom, Module, Ty};
use conduit_llvm::native;
use conduit_test::{CheckKind, Program, Target};
use conduit_wasm::Options;
use serde_json::{Value as Json, json};
use wasmtime::{Engine, Store};

struct Args {
    filter: Option<String>,
    configs: Vec<String>,
    backends: Vec<Backend>,
    fuel_only: bool,
    time: Duration,
    json: Option<PathBuf>,
    compare: Option<PathBuf>,
    markdown: bool,
    check: Option<f64>,
    badges: Option<PathBuf>,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        filter: None,
        configs: CONFIGS.iter().map(|c| c.name.to_string()).collect(),
        backends: vec![Backend::Wasm],
        fuel_only: false,
        time: Duration::from_millis(300),
        json: None,
        compare: None,
        markdown: false,
        check: None,
        badges: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--config" => {
                args.configs = value("--config")?.split(',').map(String::from).collect();
                if let Some(c) = args.configs.iter().find(|c| Config::named(c).is_none()) {
                    let names: Vec<&str> = CONFIGS.iter().map(|c| c.name).collect();
                    return Err(format!("unknown config {c}: expected one of {names:?}"));
                }
            }
            "--backend" => {
                let names = value("--backend")?;
                args.backends = (names.split(','))
                    .map(|b| {
                        Backend::named(b).ok_or_else(|| {
                            let names: Vec<&str> = Backend::ALL.iter().map(|b| b.name()).collect();
                            format!("unknown backend {b}: expected one of {names:?}")
                        })
                    })
                    .collect::<Result<_, _>>()?;
                args.backends.sort();
                args.backends.dedup();
            }
            "--fuel" => args.fuel_only = true,
            "--time" => {
                let ms = value("--time")?
                    .parse()
                    .map_err(|e| format!("--time: {e}"))?;
                args.time = Duration::from_millis(ms);
            }
            "--json" => args.json = Some(value("--json")?.into()),
            "--compare" => args.compare = Some(value("--compare")?.into()),
            "--markdown" => args.markdown = true,
            "--badges" => args.badges = Some(value("--badges")?.into()),
            "--check" => {
                let pct = value("--check")?
                    .parse()
                    .map_err(|e| format!("--check: {e}"))?;
                args.check = Some(pct);
            }
            "--help" | "-h" => {
                return Err(
                    "usage: conduit-bench [--config NAME,..] [--backend NAME,..] [--fuel] [--time MS] \
                            [--json FILE] [--compare FILE] [--markdown] [--check PCT] [--badges DIR] [filter]"
                        .into(),
                );
            }
            _ if a.starts_with("--") => return Err(format!("unknown option {a}")),
            _ => args.filter = Some(a),
        }
    }
    if args.check.is_some() && args.compare.is_none() {
        return Err("--check needs --compare".into());
    }
    Ok(args)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// What one `run` check of a program did, compiled one way, for one
/// backend. Native code has no fuel, and its size and memory aren't
/// measured.
struct Measured {
    program: String,
    run: String,
    config: String,
    backend: Backend,
    fuel: Option<u64>,
    /// The median and the median distance from it, if timed.
    time: Option<(Duration, Duration)>,
    code: Option<usize>,
    pages: Option<u64>,
}

impl Measured {
    fn key(&self) -> Key {
        (
            self.program.clone(),
            self.run.clone(),
            self.config.clone(),
            self.backend.name().to_string(),
        )
    }
}

/// A measurement's program, run, config and backend.
type Key = (String, String, String, String);

fn run() -> Result<(), String> {
    let args = args()?;
    let baseline = match &args.compare {
        Some(path) => Some(load(path)?),
        None => None,
    };
    let mut results = Vec::new();
    let mut benched = Vec::new();
    for p in conduit_test::programs() {
        let p = p.map_err(|(name, e)| format!("{name}: {e}"))?;
        if !p.has(|k| matches!(k, CheckKind::Bench))
            || !p.has_runs()
            || args
                .filter
                .as_ref()
                .is_some_and(|f| !p.name.contains(f.as_str()))
        {
            continue;
        }
        results.extend(bench(&p, &args).map_err(|e| format!("{}: {e}", p.shown()))?);
        benched.push(p);
    }
    if let Some(dir) = &args.badges {
        badges(&results, &benched, dir)?;
    }
    if args.markdown {
        markdown(&results, baseline.as_ref());
    } else {
        print(&results, baseline.as_ref());
    }
    if let Some(path) = &args.json {
        let json = json!({
            "commit": commit(),
            "results": results.iter().map(|r| json!({
                "program": r.program,
                "run": r.run,
                "config": r.config,
                "backend": r.backend.name(),
                "fuel": r.fuel,
                "time_ns": r.time.map(|t| t.0.as_nanos() as u64),
                "spread_ns": r.time.map(|t| t.1.as_nanos() as u64),
                "code": r.code,
                "pages": r.pages,
            })).collect::<Vec<_>>(),
        });
        let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let (Some(pct), Some(baseline)) = (args.check, &baseline) {
        let worse: Vec<String> = (fuel_changes(&results, baseline).into_iter())
            .filter(|(_, before, after)| *after as f64 > *before as f64 * (1.0 + pct / 100.0))
            .map(|(r, before, after)| {
                format!(
                    "{} {} {}: fuel {before} -> {after} ({})",
                    r.program,
                    r.run,
                    r.config,
                    change(before as f64, after as f64)
                )
            })
            .collect();
        if !worse.is_empty() {
            return Err(format!(
                "fuel went up by more than {pct}%:\n{}",
                worse.join("\n")
            ));
        }
    }
    Ok(())
}

/// The results whose fuel differs from `baseline`'s, with the fuel before
/// and after. Results the baseline doesn't have aren't changes.
fn fuel_changes<'r>(results: &'r [Measured], baseline: &Baseline) -> Vec<(&'r Measured, u64, u64)> {
    (results.iter())
        .filter_map(|r| {
            let after = r.fuel?;
            let before = baseline.get(&r.key())?.0?;
            (before != after).then_some((r, before, after))
        })
        .collect()
}

/// The results as a Markdown table: with a baseline, only the fuel that
/// changed, else all of them.
fn markdown(results: &[Measured], baseline: Option<&Baseline>) {
    match baseline {
        Some(baseline) => {
            let changes = fuel_changes(results, baseline);
            println!("### Fuel\n");
            if changes.is_empty() {
                println!("No change in fuel in any benchmark.");
                return;
            }
            println!("| Program | Run | Config | Backend | Before | After | Change |");
            println!("|---|---|---|---|---:|---:|---:|");
            for (r, before, after) in changes {
                println!(
                    "| `{}` | `{}` | `{}` | {} | {before} | {after} | {} |",
                    r.program,
                    r.run,
                    r.config,
                    r.backend.name(),
                    change(before as f64, after as f64)
                );
            }
        }
        None => {
            println!("| Program | Run | Config | Backend | Fuel | Time | Code | Pages |");
            println!("|---|---|---|---|---:|---:|---:|---:|");
            for r in results {
                let time = r.time.map_or("-".into(), |(t, _)| show(t));
                println!(
                    "| `{}` | `{}` | `{}` | {} | {} | {time} | {} | {} |",
                    r.program,
                    r.run,
                    r.config,
                    r.backend.name(),
                    or_dash(r.fuel),
                    or_dash(r.code),
                    or_dash(r.pages)
                );
            }
        }
    }
}

/// The modules of `p` for Wasm, one for each config asked for, with the
/// options to lower it with: with stack switching for
/// [`Backend::WasmSwitch`], for which there are none if `p`'s handlers
/// don't need it.
fn modules(
    p: &Program,
    configs: &[String],
    backend: Backend,
) -> Result<Vec<(String, Module, Options)>, String> {
    let m = p.module_for(Target::Wasm)?;
    let switching = backend == Backend::WasmSwitch;
    if switching && conduit_ir::clause::direct_handlers(&m) {
        return Ok(Vec::new());
    }
    let optimized = prepare_for(m, backend)?;
    let mut out = Vec::new();
    for name in configs {
        let config = Config::named(name).ok_or_else(|| format!("unknown config {name}"))?;
        let mut m = optimized.clone();
        config.apply(&mut m)?;
        let options = Options {
            stack_switching: switching,
            counts: config.counts,
            cycles: config.cycles,
            ..Default::default()
        };
        out.push((name.clone(), m, options));
    }
    Ok(out)
}

/// Every measurement of `p`, of each backend asked for, ordered by run,
/// then config, then backend.
fn bench(p: &Program, args: &Args) -> Result<Vec<Measured>, String> {
    let mut out = Vec::new();
    for &backend in &args.backends {
        let measured = match backend {
            Backend::Wasm | Backend::WasmSwitch => bench_wasm(p, args, backend),
            Backend::Native => bench_native(p, args),
            Backend::C | Backend::Rust => bench_by_hand(p, args, backend),
        };
        out.extend(measured.map_err(|e| format!("{}: {e}", backend.name()))?);
    }
    // The same order as the program's runs and the configs asked for.
    let runs: Vec<String> = (p.checks.iter())
        .filter_map(|c| match &c.kind {
            CheckKind::Run { func, args, .. } => Some(call_text(func, args)),
            _ => None,
        })
        .collect();
    let at = |list: &[String], x: &String| list.iter().position(|y| y == x);
    // What's written by hand, which has no config, after Conduit's.
    let config = |r: &Measured| at(&args.configs, &r.config).unwrap_or(usize::MAX);
    out.sort_by_key(|r| (at(&runs, &r.run), config(r), r.backend));
    Ok(out)
}

/// A run check's call, as results show it: `nqueens(8i32)`.
fn call_text(func: &str, args: &[conduit_ir::Lit]) -> String {
    let args: Vec<String> = args.iter().map(|&l| Atom::Lit(l).to_string()).collect();
    format!("{func}({})", args.join(", "))
}

/// `p` as Wasm in Wasmtime: fuel, pages and results in one run of each
/// config, then times, the configs taking turns.
fn bench_wasm(p: &Program, args: &Args, backend: Backend) -> Result<Vec<Measured>, String> {
    let switching = backend == Backend::WasmSwitch;
    let compiled_modules = modules(p, &args.configs, backend)?;
    if compiled_modules.is_empty() {
        return Ok(Vec::new());
    }
    let fuel_engine = engine(true, switching)?;
    let time_engine = engine(false, switching)?;
    // Each config compiled, once for fuel and once for timing.
    let mut compiled = Vec::new();
    for (config, m, options) in compiled_modules {
        let bytes =
            conduit_wasm::emit_module_with(&m, &options).map_err(|e| format!("{config}: {e}"))?;
        let fuel_module =
            wasmtime::Module::new(&fuel_engine, &bytes).map_err(|e| format!("{config}: {e:?}"))?;
        let time_module = (!args.fuel_only)
            .then(|| wasmtime::Module::new(&time_engine, &bytes))
            .transpose()
            .map_err(|e| format!("{config}: {e:?}"))?;
        compiled.push((config, m, bytes.len(), fuel_module, time_module));
    }
    let first = &compiled[0].1;
    // Passes keep functions' IDs, so the runs are the same in every config.
    let runs = p.runs(first)?;
    let mut out = Vec::new();
    for run in &runs {
        let call = call_text(run.name, run.args);
        let results = first
            .funcs
            .get(&run.func)
            .map_or(&[][..], |f| &f.results[..]);
        // Fuel, pages and the results, in one run of each.
        for (config, _, code, fuel_module, _) in &compiled {
            let mut store = Store::new(&fuel_engine, ());
            store.set_fuel(u64::MAX / 2).map_err(|e| e.to_string())?;
            let instance = instantiate(&fuel_engine, &mut store, fuel_module)?;
            let got =
                conduit_test::wasm::call(&mut store, &instance, first, run.func, run.args, results);
            let fuel = u64::MAX / 2 - store.get_fuel().map_err(|e| e.to_string())?;
            let pages = instance
                .get_memory(&mut store, "memory")
                .map_or(0, |mem| mem.size(&store));
            run.check(p, got).map_err(|e| format!("{config}: {e}"))?;
            out.push(Measured {
                program: p.name.clone(),
                run: call.clone(),
                config: config.clone(),
                backend,
                fuel: Some(fuel),
                time: None,
                code: Some(*code),
                pages: Some(pages),
            });
        }
        if args.fuel_only {
            continue;
        }
        let modules: Vec<&wasmtime::Module> = (compiled.iter())
            .filter_map(|(.., time_module)| time_module.as_ref())
            .collect();
        let times = time_calls(&time_engine, &modules, first, run, results, args.time)?;
        let n = out.len() - times.len();
        for (r, t) in out[n..].iter_mut().zip(times) {
            r.time = Some(t);
        }
    }
    Ok(out)
}

/// How many timed calls a native executable makes each time it's run.
const NATIVE_BATCH: usize = 10;

/// `p` as native code, from the LLVM backend: each config compiled with
/// `clang` and `conduit_llvm::native`'s timer, its results checked, then
/// timed, the configs taking turns a batch of calls at a time. Nothing,
/// with a note, without `clang`, or with `--fuel`, since native code has
/// no fuel, or for a program the LLVM backend can't lower yet.
fn bench_native(p: &Program, args: &Args) -> Result<Vec<Measured>, String> {
    if args.fuel_only {
        return Ok(Vec::new());
    }
    if !native::clang() {
        eprintln!("{}: native: skipped, since clang isn't installed", p.name);
        return Ok(Vec::new());
    }
    let base = prepare_for(p.module_for(Target::C)?, Backend::Native)?;
    let runs = p.runs(&base)?;
    let calls: Vec<native::Run<'_>> = (runs.iter())
        .map(|r| native::Run {
            func: r.func,
            args: r.args,
        })
        .collect();
    let dir = conduit_test::programs_dir().join("../../target/conduit-bench/native");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let timer = dir.join("timer.c");
    std::fs::write(&timer, native::TIMER).map_err(|e| e.to_string())?;
    let host = conduit_test::programs_dir().join("host/c.c");
    let mut exes = Vec::new();
    for name in &args.configs {
        let config = Config::named(name).ok_or_else(|| format!("unknown config {name}"))?;
        let mut m = base.clone();
        config.apply(&mut m)?;
        let options = conduit_llvm::Options {
            counts: config.counts,
            cycles: config.cycles,
        };
        let mut module = match conduit_llvm::lower_module_with(&m, &options) {
            Ok(module) => module,
            Err(e) => {
                eprintln!("{}: native: skipped: {e}", p.name);
                return Ok(Vec::new());
            }
        };
        module.items.extend(native::print_runs(&m, &calls, false)?);
        module.items.extend(native::bench_runs(&m, &calls)?);
        let stem = format!("{}_{}", p.name.replace('/', "_"), name.replace('+', ""));
        let exe = native::build(&dir, &stem, &module.to_string(), &[&timer, &host])?;
        exes.push((name.clone(), exe));
    }
    let mut out = Vec::new();
    for (k, run) in runs.iter().enumerate() {
        let results = &base.funcs[&run.func].results;
        for (config, exe) in &exes {
            let got = native_check(exe, k, results);
            run.check(p, got).map_err(|e| format!("{config}: {e}"))?;
        }
        let times = time_native(&exes, k, args.time)?;
        for ((config, _), time) in exes.iter().zip(times) {
            out.push(Measured {
                program: p.name.clone(),
                run: call_text(run.name, run.args),
                config: config.clone(),
                backend: Backend::Native,
                fuel: None,
                time: Some(time),
                code: None,
                pages: None,
            });
        }
    }
    Ok(out)
}

/// The config of what's written by hand, which isn't compiled by Conduit.
const BY_HAND: &str = "hand-written";

/// `p` written by hand, in C or in Rust, next to it as `<name>.c` or
/// `<name>.rs`, with `conduit_print_run(k)` and `conduit_bench_run(k)` for
/// its runs, in order, as `conduit_llvm::native` makes them: compiled,
/// checked and timed as native code is. A program with no such file is
/// skipped.
fn bench_by_hand(p: &Program, args: &Args, backend: Backend) -> Result<Vec<Measured>, String> {
    if args.fuel_only || !native::clang() {
        return Ok(Vec::new());
    }
    let ext = if backend == Backend::C { "c" } else { "rs" };
    let source = conduit_test::programs_dir().join(format!("{}.{ext}", p.name));
    if !source.exists() {
        return Ok(Vec::new());
    }
    let dir = conduit_test::programs_dir().join("../../target/conduit-bench/native");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let timer = dir.join("timer.c");
    std::fs::write(&timer, native::TIMER).map_err(|e| e.to_string())?;
    let stem = format!("{}_{}", p.name.replace('/', "_"), backend.name());
    let exe = dir.join(&stem);
    let run = |cmd: &mut std::process::Command| -> Result<(), String> {
        let out = cmd.output().map_err(|e| format!("{cmd:?}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{cmd:?}:\n{}",
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    };
    let mut clang = std::process::Command::new("clang");
    clang.args(["-O2", "-o"]).arg(&exe).arg(&timer);
    if backend == Backend::C {
        clang.arg(&source);
    } else {
        let lib = dir.join(format!("lib{stem}.a"));
        run(std::process::Command::new("rustc")
            .args(["--edition", "2024", "--crate-type", "staticlib"])
            .args(["-C", "opt-level=3", "-C", "panic=abort", "-o"])
            .arg(&lib)
            .arg(&source))?;
        // What Rust's standard library needs, as `rustc --print
        // native-static-libs` says on Linux.
        clang
            .arg(&lib)
            .args(["-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl"]);
    }
    run(&mut clang)?;
    let base = prepare_for(p.module_for(Target::C)?, backend)?;
    let runs = p.runs(&base)?;
    let exes = vec![(BY_HAND.to_string(), exe)];
    let mut out = Vec::new();
    for (k, r) in runs.iter().enumerate() {
        let results = &base.funcs[&r.func].results;
        r.check(p, native_check(&exes[0].1, k, results))?;
        let times = time_native(&exes, k, args.time)?;
        out.push(Measured {
            program: p.name.clone(),
            run: call_text(r.name, r.args),
            config: BY_HAND.to_string(),
            backend,
            fuel: None,
            time: Some(times[0]),
            code: None,
            pages: None,
        });
    }
    Ok(out)
}

/// What call `k` of the executable `exe` gives, as its `check` prints it.
fn native_check(exe: &Path, k: usize, results: &[Ty]) -> Result<conduit_test::Outcome, String> {
    use std::os::unix::process::ExitStatusExt;
    let out = std::process::Command::new(exe)
        .args(["check", &k.to_string()])
        .output()
        .map_err(|e| e.to_string())?;
    if let Some(signal) = out.status.signal() {
        return Ok(conduit_test::Outcome::Trap(format!("signal {signal}")));
    }
    if !out.status.success() {
        return Err(format!("exited with {}", out.status));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let words: Vec<&str> = text.split_whitespace().collect();
    native::parse_results(&words, results).map(conduit_test::Outcome::Values)
}

/// For each of `exes`, the median time of call `k`, and the median
/// distance from it, as [`time_calls`] does for Wasm, a batch of calls of
/// each executable a round.
fn time_native(
    exes: &[(String, PathBuf)],
    k: usize,
    budget: Duration,
) -> Result<Vec<(Duration, Duration)>, String> {
    let batch = |exe: &Path| -> Result<Vec<Duration>, String> {
        let out = std::process::Command::new(exe)
            .args(["time", &k.to_string(), &NATIVE_BATCH.to_string()])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("timing {} failed: {}", exe.display(), out.status));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        (text.lines())
            .map(|l| {
                l.trim()
                    .parse()
                    .map(Duration::from_nanos)
                    .map_err(|e| format!("{e}"))
            })
            .collect()
    };
    let mut times = vec![Vec::new(); exes.len()];
    let start = Instant::now();
    let total = budget * exes.len() as u32;
    while times[0].len() < 5 || (start.elapsed() < total && times[0].len() < 100_000) {
        for ((_, exe), ts) in exes.iter().zip(&mut times) {
            ts.extend(batch(exe)?);
        }
    }
    Ok(times.into_iter().map(median_and_spread).collect())
}

/// The median of `ts`, and the median distance from it.
fn median_and_spread(mut ts: Vec<Duration>) -> (Duration, Duration) {
    ts.sort();
    let median = ts[ts.len() / 2];
    let mut spread: Vec<Duration> = ts.iter().map(|&t| t.abs_diff(median)).collect();
    spread.sort();
    (median, spread[spread.len() / 2])
}

/// For each of `modules`, the median time of calling `run`, and the median
/// distance from it. The modules take turns, a call each per round, so
/// that the machine changing speed over time changes them all alike, over
/// at least 5 rounds and as many as fill `budget` for each. A call of each
/// first warms it up.
fn time_calls(
    engine: &Engine,
    modules: &[&wasmtime::Module],
    m: &Module,
    run: &conduit_test::Run<'_>,
    results: &[Ty],
    budget: Duration,
) -> Result<Vec<(Duration, Duration)>, String> {
    let once = |module: &wasmtime::Module| -> Result<Duration, String> {
        let mut store = Store::new(engine, ());
        let instance = instantiate(engine, &mut store, module)?;
        let t = Instant::now();
        let got = conduit_test::wasm::call(&mut store, &instance, m, run.func, run.args, results);
        let elapsed = t.elapsed();
        got?;
        Ok(elapsed)
    };
    for m in modules {
        once(m)?;
    }
    let mut times = vec![Vec::new(); modules.len()];
    let start = Instant::now();
    let total = budget * modules.len() as u32;
    while times[0].len() < 5 || (start.elapsed() < total && times[0].len() < 100_000) {
        for (m, ts) in modules.iter().zip(&mut times) {
            ts.push(once(m)?);
        }
    }
    Ok(times.into_iter().map(median_and_spread).collect())
}

fn engine(fuel: bool, stack_switching: bool) -> Result<Engine, String> {
    let mut config = conduit_test::wasm::config();
    config
        .consume_fuel(fuel)
        .wasm_gc(stack_switching)
        .wasm_stack_switching(stack_switching)
        // Instantiating from a pool is quicker, which leaves more of the
        // time for runs. Stack switching has a table for each operation's
        // continuations, which needs a slot for each one alive, and host
        // values one more.
        .allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling({
            let mut pool = wasmtime::PoolingAllocationConfig::default();
            pool.max_tables_per_module(64)
                .total_tables(64 * 1000)
                .table_elements(1 << 20);
            pool
        }));
    Engine::new(&config).map_err(|e| e.to_string())
}

fn instantiate(
    engine: &Engine,
    store: &mut Store<()>,
    module: &wasmtime::Module,
) -> Result<wasmtime::Instance, String> {
    let linker = conduit_test::wasm::linker(engine)?;
    linker.instantiate(store, module).map_err(|e| e.to_string())
}

/// A baseline's measurements, by program, run and config: fuel, and the
/// time and spread in nanoseconds, if timed.
type Baseline = BTreeMap<Key, (Option<u64>, Option<(u64, u64)>)>;

fn load(path: &PathBuf) -> Result<Baseline, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json: Json = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Baseline::new();
    for r in json["results"]
        .as_array()
        .ok_or("no results in the baseline")?
    {
        let s = |k: &str| r[k].as_str().unwrap_or_default().to_string();
        let time = r["time_ns"].as_u64().zip(r["spread_ns"].as_u64());
        // Results from before there were other backends are Wasm's.
        let backend = r["backend"].as_str().unwrap_or("wasm").to_string();
        out.insert(
            (s("program"), s("run"), s("config"), backend),
            (r["fuel"].as_u64(), time),
        );
    }
    Ok(out)
}

fn print(results: &[Measured], baseline: Option<&Baseline>) {
    let mut last = (String::new(), String::new());
    let backends = results.iter().any(|r| r.backend != Backend::Wasm);
    for r in results {
        if (&r.program, &r.run) != (&last.0, &last.1) {
            println!("\n{} {}", r.program, r.run);
            let mut head = format!(
                "  {:<14}{:<13}{:>14}{:>12}{:>10}{:>9}{:>7}",
                "", "", "fuel", "time", "±", "code", "pages"
            );
            if backends {
                head.push_str(&format!("{:>10}", "vs wasm"));
            }
            println!("{head}");
            last = (r.program.clone(), r.run.clone());
        }
        let (time, spread) = match r.time {
            Some((t, s)) => (show(t), show(s)),
            None => ("-".into(), "-".into()),
        };
        let mut line = format!(
            "  {:<14}{:<13}{:>14}{:>12}{:>10}{:>9}{:>7}",
            r.config,
            r.backend.name(),
            or_dash(r.fuel),
            time,
            spread,
            or_dash(r.code),
            or_dash(r.pages)
        );
        // How the time compares with Wasm's, for the same config.
        if backends {
            let wasm = (results.iter()).find(|w| {
                w.backend == Backend::Wasm
                    && (&w.program, &w.run, &w.config) == (&r.program, &r.run, &r.config)
            });
            let ratio = match (r.backend, r.time, wasm.and_then(|w| w.time)) {
                (Backend::Wasm, ..) => String::new(),
                (_, Some((t, _)), Some((w, _))) => {
                    format!("{:.2}", t.as_secs_f64() / w.as_secs_f64())
                }
                _ => "-".into(),
            };
            line.push_str(&format!("{ratio:>10}"));
        }
        if let Some(&(fuel, base_time)) = baseline.and_then(|b| b.get(&r.key())) {
            if let (Some(before), Some(after)) = (fuel, r.fuel)
                && before != after
            {
                line.push_str(&format!("  fuel {}", change(before as f64, after as f64)));
            }
            if let (Some((t, s)), Some((bt, bs))) = (r.time, base_time) {
                let (t, bt) = (t.as_nanos() as f64, bt as f64);
                let noise = (s.as_nanos() as f64).max(bs as f64);
                if (t - bt).abs() > 0.05 * bt && (t - bt).abs() > noise {
                    line.push_str(&format!("  time {}", change(bt, t)));
                }
            }
        }
        println!("{line}");
    }
}

/// A number, or `-` for none.
fn or_dash(n: Option<impl ToString>) -> String {
    n.map_or("-".into(), |n| n.to_string())
}

fn change(before: f64, after: f64) -> String {
    format!("{:+.1}%", (after - before) / before * 100.0)
}

fn show(d: Duration) -> String {
    let ns = d.as_nanos();
    if ns >= 1_000_000 {
        format!("{:.2}ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.1}µs", ns as f64 / 1e3)
    } else {
        format!("{ns}ns")
    }
}

/// The commit the workspace is at, if git knows it, with `+dirty` if it
/// has changes that aren't committed.
fn commit() -> Option<String> {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let commit = git(&["rev-parse", "--short", "HEAD"])?;
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    Some(if dirty { commit + "+dirty" } else { commit })
}

/// The programs the badges summarize: the benchmarks of data structures,
/// in the style of the Perceus paper's, which is what counting and reuse
/// are about. The `bench/effects_*` ones compare ways of lowering
/// handlers, and would only dilute both: they have nothing to reuse, and
/// what counting costs there is mostly evidence passing's.
const BADGE_PROGRAMS: &[&str] = &[
    "bench/binarytrees",
    "bench/bstinsert",
    "bench/cfold",
    "bench/nqueens",
];

/// Writes the README's badges to `dir`, as shields.io endpoint JSON, over
/// [`BADGE_PROGRAMS`], each by its largest run, the one that takes the
/// most fuel without counts, so that a warm-up run doesn't weigh as much
/// as the run that matters, and each benchmark weighs the same:
/// `overhead.json`, the geometric mean of the fuel with everything on
/// (`+cycles`) over the fuel without counts (`optimized`);
/// `reuse.json`, how many fewer objects the evaluator allocates with
/// reuse than with counts alone, as the geometric mean of each one's
/// ratio; and `benchmarks.json`, how many there are.
fn badges(results: &[Measured], programs: &[Program], dir: &Path) -> Result<(), String> {
    // Wasm's fuel, the only measurement that's the same every time.
    let wasm: Vec<&Measured> = (results.iter())
        .filter(|r| r.backend == Backend::Wasm)
        .collect();
    let fuel = |program: &str, run: &str, config: &str| {
        (wasm.iter())
            .find(|r| r.program == program && r.run == run && r.config == config)
            .and_then(|r| r.fuel)
            .map(|f| f as f64)
    };
    let geomean =
        |ratios: &[f64]| (ratios.iter().map(|r| r.ln()).sum::<f64>() / ratios.len() as f64).exp();
    let (mut overheads, mut kept) = (Vec::new(), Vec::new());
    for p in programs
        .iter()
        .filter(|p| BADGE_PROGRAMS.contains(&p.name.as_str()))
    {
        // Its largest run.
        let largest = (wasm.iter())
            .filter(|r| r.program == p.name && r.config == "optimized")
            .max_by_key(|r| r.fuel)
            .ok_or("the badges need the optimized config, as Wasm")?;
        let (Some(counted), Some(plain)) = (fuel(&p.name, &largest.run, "+cycles"), largest.fuel)
        else {
            return Err("the badges need the optimized and +cycles configs, as Wasm".into());
        };
        overheads.push(counted / plain as f64);

        // Its allocations, in the evaluator, which counts them.
        let base = prepare(p.module()?)?;
        let runs = p.runs(&base)?;
        let run = (runs.iter())
            .find(|r| call_text(r.name, r.args) == largest.run)
            .ok_or_else(|| format!("{}: no run {}", p.name, largest.run))?;
        let mut allocs = Vec::new();
        for name in ["counts", "+reuse"] {
            let mut m = base.clone();
            Config::named(name).unwrap().apply(&mut m)?;
            let mut machine = conduit_eval::Machine::new(&m, conduit_test::eval::TestHost);
            machine.counting = true;
            let args = run
                .args
                .iter()
                .map(|&l| conduit_eval::Value::from(l))
                .collect();
            machine.call(run.func, args).map_err(|e| format!("{e:?}"))?;
            allocs.push(machine.stats.allocs as f64);
        }
        if allocs[0] > 0.0 {
            kept.push(allocs[1].max(1.0) / allocs[0]);
        }
    }
    let count = overheads.len();
    if count == 0 {
        return Err(format!("none of {BADGE_PROGRAMS:?} to make badges of"));
    }
    let overhead = geomean(&overheads) - 1.0;
    let saved = if kept.is_empty() {
        0.0
    } else {
        1.0 - geomean(&kept)
    };

    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let write = |name: &str, label: &str, message: String, color: &str| {
        let badge = json!({
            "schemaVersion": 1,
            "label": label,
            "message": message,
            "color": color,
        });
        let path = dir.join(name);
        std::fs::write(&path, badge.to_string() + "\n")
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    let pct = overhead * 100.0;
    let color = match pct {
        p if p < 20.0 => "brightgreen",
        p if p < 50.0 => "yellow",
        _ => "red",
    };
    write(
        "overhead.json",
        "counting overhead",
        format!("{pct:+.0}% fuel"),
        color,
    )?;
    let color = match saved * 100.0 {
        s if s >= 50.0 => "brightgreen",
        s if s >= 20.0 => "yellow",
        _ => "lightgrey",
    };
    let message = format!("{:.0}% fewer allocations", saved * 100.0);
    write("reuse.json", "reuse", message, color)?;
    write(
        "benchmarks.json",
        "benchmarks",
        format!("{count} programs"),
        "blue",
    )?;
    Ok(())
}
